//! Post-session re-alignment: the full recording against the whole script.
//!
//! The live tracker has to decide where the cursor goes from a few words at a
//! time, several times a second. After the session there is no such pressure:
//! the whole recording has been transcribed, so we can align ALL of it against
//! ALL of the script in one global pass and ask, sentence by sentence, "was this
//! actually said?". This is the audiobook-proofing / Whispersync pattern
//! (manuscript vs. finished audio), and it is what the compliance report should
//! rest on. The live tracker only drives the scroll.
//!
//! The alignment is a global, monotonic dynamic program over words
//! (Needleman-Wunsch): matching a transcript word to a script word earns its
//! similarity, a script word with no partner is an omission, and a transcript
//! word with no partner is an insertion (patient speech, small talk, ad-libs).
//! Insertions are cheap because a consult is full of them; omissions cost more.

use crate::align::{normalize, word_sim};
use std::collections::HashMap;

/// Per-word match bar (same as the live aligner).
const WORD_MATCH: f32 = 0.7;
/// Cost of pairing two unrelated words.
const MISMATCH: f32 = -1.0;
/// Cost of a transcript word with no script partner (patient talk, ad-libs).
const INSERTION: f32 = -0.2;
/// Cost of a script word that was never said.
const OMISSION: f32 = -0.4;
/// A sentence counts as delivered when at least this share of its words matched.
const SENTENCE_COVERED: f32 = 0.5;

// Bound both the traceback allocation and the expensive fuzzy vocabulary
// comparison pass before allocating either matrix. Live evidence is retained
// when a recording is too large for this exact, quadratic verifier.
const MAX_ALIGNMENT_CELLS: usize = 25_000_000;
const MAX_VOCABULARY_PAIRS: usize = 1_000_000;
const MAX_VOCABULARY_WORK: usize = 128_000_000;
const MAX_INPUT_BYTES: usize = 4 * 1024 * 1024;
const MAX_WORD_BYTES: usize = 256;

/// A recording exceeds the bounded exact verifier's resource budget.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("recording verification exceeded the {limit} limit")]
pub struct RealignmentError {
    pub limit: &'static str,
}

/// Result of aligning a full transcript against the script's main sentences.
#[derive(Debug, Clone, PartialEq)]
pub struct Realignment {
    /// For each main sentence: share of its words matched (0.0-1.0).
    pub sentence_match: Vec<f32>,
    /// For each main sentence: whether it counts as delivered.
    pub covered: Vec<bool>,
    /// Transcript words that matched a script word.
    pub matched_words: usize,
    /// Transcript words that matched nothing (off-script speech).
    pub inserted_words: usize,
    /// Total transcript words aligned.
    pub transcript_words: usize,
}

impl Realignment {
    /// Indices of sentences that were not delivered.
    pub fn omitted(&self) -> Vec<usize> {
        self.covered
            .iter()
            .enumerate()
            .filter(|(_, &c)| !c)
            .map(|(i, _)| i)
            .collect()
    }
}

/// Align the full `transcript` against `sentences` (the script's main line).
///
/// Oversized inputs return no coverage evidence. Applications should prefer
/// [`try_realign`] so they can explain why recording verification was skipped.
pub fn realign(sentences: &[String], transcript: &str) -> Realignment {
    try_realign(sentences, transcript).unwrap_or_else(|_| {
        let words = transcript
            .split(|c: char| !c.is_alphanumeric())
            .filter(|word| !word.is_empty())
            .count();
        Realignment {
            sentence_match: vec![0.0; sentences.len()],
            covered: vec![false; sentences.len()],
            matched_words: 0,
            inserted_words: words,
            transcript_words: words,
        }
    })
}

/// Run exact recording verification within explicit memory and work limits.
/// An error carries no delivery evidence and must not replace live coverage.
pub fn try_realign(
    sentences: &[String],
    transcript: &str,
) -> Result<Realignment, RealignmentError> {
    let input_bytes = sentences
        .iter()
        .try_fold(transcript.len(), |total, sentence| {
            total.checked_add(sentence.len())
        });
    if input_bytes.is_none_or(|bytes| bytes > MAX_INPUT_BYTES) {
        return Err(RealignmentError {
            limit: "input size",
        });
    }
    // Script words with a word -> sentence map.
    let mut script: Vec<String> = Vec::new();
    let mut word_sentence: Vec<usize> = Vec::new();
    let mut sentence_len = vec![0usize; sentences.len()];
    for (si, s) in sentences.iter().enumerate() {
        for w in normalize(s).split_whitespace() {
            if w.len() > MAX_WORD_BYTES {
                return Err(RealignmentError {
                    limit: "word length",
                });
            }
            script.push(w.to_string());
            word_sentence.push(si);
            sentence_len[si] += 1;
        }
    }
    let spoken_norm = normalize(transcript);
    let spoken: Vec<&str> = spoken_norm.split_whitespace().collect();
    if spoken.iter().any(|word| word.len() > MAX_WORD_BYTES) {
        return Err(RealignmentError {
            limit: "word length",
        });
    }

    let empty = Realignment {
        sentence_match: vec![0.0; sentences.len()],
        covered: vec![false; sentences.len()],
        matched_words: 0,
        inserted_words: spoken.len(),
        transcript_words: spoken.len(),
    };
    if script.is_empty() || spoken.is_empty() {
        return Ok(empty);
    }

    let (m, n) = (spoken.len(), script.len());
    let cells = (m + 1).checked_mul(n + 1);
    if cells.is_none_or(|count| count > MAX_ALIGNMENT_CELLS) {
        return Err(RealignmentError {
            limit: "alignment size",
        });
    }

    // Intern both vocabularies so each distinct word pair is scored once (a
    // consult is ~2-3k words but only a few hundred distinct ones).
    let mut s_ids: HashMap<&str, usize> = HashMap::new();
    let script_ids: Vec<usize> = script
        .iter()
        .map(|w| {
            let n = s_ids.len();
            *s_ids.entry(w.as_str()).or_insert(n)
        })
        .collect();
    let mut t_ids: HashMap<&str, usize> = HashMap::new();
    let spoken_ids: Vec<usize> = spoken
        .iter()
        .map(|w| {
            let n = t_ids.len();
            *t_ids.entry(*w).or_insert(n)
        })
        .collect();
    if s_ids
        .len()
        .checked_mul(t_ids.len())
        .is_none_or(|pairs| pairs > MAX_VOCABULARY_PAIRS)
    {
        return Err(RealignmentError {
            limit: "vocabulary size",
        });
    }
    // Fuzzy comparison itself is quadratic in word length, so bounding only
    // the number of pairs still permits expensive synthetic long-word inputs.
    let script_chars: usize = s_ids.keys().map(|word| word.len()).sum();
    let spoken_chars: usize = t_ids.keys().map(|word| word.len()).sum();
    if script_chars
        .checked_mul(spoken_chars)
        .is_none_or(|work| work > MAX_VOCABULARY_WORK)
    {
        return Err(RealignmentError {
            limit: "vocabulary comparison work",
        });
    }
    let mut s_vocab = vec![""; s_ids.len()];
    for (w, &i) in &s_ids {
        s_vocab[i] = w;
    }
    let mut t_vocab = vec![""; t_ids.len()];
    for (w, &i) in &t_ids {
        t_vocab[i] = w;
    }
    let mut sim = vec![0.0f32; t_vocab.len() * s_vocab.len()];
    for (ti, tw) in t_vocab.iter().enumerate() {
        for (si, sw) in s_vocab.iter().enumerate() {
            sim[ti * s_vocab.len() + si] = word_sim(tw, sw);
        }
    }

    // Global DP. Rows = transcript words, cols = script words. Keep a rolling
    // score row and a full traceback (1 byte per cell).
    const DIAG: u8 = 0;
    const UP: u8 = 1; // transcript word inserted
    const LEFT: u8 = 2; // script word omitted
    let mut back = vec![0u8; cells.expect("alignment size checked above")];
    let mut prev: Vec<f32> = (0..=n).map(|j| j as f32 * OMISSION).collect();
    back[1..=n].fill(LEFT);
    let mut cur = vec![0.0f32; n + 1];
    for i in 1..=m {
        cur[0] = prev[0] + INSERTION;
        back[i * (n + 1)] = UP;
        let row = spoken_ids[i - 1] * s_vocab.len();
        for j in 1..=n {
            let s = sim[row + script_ids[j - 1]];
            let d = prev[j - 1] + if s >= WORD_MATCH { s } else { MISMATCH };
            let u = prev[j] + INSERTION;
            let l = cur[j - 1] + OMISSION;
            let (best, dir) = if d >= u && d >= l {
                (d, DIAG)
            } else if u >= l {
                (u, UP)
            } else {
                (l, LEFT)
            };
            cur[j] = best;
            back[i * (n + 1) + j] = dir;
        }
        std::mem::swap(&mut prev, &mut cur);
    }

    // Trace back, marking script words that were matched.
    let mut word_hit = vec![false; n];
    let (mut i, mut j) = (m, n);
    let mut matched_words = 0usize;
    while i > 0 || j > 0 {
        let dir = if i == 0 {
            LEFT
        } else if j == 0 {
            UP
        } else {
            back[i * (n + 1) + j]
        };
        match dir {
            DIAG => {
                if sim[spoken_ids[i - 1] * s_vocab.len() + script_ids[j - 1]] >= WORD_MATCH {
                    word_hit[j - 1] = true;
                    matched_words += 1;
                }
                i -= 1;
                j -= 1;
            }
            UP => i -= 1,
            _ => j -= 1,
        }
    }

    let mut hits = vec![0usize; sentences.len()];
    for (w, &hit) in word_hit.iter().enumerate() {
        if hit {
            hits[word_sentence[w]] += 1;
        }
    }
    let sentence_match: Vec<f32> = hits
        .iter()
        .zip(&sentence_len)
        .map(|(&h, &len)| if len == 0 { 0.0 } else { h as f32 / len as f32 })
        .collect();
    let covered = sentence_match
        .iter()
        .zip(&sentence_len)
        .map(|(&f, &len)| len > 0 && f >= SENTENCE_COVERED)
        .collect();
    Ok(Realignment {
        sentence_match,
        covered,
        matched_words,
        inserted_words: spoken.len() - matched_words,
        transcript_words: spoken.len(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn script() -> Vec<String> {
        vec![
            "hi thanks for meeting with me today".into(),
            "i reviewed every medication you are taking".into(),
            "you take metoprolol every morning with breakfast".into(),
            "garlic extract can thin your blood".into(),
            "does that sound helpful to you".into(),
        ]
    }

    #[test]
    fn full_read_covers_everything() {
        let t = "hi thanks for meeting with me today i reviewed every medication you are taking \
                 you take metoprolol every morning with breakfast garlic extract can thin your blood \
                 does that sound helpful to you";
        let r = realign(&script(), t);
        assert!(r.covered.iter().all(|&c| c), "{:?}", r.sentence_match);
        assert_eq!(r.inserted_words, 0);
    }

    #[test]
    fn skipped_sentence_is_omitted_and_patient_talk_is_inserted() {
        let t = "hi thanks for meeting with me today \
                 oh sure no problem happy to be here \
                 i reviewed every medication you are taking \
                 garlic extract can thin your blood does that sound helpful to you";
        let r = realign(&script(), t);
        assert_eq!(r.omitted(), vec![2], "{:?}", r.sentence_match);
        assert!(r.inserted_words >= 7);
    }

    #[test]
    fn misheard_drug_name_still_counts() {
        let t = "you take metro pro law every morning with breakfast";
        let r = realign(&script(), t);
        assert!(r.covered[2], "{:?}", r.sentence_match);
    }

    #[test]
    fn empty_inputs_are_safe() {
        assert!(realign(&[], "hello there").covered.is_empty());
        let r = realign(&script(), "");
        assert_eq!(r.omitted().len(), 5);
    }

    #[test]
    fn oversized_alignment_is_rejected_without_a_quadratic_allocation() {
        let sentences = vec!["word ".repeat(5_000)];
        let transcript = "word ".repeat(5_000);
        assert_eq!(
            try_realign(&sentences, &transcript).unwrap_err().limit,
            "alignment size"
        );
        let conservative = realign(&sentences, &transcript);
        assert_eq!(conservative.covered, vec![false]);
        assert_eq!(conservative.matched_words, 0);
        assert_eq!(conservative.inserted_words, 5_000);
    }

    #[test]
    fn expensive_vocabulary_and_extreme_tokens_are_rejected() {
        let unique = (0..1_001)
            .map(|i| format!("word{i}"))
            .collect::<Vec<_>>()
            .join(" ");
        assert_eq!(
            try_realign(std::slice::from_ref(&unique), &unique)
                .unwrap_err()
                .limit,
            "vocabulary size"
        );
        assert_eq!(
            try_realign(&script(), &"a".repeat(MAX_WORD_BYTES + 1))
                .unwrap_err()
                .limit,
            "word length"
        );
        assert_eq!(
            try_realign(&script(), &"a".repeat(MAX_INPUT_BYTES + 1))
                .unwrap_err()
                .limit,
            "input size"
        );
        let long_vocab = (0..100)
            .map(|i| format!("{}{i}", "a".repeat(150)))
            .collect::<Vec<_>>()
            .join(" ");
        assert_eq!(
            try_realign(std::slice::from_ref(&long_vocab), &long_vocab)
                .unwrap_err()
                .limit,
            "vocabulary comparison work"
        );
    }
}
