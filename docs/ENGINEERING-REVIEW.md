# Linecheck engineering assessment

Review baseline: `e773588` (v0.2.1 line), reviewed October 12, 2026.

## Assessment

Linecheck has a strong, distinctive product foundation: a local-first conversation prompter with branches, interruption recovery, and an independent post-session check. The pure Rust core, real recognizer recordings, and small Tauri shell are good architectural choices. Keep those strengths.

The next step toward state-of-the-art quality is measurable trustworthiness. Accurate recovery and honest evidence matter more here than a larger model or a visual redesign. The existing tests demonstrate useful behavior, but they do not establish comparative accuracy across accents, microphones, noisy calls, or consequential wording changes. This upgrade does not claim that benchmark.

## Implemented in this change

| Finding | Change | User benefit |
| --- | --- | --- |
| A capped/held cursor could credit a different sentence from the one matched | Attribute evidence only to the actual matched location | Skipping ahead does not invent delivery of intervening lines |
| Reading a branch answer could credit its skipped lead-in | Separate branch navigation from main-line evidence; retain lead-in credit only when its full text is present | Branch selection does not imply the preceding line was read |
| Partial branch/relocation updates could be labeled final; later partial text was lost | Preserve recognizer finality and refresh provisional transcript snapshots | More faithful session records |
| Fuzzy matching could treat different numeric tokens as equivalent | Require exact matching for digit-bearing tokens | Values such as `20mg` and `200mg` are not fuzzy equivalents |
| Checklist substring matching accepted unrelated word fragments and ambiguous verdicts | Match contiguous normalized tokens and exact structured verdicts; preserve per-item fallback evidence | Fewer accidental checklist positives |
| Reports were written before permissions were restricted, with racy filename selection | Create private temporary files, publish without clobbering, and replace atomically | No public-permission creation window or partial report replacement |
| YAML interpolated unescaped strings and unordered maps | Quote scalar values and sort map entries | Titles, punctuation, and branches round-trip reliably |
| Raw script/speech logging was automatic | Add a disabled-by-default diagnostic replay setting; remove speech previews from stderr | Debugging retention is an explicit choice |
| Verification had no outer deadline or alignment work budget | Bound helper execution/output and alignment size, vocabulary, and word-comparison work | Oversized or stuck checks fail visibly while preserving the live report |
| Startup, shutdown, and fast verification events could race | Register listeners first, await shutdown, correlate results to saved reports, count concurrent checks | Results belong to the correct session |
| UI could substitute cursor movement for evidence or run a timer during voice mode | Render canonical report data, disclose timer mode, and separate tracking mechanisms | The UI does not overstate what speech recognition established |
| Global hotkeys intercepted normal control behavior | Preserve native keyboard interactions, trap modal focus, support reduced motion | More usable keyboard and assistive-technology interaction |
| Only releases exercised CI | Add pull-request Linux core/UI/process tests and native macOS compilation/tests | Regressions are checked before release |

## Meaning of the report

- “Covered” is heuristic delivery evidence, not proof of meaning, correctness, consent, or regulatory compliance. A section is covered when at least one main-line sentence is covered; partial sections require review.
- Post-session sentence coverage still uses the existing 50% word-match threshold. Exact numeric-token matching alone does **not** guarantee that an entire sentence containing the wrong dose is rejected: its other words may exceed that threshold. Critical-token semantics need a separate policy.
- Main-line adherence excludes optional branch words. The post-session pass is monotonic over the main script; branch speech and rereads are not modeled as a full conversation graph.
- Live transcripts are recognition snapshots, not complete verbatim records. The full-recording transcript is a separate artifact when verification succeeds.
- Pause freezes the prompter while the microphone remains active. End stops it.
- A failed or too-short verification preserves the live report. “Verified” describes the recording-alignment pass, not an external clinical assessment.

## Next improvements, in order

### 1. An evaluation corpus and published scorecard

Extend the existing recording fixtures with consented or synthetic labeled sessions: ordinary reading, skipped lines, rereads, identical phrases in different sections, branches, speaker overlap, negation, changed numbers, accents, and microphone noise. Keep participant-identifying data out of the repository. Separate tuning examples from a held-out evaluation set.

Measure false coverage, missed coverage, cursor error, recovery latency, branch-selection error, and p50/p95 processing time. Publish both denominators and failure examples. Make accuracy/latency budgets a regression gate rather than treating a smooth demo as evidence of general performance.

### 2. Timestamped, inspectable evidence

Attach recognized word spans and confidence/provenance to each line. Let the user inspect the transcript excerpt supporting a covered sentence and see “uncertain” separately from “not heard.” Define exact critical tokens for numbers, units, names, negation, and required disclosures. Validate that policy against the corpus before changing report semantics.

### 3. Branch-aware verification

Model the script as a graph of main lines and chosen optional paths. Align against admissible paths, preserve explicit branch decisions, and allow bounded rereads. Compare it with the current monotonic baseline using the same held-out sessions. Incorrectly chosen branches should remain visible; an optimizer should not silently choose whichever path maximizes the score.

### 4. Script readiness and authoring feedback

Add a pure-core lint API and pre-session preview for unresolved variables, ambiguous duplicate headings, empty branches, and directives unreachable by the tracker. Offer warnings with source locations before listening starts. Do not silently rewrite a script containing required wording.

### 5. Hardened capture and maintainable application modules

Extract speech-worker ownership and session lifecycle into testable modules, add real macOS microphone/permission smoke tests, and make capture failures and retention rules inspectable. Then evaluate alternative on-device speech backends against the same corpus before expanding platform support.

## Validation and practical limits

Commands used by the new CI:

```sh
cargo test -p prompter-core --all-targets --locked
cargo clippy -p prompter-core --all-targets --locked -- -D warnings
node --test crates/app/ui/*.test.cjs
rustc --edition=2021 --test crates/app/src/process.rs -o /tmp/linecheck-process-tests
/tmp/linecheck-process-tests
```

The native macOS job compiles the Swift helper and runs `cargo test -p prompter-app --locked` without release signing secrets. Linux tests cannot establish that macOS permissions, audio capture, or the rendered Tauri window work correctly. Before shipping, exercise a real microphone session, pause/resume, a denied permission, a successful and failed recording check, and a new session while the previous check completes.

Atomic report publication uses same-directory hard links and replacement renames, supported by normal local macOS/Linux filesystems. A filesystem that does not support them returns an error rather than falling back to an unsafe write. Private permissions apply to newly created directories; existing parent-directory permissions are not silently changed. Previously saved diagnostic recordings are not automatically deleted.

## Local integration validation

The patch was integrated against GitHub master `e773588` on October 12, 2026. On Apple silicon macOS, the Swift helper compiled and the Rust/Tauri application compiled and passed its four process tests. The 111 core unit tests, 13 integration/replay tests, 14 frontend tests, and strict core Clippy passed. A browser render check with a mocked Tauri bridge confirmed report states, native keyboard controls, modal focus wrapping, and inert background content. The report label was corrected to describe sentence-level estimated coverage rather than matched spoken words. Real microphone, speech permissions, and capture behavior remain unverified; run the native smoke checks above before release.
