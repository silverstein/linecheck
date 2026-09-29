# Linecheck: the teleprompter for conversations

[![GitHub stars](https://img.shields.io/github/stars/silverstein/linecheck?style=social)](https://github.com/silverstein/linecheck)
[![Latest release](https://img.shields.io/github/v/release/silverstein/linecheck)](https://github.com/silverstein/linecheck/releases/latest)
[![License: MIT](https://img.shields.io/badge/License-MIT-blue.svg)](LICENSE)
[![macOS 13+](https://img.shields.io/badge/macOS-13%2B-black?logo=apple)](#install)

**A teleprompter for conversations, not monologues.**

Linecheck is a free, open-source macOS teleprompter for calls where you have to stay on script: consultations, sales calls, disclosures, onboarding, interviews. It listens to you on-device, keeps the line you're reading just under your camera, waits while the other person talks, follows the branch their answer sends you down, and afterwards tells you exactly which lines you delivered and which you skipped.

It was built for pharmacists delivering Medication Therapy Management (MTM) consultations over video, where every required line has to be said and the patient keeps interrupting. It works for any structured conversation.

<p align="center">
  <img src="docs/assets/demo.gif" alt="Linecheck following a medication review: it keeps its place through a misheard drug name and a skipped line, waits while the patient answers, follows the YES branch, and ends with a report of what was delivered" width="960">
</p>

## Why another teleprompter?

Most teleprompters assume you're reading a monologue to a camera. They scroll at a fixed speed, or follow your voice line by line. A real conversation breaks both: you skip ahead, go back to re-read, answer a question, and pick the script up again somewhere else.

| | Fixed-speed teleprompters | Voice-follow teleprompters | Linecheck |
|---|---|---|---|
| Scrolling | Constant speed | Follows your voice | Follows your voice, word by word, through skips and re-reads |
| When the other person talks | Keeps scrolling | Pauses on silence | Waits at check-in points; can detect the other side of a call |
| Yes/no answers | One linear script | One linear script | `BRANCH` points follow whichever answer you read |
| Misheard words | n/a | Loses its place | Sound-alike matching ("metro pro law" → metoprolol) |
| Afterwards | Nothing | Nothing | Report: lines delivered, lines missed, time per section, coaching |
| On a shared screen | Visible | Usually visible | Hidden from screenshots and screen sharing by default |

## Install

Download `Linecheck_<version>_aarch64.dmg` from the [latest release](https://github.com/silverstein/linecheck/releases/latest), open it, and drag Linecheck to Applications. It's signed and notarized, and it updates itself. It needs macOS 13 or later on Apple silicon (the per-script language model needs macOS 14).

Allow microphone and speech recognition access when the first session starts. No account, no API keys.

## How it works

1. Load a script: open a file, paste from the clipboard, or drag one in.
2. Put the reading line just under your camera (`[` `]`) and set the column width (`,` `.`).
3. Press **Start Session** (or Space) and start talking. Linecheck holds the line you're reading at the reading line.
4. It waits at `PAUSE` points. At `BRANCH` points it follows whichever answer you start reading, or you can click one.
5. Click **End**. You get a compliance report and coaching notes, re-checked against the full recording.

## Private by design

- Speech recognition runs on your Mac with Apple's on-device recognizer. Audio never leaves the machine.
- The session recording is used once, for the post-session check, then deleted (unless you choose to keep it).
- Reports and transcripts are written as `0600` files in `~/meetings/consults/`.
- The window is hidden from screenshots and screen sharing by default. Toggle it in Session options (`H`) or from the menu-bar icon.

## Script format

Scripts are plain annotated Markdown (`.script.md`). Frontmatter is optional: paste plain text and it works.

```markdown
---
title: MTM Consultation
variables:
  patient_name: Jane Smith
estimated_duration: 18min
---

# Intro

Hi {{patient_name}}, thanks for meeting with me today.

> PAUSE: Does that sound helpful to you?

# Findings

Let me walk you through what I found.

> BRANCH: Would you like me to get this plan started?
>> YES
Great. Let me get that organized for you.
>> NO
No problem. May I share this with your doctor?
```

Directives render as blockquotes, so a script still reads cleanly on GitHub or in any Markdown editor. See [SPEC.md](SPEC.md) for the full format.

## After a session

Linecheck writes a Markdown report to `~/meetings/consults/`:

- Sections covered and skipped, and time per section
- Pause points reached and branch decisions taken
- Adherence percentage
- Lines not delivered, restarts, and off-script stretches
- Coaching insights on pacing, coverage, pause discipline, and section balance (computed from the session data, no LLM)

Live tracking can miss things, so after you end the session Linecheck re-transcribes the whole recording, aligns it against the script again, and rewrites the report from that pass.

## How the tracking works

The interesting part is keeping your place when speech recognition is imperfect and you don't read in order.

- **Custom language model.** Each session builds a language model from the script's own spoken lines (macOS 14+), so the recognizer expects "metoprolol" rather than "metro pro law".
- **Word-level alignment.** Recognized words are aligned to the script with a bounded, monotonic dynamic-programming search around the cursor. It uses sound-alike matching for misheard terms and handles words the recognizer splits or joins.
- **Whole-script relocation.** A second search looks across the entire script. The cursor only jumps far after three agreeing, progressing matches, so one stray phrase can't throw it to the wrong section. Repeated lines don't mislead it: it waits until the words that follow tell the copies apart.
- **Post-session re-alignment.** A global alignment of the full-recording transcript decides, sentence by sentence, what was delivered and what was omitted.

The engine lives in `prompter-core`, a pure Rust crate with no platform dependencies. It builds and tests on Linux, and every live session's recognizer stream can be replayed through it (`cargo run -p prompter-core --example replay`).

## Keyboard shortcuts

| Key | Action |
|-----|--------|
| Space | Start / Pause / Resume |
| `[` / `]` | Move the reading line up / down |
| `,` / `.` | Narrower / wider text column |
| `+` / `-` | Font size |
| Up / Down | Previous / next sentence |
| Click a line | Make it the current line |
| 1–9 | Jump to section |
| Tab / Enter | Choose a branch answer by hand |
| Cmd+O | Open file |
| Cmd+V | Paste script |
| H | Help and session options |
| Esc | End session |

## Build from source

Requires Rust, the Tauri CLI (`cargo install tauri-cli`), and the Xcode command line tools (for the Swift speech helper).

```bash
# Build the app (Swift helper + Tauri bundle), and optionally install it
./scripts/build.sh --install

# Run the tests (the core crate also builds and tests on Linux)
cargo test
```

```
linecheck/
├── crates/
│   ├── core/                    # Rust library: pure logic, no audio
│   │   ├── script.rs            # .script.md parser
│   │   ├── align.rs             # Word-level alignment engine
│   │   ├── tracker.rs           # Live position, relocator, pauses and branches
│   │   ├── session.rs           # Speech-verified coverage and transcript
│   │   ├── realign.rs           # Post-session re-alignment of the recording
│   │   ├── compliance.rs        # Session report generator
│   │   └── coaching.rs          # Data-driven delivery analysis
│   └── app/                     # Tauri v2 desktop app
│       ├── src/main.rs          # Tauri commands, speech helper, verification
│       └── ui/index.html        # Teleprompter UI (vanilla JS, no build step)
├── scripts/
│   ├── speech-recognizer.swift  # On-device speech helper (Apple Speech)
│   └── build.sh                 # Build + install script
├── SPEC.md                      # .script.md format specification
└── INTEGRATION.md               # Integration guide for script sources
```

## Sending scripts to Linecheck

Other tools can hand scripts to Linecheck three ways:

- **Watched folder:** scripts saved to `~/meetings/scripts/` appear in Linecheck's list.
- **URL scheme:** `linecheck://open?file=/path/to/script.script.md`, or `linecheck://open?consultation_id=abc-123` to open the matching script from the watched folder or `~/Downloads`.
- **Clipboard:** copy Markdown anywhere, then Cmd+V in Linecheck.

See [INTEGRATION.md](INTEGRATION.md).

## Related

Linecheck shares the `~/meetings/` folder convention with [Minutes](https://github.com/silverstein/minutes), an open-source, local-first conversation memory for Claude Code, Codex, and other MCP clients.

Linecheck was called Prompter until September 2026; `prompter://` links still work.

Built by [Mat Silverstein](https://github.com/silverstein).

## License

MIT
