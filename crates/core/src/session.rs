//! Session recorder: speech-verified coverage + transcript.
//!
//! Consumes the same [`TrackUpdate`] stream the UI renders (plus the recognized
//! text) and accumulates *evidence* of what was actually said, so the compliance
//! report reflects delivery rather than cursor position. The old path derived
//! coverage from the raw cursor index, so a single forward jump marked every
//! earlier sentence "covered"; here a main sentence counts as covered only when
//! a committed match actually landed on it. Also captures the branch path taken,
//! which pause points were reached, and the recognized transcript (which the
//! prior pipeline never persisted).

use crate::align::normalize;
use crate::compliance::{ComplianceReport, DeliveryStats};
use crate::realign::Realignment;
use crate::script::{Directive, Element, Script};
use crate::tracker::{TrackState, TrackUpdate};
use std::collections::{HashMap, HashSet};

/// One recognized snapshot of the session transcript (possibly provisional).
#[derive(Debug, Clone, PartialEq)]
pub struct TranscriptLine {
    /// Main-line sentence index associated with this snapshot.
    pub sentence_index: usize,
    /// The recognized text that produced this snapshot.
    pub recognized: String,
    /// Whether this line entered a branch option (and which).
    pub branch_option: Option<String>,
}

/// Accumulates speech evidence across a session.
pub struct SessionRecorder {
    script_title: String,
    script_version: Option<String>,
    /// Section index for each main sentence.
    sentence_section: Vec<usize>,
    /// Word count for each main sentence.
    sentence_words: Vec<usize>,
    section_names: Vec<String>,
    pause_total: usize,
    total_words: usize,

    /// Whether each main sentence has committed evidence of being spoken.
    covered: Vec<bool>,
    /// Distinct pause points reached, keyed by the preceding main sentence.
    pauses_reached: HashSet<usize>,
    /// Branch question -> chosen option label.
    branches_taken: HashMap<String, String>,
    transcript: Vec<TranscriptLine>,
    /// Last timeline location written to the transcript. Unlike a high-water
    /// mark, this also records re-reads and individual branch sentences.
    last_transcript_timeline: Option<usize>,
    /// A provisional transcript line is replaced as the recognizer revises it.
    last_transcript_partial: bool,
    /// Text of each main sentence (for "lines not delivered" excerpts).
    sentence_text: Vec<String>,
    /// Last sentence with match evidence (to spot going back to re-read).
    last_matched: Option<usize>,
    repeats: usize,
    off_script_episodes: usize,
    in_off_script: bool,
    /// Set by [`Self::apply_realignment`] / [`Self::set_timing`].
    off_script_words: Option<usize>,
    speaking_wpm: Option<f32>,
    patient_talk_secs: Option<u64>,
    verified_by_recording: bool,
}

/// Longest excerpt used for an omitted line in the report.
const EXCERPT_WORDS: usize = 10;
/// Most omitted lines listed in a report.
const MAX_OMITTED_LISTED: usize = 12;

impl SessionRecorder {
    /// Build a recorder for a script.
    pub fn new(script: &Script) -> Self {
        let mut sentence_section = Vec::new();
        let mut sentence_words = Vec::new();
        let mut section_names = Vec::new();
        let mut pause_total = 0usize;
        let mut total_words = 0usize;
        let mut sentence_text = Vec::new();

        for (section_index, section) in script.sections.iter().enumerate() {
            section_names.push(section.name.clone());
            // A section heading breaks adjacency, mirroring the tracker: a pause
            // is "reachable" only when a main sentence immediately precedes it.
            let mut prev_was_main = false;
            for element in &section.elements {
                match element {
                    Element::Text(sentences) => {
                        for sentence in sentences {
                            sentence_section.push(section_index);
                            sentence_words.push(sentence.word_count);
                            sentence_text.push(sentence.text.clone());
                            total_words += sentence.word_count;
                        }
                        prev_was_main = !sentences.is_empty();
                    }
                    // Count only pauses the tracker can actually reach (those
                    // immediately after a main sentence), so reached/total is an
                    // honest ratio. Consecutive or branch-adjacent pauses are a
                    // known limitation.
                    Element::Directive(Directive::Pause { .. }) => {
                        if prev_was_main {
                            pause_total += 1;
                        }
                        prev_was_main = false;
                    }
                    // Branch option words are optional paths and are not counted
                    // in main-line adherence (consistent with the tracker, which
                    // keeps branch sentences out of the main aligner).
                    Element::Directive(Directive::Branch { .. }) => {
                        prev_was_main = false;
                    }
                }
            }
        }

        let covered = vec![false; sentence_section.len()];
        Self {
            script_title: script.frontmatter.title.clone(),
            script_version: script.frontmatter.version.clone(),
            sentence_section,
            sentence_words,
            section_names,
            pause_total,
            total_words,
            covered,
            pauses_reached: HashSet::new(),
            branches_taken: HashMap::new(),
            transcript: Vec::new(),
            last_transcript_timeline: None,
            last_transcript_partial: false,
            sentence_text,
            last_matched: None,
            repeats: 0,
            off_script_episodes: 0,
            in_off_script: false,
            off_script_words: None,
            speaking_wpm: None,
            patient_talk_secs: None,
            verified_by_recording: false,
        }
    }

    /// Record one update and the text that produced it.
    ///
    /// Evidence may come from a committed final OR a confident partial: Apple's
    /// recognizer only finalizes at long pauses, so during continuous reading
    /// matched partials are the ONLY delivery evidence the recorder ever sees.
    /// Gating on `committed` alone left the compliance report empty for an
    /// entire read. Unmatched, non-committed updates still carry nothing.
    pub fn record(&mut self, update: &TrackUpdate, recognized: &str) {
        // Off-script stretches: count each entry into the ad-lib state once.
        let off = matches!(update.state, TrackState::AdLibbing);
        if off && !self.in_off_script {
            self.off_script_episodes += 1;
        }
        if update.matched || off {
            self.in_off_script = off;
        }
        if !update.matched && !update.committed {
            return;
        }
        // Going back to an earlier line (a restart / re-read) counts once per
        // backward move.
        if update.matched && !matches!(update.state, TrackState::InBranch { .. }) {
            if let Some(prev) = self.last_matched {
                if update.sentence_index < prev {
                    self.repeats += 1;
                }
            }
            self.last_matched = Some(update.sentence_index);
        }

        // Coverage / pause evidence counts ONLY on a real match (partial or
        // final). An unmatched committed final (off-script speech, a short
        // utterance) must not mark a sentence delivered -- that was the
        // over-count the cursor-based path had.
        if update.matched {
            match &update.state {
                TrackState::Speaking | TrackState::AtBranch { .. } => {
                    if update.sentence_index < self.covered.len() {
                        self.covered[update.sentence_index] = true;
                    }
                }
                TrackState::AtPause { .. } => {
                    if update.sentence_index < self.covered.len() {
                        self.covered[update.sentence_index] = true;
                        self.pauses_reached.insert(update.sentence_index);
                    }
                }
                // InBranch keeps the pre-branch main index for navigation.
                // Matching an answer (or selecting it manually) is no evidence
                // that this earlier sentence was delivered. A single final can
                // contain BOTH the full lead-in and answer, so preserve that
                // direct evidence without inferring it from branch position.
                TrackState::InBranch { .. } if update.branch_choice.is_some() => {
                    if let Some(sentence) = self.sentence_text.get(update.sentence_index) {
                        let sentence = normalize(sentence);
                        if !sentence.is_empty()
                            && format!(" {} ", normalize(recognized))
                                .contains(&format!(" {sentence} "))
                        {
                            self.covered[update.sentence_index] = true;
                        }
                    }
                }
                TrackState::InBranch { .. } | TrackState::AdLibbing => {}
            }
        }

        // Branch attribution comes straight from the selecting update -- no
        // stateful reconstruction, so set_position-entered or same-update
        // selections are not missed and stale questions cannot mis-attribute.
        if let Some(choice) = &update.branch_choice {
            self.branches_taken
                .insert(choice.question.clone(), choice.option_label.clone());
        }

        // Keep the latest hypothesis per contiguous timeline location, then
        // replace it with the authoritative final. Retaining only the FIRST
        // partial lost every word recognized later in that sentence. These are
        // recognition snapshots; a rolling-window ASR input is not a verbatim
        // full-session transcript.
        if !recognized.trim().is_empty()
            && (update.committed || update.branch_choice.is_some() || update.matched)
        {
            let line = TranscriptLine {
                sentence_index: update.sentence_index,
                recognized: recognized.to_string(),
                branch_option: match &update.state {
                    TrackState::InBranch { option_label } => Some(option_label.clone()),
                    _ => None,
                },
            };
            if self.last_transcript_partial
                && self.last_transcript_timeline == Some(update.timeline_index)
            {
                *self
                    .transcript
                    .last_mut()
                    .expect("partial has a transcript line") = line;
            } else {
                self.transcript.push(line);
            }
            self.last_transcript_timeline = Some(update.timeline_index);
            self.last_transcript_partial = !update.committed;
        }
    }

    /// Word count of sentences with alignment evidence of delivery.
    /// This is a sentence-level estimate, not a verbatim spoken-word count.
    pub fn words_delivered(&self) -> usize {
        self.sentence_words
            .iter()
            .zip(&self.covered)
            .filter(|(_, &c)| c)
            .map(|(w, _)| *w)
            .sum()
    }

    /// Sections with at least one covered sentence, and those with none.
    fn section_split(&self) -> (Vec<String>, Vec<String>) {
        let mut any_covered = vec![false; self.section_names.len()];
        for (sentence_idx, &section_idx) in self.sentence_section.iter().enumerate() {
            if self.covered[sentence_idx] {
                any_covered[section_idx] = true;
            }
        }
        let mut covered = Vec::new();
        let mut skipped = Vec::new();
        for (i, name) in self.section_names.iter().enumerate() {
            if any_covered[i] {
                covered.push(name.clone());
            } else {
                skipped.push(name.clone());
            }
        }
        (covered, skipped)
    }

    /// Build an alignment-based compliance report. `duration_secs` comes from the
    /// caller (core has no clock).
    pub fn build_report(&self, duration_secs: u64) -> ComplianceReport {
        let (sections_covered, sections_skipped) = self.section_split();
        ComplianceReport {
            script_title: self.script_title.clone(),
            script_version: self.script_version.clone(),
            sections_covered,
            sections_skipped,
            duration_secs,
            section_times: HashMap::new(),
            pause_points_reached: self.pauses_reached.len(),
            pause_points_total: self.pause_total,
            branches_taken: self.branches_taken.clone(),
            total_words: self.total_words,
            words_delivered: self.words_delivered(),
            delivery: self.delivery_stats(),
        }
    }

    /// Replace live coverage with the full-recording re-alignment. The recording
    /// is authoritative when it holds a real read; a near-empty recording (mic
    /// failure, a 5-second session) is ignored so it can't wipe live evidence.
    /// Returns whether it was applied.
    pub fn apply_realignment(&mut self, r: &Realignment) -> bool {
        if r.covered.len() != self.covered.len() {
            return false;
        }
        // A fifth of the script, at least 10 words (but never more than half a
        // very short script).
        let min_words = (self.total_words / 5)
            .max(10)
            .min(self.total_words / 2)
            .max(1);
        if r.matched_words < min_words {
            return false;
        }
        self.covered = r.covered.clone();
        self.off_script_words = Some(r.inserted_words);
        self.verified_by_recording = true;
        true
    }

    /// Timing facts measured outside the tracker: speaking pace (from the
    /// recording's word timings) and patient talk time (from call audio).
    pub fn set_timing(&mut self, speaking_wpm: Option<f32>, patient_talk_secs: Option<u64>) {
        if speaking_wpm.is_some() {
            self.speaking_wpm = speaking_wpm;
        }
        if patient_talk_secs.is_some() {
            self.patient_talk_secs = patient_talk_secs;
        }
    }

    /// Main-line sentence texts, in order (what the re-alignment runs against).
    pub fn sentences(&self) -> &[String] {
        &self.sentence_text
    }

    fn delivery_stats(&self) -> DeliveryStats {
        let omitted_lines = self
            .covered
            .iter()
            .enumerate()
            .filter(|(_, &c)| !c)
            .take(MAX_OMITTED_LISTED)
            .map(|(i, _)| {
                let words: Vec<&str> = self.sentence_text[i].split_whitespace().collect();
                let mut e = words[..words.len().min(EXCERPT_WORDS)].join(" ");
                if words.len() > EXCERPT_WORDS {
                    e.push('…');
                }
                e
            })
            .collect();
        DeliveryStats {
            omitted_lines,
            repeats: self.repeats,
            off_script_episodes: self.off_script_episodes,
            off_script_words: self.off_script_words,
            speaking_wpm: self.speaking_wpm,
            patient_talk_secs: self.patient_talk_secs,
            verified_by_recording: self.verified_by_recording,
        }
    }

    /// Render the recognized transcript as a portable markdown artifact.
    pub fn transcript_markdown(&self) -> String {
        let mut out = format!("# Transcript: {}\n\n", self.script_title);
        if self.transcript.is_empty() {
            out.push_str("_No recognized speech recorded._\n");
            return out;
        }
        for line in &self.transcript {
            if let Some(label) = &line.branch_option {
                out.push_str(&format!("- [branch: {label}] {}\n", line.recognized));
            } else {
                out.push_str(&format!("- {}\n", line.recognized));
            }
        }
        out
    }

    /// The recorded transcript lines.
    pub fn transcript(&self) -> &[TranscriptLine] {
        &self.transcript
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::script::{BranchOption, Frontmatter, Section, Sentence};
    use crate::speech::SpeechUpdate;
    use crate::tracker::ScriptTracker;

    fn sent(text: &str) -> Sentence {
        Sentence {
            text: text.into(),
            word_count: text.split_whitespace().count(),
        }
    }

    fn sample_script() -> Script {
        Script {
            frontmatter: Frontmatter {
                title: "Consult".into(),
                r#type: None,
                version: Some("1".into()),
                variables: Default::default(),
                estimated_duration: None,
            },
            sections: vec![
                Section {
                    name: "Intro".into(),
                    word_count: 0,
                    elements: vec![
                        Element::Text(vec![
                            sent("hi thanks for meeting with me today"),
                            sent("my goal is simple to keep your medications safe"),
                        ]),
                        Element::Directive(Directive::Pause {
                            prompt: "wait".into(),
                        }),
                    ],
                },
                Section {
                    name: "Review".into(),
                    word_count: 0,
                    elements: vec![
                        Element::Text(vec![sent(
                            "you are currently taking warfarin and metformin",
                        )]),
                        Element::Directive(Directive::Branch {
                            question: "any questions".into(),
                            options: vec![
                                BranchOption {
                                    label: "YES".into(),
                                    sentences: vec![sent("great lets discuss your concerns now")],
                                },
                                BranchOption {
                                    label: "NO".into(),
                                    sentences: vec![sent("okay then lets keep moving along")],
                                },
                            ],
                        }),
                        Element::Text(vec![sent("does this all make sense so far")]),
                    ],
                },
            ],
            word_count: 0,
        }
    }

    /// Feed a sequence of finals through a tracker into a recorder.
    fn run(updates: &[&str]) -> (SessionRecorder, Script) {
        let script = sample_script();
        let mut tracker = ScriptTracker::new(&script);
        let mut rec = SessionRecorder::new(&script);
        for &text in updates {
            let u = tracker.observe(&SpeechUpdate::finalized(text));
            rec.record(&u, text);
        }
        (rec, script)
    }

    #[test]
    fn coverage_is_speech_verified_not_cursor_jump() {
        // Speaker reads only the first sentence, then jumps to the review line,
        // skipping sentence 2. Sentence 2 must NOT count as covered.
        let (rec, _) = run(&[
            "hi thanks for meeting with me today",
            "you are currently taking warfarin and metformin",
        ]);
        // Covered: sentence 0 (7 words) + sentence 2 (7 words) = 14. Sentence 1
        // (the skipped one, 9 words) is excluded.
        let report = rec.build_report(60);
        assert_eq!(report.words_delivered, 7 + 7);
        assert!(report.total_words > report.words_delivered);
    }

    #[test]
    fn sections_split_by_evidence() {
        let (rec, _) = run(&["hi thanks for meeting with me today"]);
        let report = rec.build_report(10);
        assert_eq!(report.sections_covered, vec!["Intro".to_string()]);
        assert_eq!(report.sections_skipped, vec!["Review".to_string()]);
    }

    #[test]
    fn pause_reached_is_counted() {
        let (rec, _) = run(&["my goal is simple to keep your medications safe"]);
        let report = rec.build_report(10);
        assert_eq!(report.pause_points_total, 1);
        assert_eq!(report.pause_points_reached, 1);
    }

    #[test]
    fn branch_path_is_recorded() {
        let (rec, _) = run(&[
            "you are currently taking warfarin and metformin",
            "okay then lets keep moving along",
        ]);
        let report = rec.build_report(20);
        assert_eq!(
            report.branches_taken.get("any questions"),
            Some(&"NO".to_string())
        );
    }

    #[test]
    fn transcript_is_persisted() {
        let (rec, _) = run(&[
            "you are currently taking warfarin and metformin",
            "great lets discuss your concerns now",
        ]);
        let md = rec.transcript_markdown();
        assert!(md.contains("warfarin and metformin"));
        assert!(md.contains("[branch: YES]"));
        assert_eq!(rec.transcript().len(), 2);
    }

    #[test]
    fn unmatched_partial_carries_no_evidence() {
        // Off-script partial that does not align: no coverage, no transcript.
        let script = sample_script();
        let mut tracker = ScriptTracker::new(&script);
        let mut rec = SessionRecorder::new(&script);
        let u = tracker.observe(&SpeechUpdate::partial(
            "kubernetes pod autoscaling webpack bundle configuration",
        ));
        rec.record(
            &u,
            "kubernetes pod autoscaling webpack bundle configuration",
        );
        assert_eq!(rec.build_report(5).words_delivered, 0);
        assert!(rec.transcript().is_empty());
    }

    #[test]
    fn matched_partial_is_evidence() {
        // A confident partial IS delivery evidence: with a partials-only
        // provider (no finals during continuous reading) this is the only
        // signal the compliance report can be built from. Regression for the
        // "report shows 0 words delivered after reading the whole script" bug.
        let script = sample_script();
        let mut tracker = ScriptTracker::new(&script);
        let mut rec = SessionRecorder::new(&script);
        let text = "hi thanks for meeting with me today";
        let u = tracker.observe(&SpeechUpdate::partial(text));
        assert!(u.matched && !u.committed, "matched partial, not committed");
        rec.record(&u, text);
        // Sentence 0 (7 words) is now credited from the partial alone.
        assert_eq!(rec.build_report(5).words_delivered, 7);
        assert_eq!(rec.transcript().len(), 1);
    }

    #[test]
    fn repeated_partials_on_one_sentence_dont_spam_transcript() {
        // Cumulative partials re-emit the same leading edge many times; the
        // transcript must add at most one line per sentence reached.
        let script = sample_script();
        let mut tracker = ScriptTracker::new(&script);
        let mut rec = SessionRecorder::new(&script);
        let text = "hi thanks for meeting with me today";
        for _ in 0..5 {
            let u = tracker.observe(&SpeechUpdate::partial(text));
            rec.record(&u, text);
        }
        assert_eq!(rec.transcript().len(), 1);
    }

    #[test]
    fn partial_transcript_keeps_the_latest_words_and_is_replaced_by_the_final() {
        let script = sample_script();
        let mut tracker = ScriptTracker::new(&script);
        let mut rec = SessionRecorder::new(&script);
        for text in [
            "hi thanks",
            "hi thanks for meeting",
            "hi thanks for meeting with me today",
        ] {
            rec.record(&tracker.observe(&SpeechUpdate::partial(text)), text);
        }
        assert_eq!(rec.transcript().len(), 1);
        assert_eq!(
            rec.transcript()[0].recognized,
            "hi thanks for meeting with me today"
        );
        let text = "Hi, thanks for meeting with me today.";
        rec.record(&tracker.observe(&SpeechUpdate::finalized(text)), text);
        assert_eq!(rec.transcript().len(), 1);
        assert_eq!(rec.transcript()[0].recognized, text);
    }

    #[test]
    fn skipping_straight_to_an_answer_does_not_cover_its_lead_in() {
        let (rec, _) = run(&["okay then lets keep moving along"]);
        let report = rec.build_report(10);
        assert_eq!(report.words_delivered, 0);
        assert_eq!(
            report.branches_taken.get("any questions"),
            Some(&"NO".to_string())
        );
    }

    #[test]
    fn one_final_containing_the_lead_in_and_answer_preserves_both() {
        let (rec, _) = run(&[
            "you are currently taking warfarin and metformin okay then lets keep moving along",
        ]);
        let report = rec.build_report(10);
        assert_eq!(report.words_delivered, 7);
        assert_eq!(
            report.branches_taken.get("any questions"),
            Some(&"NO".to_string())
        );
    }

    #[test]
    fn manually_selecting_an_answer_does_not_cover_its_lead_in() {
        let script = sample_script();
        let mut tracker = ScriptTracker::new(&script);
        let mut rec = SessionRecorder::new(&script);
        tracker.choose_branch(0, 0);
        let text = "great lets discuss your concerns now";
        rec.record(&tracker.observe(&SpeechUpdate::partial(text)), text);
        assert_eq!(rec.build_report(10).words_delivered, 0);
        assert_eq!(rec.transcript()[0].branch_option.as_deref(), Some("YES"));
    }

    #[test]
    fn partial_transcript_preserves_each_branch_sentence_and_reread() {
        let script = crate::script::parse(
            "# Story\n\nFirst opening words here. Final closing words here.\n",
        )
        .unwrap();
        let mut rec = SessionRecorder::new(&script);
        let partial = |timeline, state| TrackUpdate {
            sentence_index: 0,
            timeline_index: timeline,
            committed: false,
            matched: true,
            confidence: 1.0,
            state,
            branch_choice: None,
            relocated: false,
        };
        let branch = || TrackState::InBranch {
            option_label: "YES".into(),
        };
        rec.record(&partial(3, branch()), "First answer sentence");
        rec.record(&partial(4, branch()), "Second answer sentence");
        rec.record(
            &partial(1, TrackState::Speaking),
            "First opening words here",
        );
        assert_eq!(rec.transcript().len(), 3);
        assert!(rec.transcript()[..2]
            .iter()
            .all(|line| line.branch_option.as_deref() == Some("YES")));
        assert_eq!(rec.transcript()[2].branch_option, None);
    }

    #[test]
    fn repeats_and_off_script_are_counted_once_per_event() {
        use crate::tracker::{TrackState, TrackUpdate};
        let script = crate::script::parse(
            "# A\n\nOne two three four. Five six seven eight. Nine ten eleven twelve.\n",
        )
        .unwrap();
        let mut rec = SessionRecorder::new(&script);
        let up = |i: usize, state: TrackState, matched: bool| TrackUpdate {
            sentence_index: i,
            timeline_index: i + 1,
            committed: true,
            matched,
            confidence: 1.0,
            state,
            branch_choice: None,
            relocated: false,
        };
        rec.record(&up(0, TrackState::Speaking, true), "one two");
        rec.record(&up(2, TrackState::Speaking, true), "nine ten");
        rec.record(&up(1, TrackState::Speaking, true), "five six"); // went back
        rec.record(&up(2, TrackState::Speaking, true), "nine ten");
        rec.record(&up(2, TrackState::AdLibbing, false), "blah");
        rec.record(&up(2, TrackState::AdLibbing, false), "blah blah");
        rec.record(&up(2, TrackState::Speaking, true), "eleven twelve");
        let d = rec.build_report(60).delivery;
        assert_eq!(d.repeats, 1);
        assert_eq!(d.off_script_episodes, 1);
    }

    #[test]
    fn realignment_replaces_live_coverage_only_with_a_real_recording() {
        let script = crate::script::parse(
            "# A\n\nOne two three four. Five six seven eight. Nine ten eleven twelve.\n",
        )
        .unwrap();
        let mut rec = SessionRecorder::new(&script);
        let tiny = crate::realign::realign(rec.sentences(), "one two");
        assert!(!rec.apply_realignment(&tiny), "too little to trust");
        let full =
            crate::realign::realign(rec.sentences(), "one two three four nine ten eleven twelve");
        assert!(rec.apply_realignment(&full));
        let r = rec.build_report(60);
        assert!(r.delivery.verified_by_recording);
        assert_eq!(
            r.delivery.omitted_lines,
            vec!["Five six seven eight.".to_string()]
        );
    }
}
