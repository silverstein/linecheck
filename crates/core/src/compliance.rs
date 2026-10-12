use serde::Serialize;
use std::collections::{BTreeMap, HashMap};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

// ──────────────────────────────────────────────────────────────
// Compliance report — written after each teleprompter session.
//
//   Session events ──▶ ComplianceReport ──▶ YAML frontmatter + transcript
//                                          saved to ~/meetings/consults/
//
// The report includes: sections covered, time per section,
// pause points reached, branches taken, total duration.
// ──────────────────────────────────────────────────────────────

/// How the script was delivered, beyond coverage. Categories follow reading
/// assessment practice (Microsoft Reading Progress: omission, insertion,
/// repetition) so coaching speaks a familiar language.
#[derive(Debug, Clone, Default, Serialize)]
pub struct DeliveryStats {
    /// Script lines not delivered (short excerpts, in script order).
    pub omitted_lines: Vec<String>,
    /// Times the speaker went back to re-read / restart a line.
    pub repeats: usize,
    /// Separate stretches of off-script speech the live tracker noticed.
    pub off_script_episodes: usize,
    /// Words spoken that matched no script word (from the full-recording pass).
    pub off_script_words: Option<usize>,
    /// Speaking pace in words per minute over time actually spent speaking.
    pub speaking_wpm: Option<f32>,
    /// Seconds the other party (patient) was heard speaking, when call audio
    /// was captured separately.
    pub patient_talk_secs: Option<u64>,
    /// True when coverage comes from re-aligning the full recording rather than
    /// the live tracker alone.
    pub verified_by_recording: bool,
}

/// A completed session's compliance data.
#[derive(Debug, Clone, Default, Serialize)]
pub struct ComplianceReport {
    pub script_title: String,
    pub script_version: Option<String>,
    pub sections_covered: Vec<String>,
    pub sections_skipped: Vec<String>,
    pub duration_secs: u64,
    pub section_times: HashMap<String, u64>,
    pub pause_points_reached: usize,
    pub pause_points_total: usize,
    pub branches_taken: HashMap<String, String>,
    pub total_words: usize,
    pub words_delivered: usize,
    pub delivery: DeliveryStats,
}

impl ComplianceReport {
    /// Format duration as "M:SS".
    pub fn duration_display(&self) -> String {
        fmt_duration(self.duration_secs)
    }

    /// Adherence percentage (words delivered / total words).
    pub fn adherence_pct(&self) -> f64 {
        if self.total_words == 0 {
            return 0.0;
        }
        (self.words_delivered as f64 / self.total_words as f64 * 100.0).min(100.0)
    }

    /// Write the compliance report as a Minutes-compatible markdown file.
    /// Returns the path where the file was written.
    pub fn write_to_dir(&self, dir: &Path) -> Result<PathBuf, std::io::Error> {
        let mut builder = std::fs::DirBuilder::new();
        builder.recursive(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt;
            builder.mode(0o700);
        }
        builder.create(dir)?;

        // Publish a complete private file without replacing another session's
        // report. Checking `exists()` before writing races with other writers.
        let date = chrono_lite_date();
        let slug = slugify(&self.script_title);
        let base = format!("{}-{}", date, slug);
        let pending = PrivateFile::new(dir, &self.render(&date))?;
        for counter in 1u64.. {
            let filename = if counter == 1 {
                format!("{}.md", base)
            } else {
                format!("{}-{}.md", base, counter)
            };
            let path = dir.join(filename);
            match std::fs::hard_link(pending.path(), &path) {
                Ok(()) => return Ok(path),
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(error) => return Err(error),
            }
        }
        Err(std::io::Error::new(
            std::io::ErrorKind::AlreadyExists,
            "no available report filename",
        ))
    }

    /// Atomically replace an existing report (e.g. after the full-recording
    /// verification pass upgrades the live report). Keeps 0600 permissions.
    pub fn rewrite(&self, path: &Path) -> Result<(), std::io::Error> {
        write_private(path, &self.render(&chrono_lite_date()))
    }

    /// Render the report as Minutes-compatible markdown.
    pub fn render(&self, date: &str) -> String {
        let mut content = String::new();

        // Quote every dynamic YAML string, including mapping keys. Apart from
        // punctuation/newlines this preserves values such as YES and dates in
        // both YAML 1.1 and 1.2 readers. Sort maps for reproducible reports.
        content.push_str("---\n");
        content.push_str(&format!("title: {}\n", yaml_string(&self.script_title)));
        content.push_str("type: consultation\n");
        if let Some(version) = &self.script_version {
            content.push_str(&format!("script_version: {}\n", yaml_string(version)));
        }
        content.push_str(&format!("date: {}\n", yaml_string(date)));
        content.push_str(&format!(
            "duration: {}\n",
            yaml_string(&self.duration_display())
        ));
        content.push_str("compliance:\n");
        content.push_str(&format!(
            "  sections_covered: [{}]\n",
            self.sections_covered
                .iter()
                .map(|name| yaml_string(name))
                .collect::<Vec<_>>()
                .join(", ")
        ));
        if !self.sections_skipped.is_empty() {
            content.push_str(&format!(
                "  sections_skipped: [{}]\n",
                self.sections_skipped
                    .iter()
                    .map(|name| yaml_string(name))
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
        }
        content.push_str(&format!("  adherence: {:.0}%\n", self.adherence_pct()));
        content.push_str(&format!(
            "  pause_points: {}/{}\n",
            self.pause_points_reached, self.pause_points_total
        ));
        if !self.section_times.is_empty() {
            content.push_str("  section_times:\n");
            for (name, secs) in self.section_times.iter().collect::<BTreeMap<_, _>>() {
                content.push_str(&format!(
                    "    {}: {}\n",
                    yaml_string(name),
                    yaml_string(&fmt_duration(*secs))
                ));
            }
        }
        if !self.branches_taken.is_empty() {
            content.push_str("  branches:\n");
            for (question, answer) in self.branches_taken.iter().collect::<BTreeMap<_, _>>() {
                content.push_str(&format!(
                    "    {}: {}\n",
                    yaml_string(question),
                    yaml_string(answer)
                ));
            }
        }
        content.push_str("---\n\n");

        // Body: summary
        content.push_str(&format!("# {}\n\n", self.script_title));
        content.push_str(&format!(
            "Consultation completed in {}. {} of {} sections covered ({:.0}% adherence).\n\n",
            self.duration_display(),
            self.sections_covered.len(),
            self.sections_covered.len() + self.sections_skipped.len(),
            self.adherence_pct()
        ));

        // Section breakdown
        content.push_str("## Sections\n\n");
        for name in &self.sections_covered {
            let time = self
                .section_times
                .get(name)
                .map(|s| fmt_duration(*s))
                .unwrap_or_default();
            content.push_str(&format!("- [x] {} ({})\n", name, time));
        }
        for name in &self.sections_skipped {
            content.push_str(&format!("- [ ] {} (skipped)\n", name));
        }

        // Delivery detail
        let d = &self.delivery;
        content.push_str("\n## Delivery\n\n");
        content.push_str(&format!(
            "Coverage source: {}.\n\n",
            if d.verified_by_recording {
                "full recording re-aligned against the script after the session"
            } else {
                "live tracking only"
            }
        ));
        if let Some(wpm) = d.speaking_wpm {
            content.push_str(&format!("- Speaking pace: {:.0} words per minute\n", wpm));
        }
        if let Some(secs) = d.patient_talk_secs {
            content.push_str(&format!(
                "- Patient speaking time: {}\n",
                fmt_duration(secs)
            ));
        }
        content.push_str(&format!("- Lines restarted: {}\n", d.repeats));
        content.push_str(&format!(
            "- Off-script stretches: {}\n",
            d.off_script_episodes
        ));
        if let Some(w) = d.off_script_words {
            content.push_str(&format!("- Off-script words (incl. patient): {}\n", w));
        }
        if !d.omitted_lines.is_empty() {
            content.push_str("\n### Lines not delivered\n\n");
            for line in &d.omitted_lines {
                content.push_str(&format!("- {}\n", line));
            }
        }

        content
    }
}

/// Write `content` to `path` with owner-only permissions on Unix (reports and
/// transcripts contain patient conversation). The replacement is atomic and
/// never follows a destination symlink or modifies an existing hard link.
pub fn write_private(path: &Path, content: &str) -> Result<(), std::io::Error> {
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let mut pending = PrivateFile::new(parent, content)?;
    std::fs::rename(pending.path(), path)?;
    pending.path = None;
    Ok(())
}

/// A fully written temporary file beside its destination. Creating it with
/// 0600, rather than chmod after writing, prevents an initial disclosure window.
/// Drop removes unpublished files on write/rename/link failure as well.
struct PrivateFile {
    path: Option<PathBuf>,
}

impl PrivateFile {
    fn new(dir: &Path, content: &str) -> Result<Self, std::io::Error> {
        static NEXT_FILE: AtomicU64 = AtomicU64::new(0);
        for _ in 0..1024 {
            let path = dir.join(format!(
                ".linecheck-{}-{}.tmp",
                std::process::id(),
                NEXT_FILE.fetch_add(1, Ordering::Relaxed)
            ));
            let mut options = std::fs::OpenOptions::new();
            options.write(true).create_new(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                options.mode(0o600);
            }
            let mut file = match options.open(&path) {
                Ok(file) => file,
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(error) => return Err(error),
            };
            let pending = Self { path: Some(path) };
            let result = file
                .write_all(content.as_bytes())
                .and_then(|()| file.sync_all());
            // Close before cleanup so failed writes can be removed on Windows.
            drop(file);
            result?;
            return Ok(pending);
        }
        Err(std::io::Error::new(
            std::io::ErrorKind::AlreadyExists,
            "could not create a private temporary report file",
        ))
    }

    fn path(&self) -> &Path {
        self.path
            .as_deref()
            .expect("temporary file is still pending")
    }
}

impl Drop for PrivateFile {
    fn drop(&mut self) {
        if let Some(path) = &self.path {
            let _ = std::fs::remove_file(path);
        }
    }
}

/// YAML's double-quoted style safely represents a string on one physical line.
/// Escape Unicode line separators as well as ASCII controls so a title cannot
/// terminate the frontmatter or change the shape of a report's metadata.
fn yaml_string(value: &str) -> String {
    let mut quoted = String::from("\"");
    for character in value.chars() {
        match character {
            '\\' => quoted.push_str("\\\\"),
            '"' => quoted.push_str("\\\""),
            '\n' => quoted.push_str("\\n"),
            '\r' => quoted.push_str("\\r"),
            '\t' => quoted.push_str("\\t"),
            character
                if character.is_control()
                    || matches!(character, '\u{2028}' | '\u{2029}' | '\u{fffe}' | '\u{ffff}') =>
            {
                quoted.push_str(&format!("\\u{:04X}", character as u32));
            }
            character => quoted.push(character),
        }
    }
    quoted.push('"');
    quoted
}

fn fmt_duration(secs: u64) -> String {
    let m = secs / 60;
    let s = secs % 60;
    format!("{}:{:02}", m, s)
}

fn slugify(s: &str) -> String {
    let mut slug = s
        .chars()
        .map(|c| {
            if c.is_alphanumeric() {
                c.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect::<String>()
        .split('-')
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join("-");
    // Leave room for the date, collision suffix and extension on filesystems
    // with a 255-byte filename limit, without cutting a Unicode code point.
    let mut end = slug.len().min(120);
    while !slug.is_char_boundary(end) {
        end -= 1;
    }
    slug.truncate(end);
    let slug = slug.trim_end_matches('-');
    if slug.is_empty() {
        "consultation".into()
    } else {
        slug.into()
    }
}

/// Simple date string without chrono dependency.
fn chrono_lite_date() -> String {
    // Use std::time for a basic YYYY-MM-DD
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();

    // Simple unix timestamp to date conversion
    let days = now / 86400;
    let mut y = 1970i64;
    let mut remaining = days as i64;

    loop {
        let days_in_year = if is_leap(y) { 366 } else { 365 };
        if remaining < days_in_year {
            break;
        }
        remaining -= days_in_year;
        y += 1;
    }

    let months_days: [i64; 12] = if is_leap(y) {
        [31, 29, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31]
    } else {
        [31, 28, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31]
    };

    let mut m = 1;
    for &md in &months_days {
        if remaining < md {
            break;
        }
        remaining -= md;
        m += 1;
    }
    let d = remaining + 1;

    format!("{:04}-{:02}-{:02}", y, m, d)
}

fn is_leap(y: i64) -> bool {
    (y % 4 == 0 && y % 100 != 0) || y % 400 == 0
}

#[cfg(test)]
mod tests {
    use super::*;

    struct TestDir(PathBuf);

    impl TestDir {
        fn new() -> Self {
            static NEXT_DIR: AtomicU64 = AtomicU64::new(0);
            let path = std::env::temp_dir().join(format!(
                "linecheck-compliance-test-{}-{}",
                std::process::id(),
                NEXT_DIR.fetch_add(1, Ordering::Relaxed)
            ));
            std::fs::create_dir(&path).unwrap();
            Self(path)
        }
    }

    impl Drop for TestDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn frontmatter(content: &str) -> serde_yaml::Value {
        let yaml = content
            .strip_prefix("---\n")
            .unwrap()
            .split_once("\n---\n")
            .unwrap()
            .0;
        serde_yaml::from_str(yaml).expect("valid YAML frontmatter")
    }

    #[test]
    fn adherence_calculation() {
        let report = ComplianceReport {
            script_title: "Test".into(),
            script_version: Some("1.0".into()),
            sections_covered: vec!["A".into(), "B".into()],
            sections_skipped: vec![],
            duration_secs: 120,
            section_times: HashMap::from([("A".into(), 60), ("B".into(), 60)]),
            pause_points_reached: 3,
            pause_points_total: 3,
            branches_taken: HashMap::new(),
            total_words: 1000,
            words_delivered: 850,
            ..Default::default()
        };

        assert_eq!(report.adherence_pct(), 85.0);
        assert_eq!(report.duration_display(), "2:00");
    }

    #[test]
    fn slugify_title() {
        assert_eq!(
            slugify("MTM Consultation — Jane Smith"),
            "mtm-consultation-jane-smith"
        );
        assert_eq!(slugify("Hello World!"), "hello-world");
        assert_eq!(slugify("!?!"), "consultation");
        assert_eq!(slugify(&"薬".repeat(100)).len(), 120);
    }

    #[test]
    fn write_report_to_temp_dir() {
        let dir = TestDir::new();

        let report = ComplianceReport {
            script_title: "Test Consultation".into(),
            script_version: Some("1.0".into()),
            sections_covered: vec!["Intro".into(), "Findings".into()],
            sections_skipped: vec!["Closing".into()],
            duration_secs: 300,
            section_times: HashMap::from([("Intro".into(), 120), ("Findings".into(), 180)]),
            pause_points_reached: 2,
            pause_points_total: 3,
            branches_taken: HashMap::from([("Question?".into(), "YES".into())]),
            total_words: 500,
            words_delivered: 400,
            ..Default::default()
        };

        let path = report.write_to_dir(&dir.0).expect("write failed");
        assert!(path.exists());

        let content = std::fs::read_to_string(&path).unwrap();
        assert_eq!(frontmatter(&content)["title"], "Test Consultation");
        assert!(content.contains("adherence: 80%"));
        assert!(content.contains("- [x] Intro"));
        assert!(content.contains("- [ ] Closing (skipped)"));
    }

    #[test]
    fn frontmatter_round_trips_arbitrary_report_strings() {
        let special = "Quotes: \"yes\", 'no', \\path\n---\n[one, two]: #薬\u{85}\u{2028}\0";
        let report = ComplianceReport {
            script_title: special.into(),
            script_version: Some(special.into()),
            sections_covered: vec![special.into(), "true".into(), "null".into(), "01".into()],
            sections_skipped: vec![special.into()],
            section_times: HashMap::from([(special.into(), 65), ("Uncovered".into(), 2)]),
            branches_taken: HashMap::from([(special.into(), special.into())]),
            ..Default::default()
        };
        let yaml = frontmatter(&report.render(special));
        assert_eq!(yaml["title"], special);
        assert_eq!(yaml["script_version"], special);
        assert_eq!(yaml["date"], special);
        assert_eq!(
            yaml["compliance"]["sections_covered"],
            serde_yaml::to_value(&report.sections_covered).unwrap()
        );
        assert_eq!(yaml["compliance"]["sections_skipped"][0], special);
        assert_eq!(yaml["compliance"]["section_times"][special], "1:05");
        assert_eq!(yaml["compliance"]["section_times"]["Uncovered"], "0:02");
        assert_eq!(yaml["compliance"]["branches"][special], special);
    }

    #[test]
    fn yaml_scalars_preserve_controls_and_ambiguous_types() {
        let controls: String = (0..=0x9f).filter_map(char::from_u32).collect();
        for value in [controls.as_str(), "YES", "null", "2026-01-01", "01", ""] {
            let quoted = yaml_string(value);
            assert!(quoted.starts_with('"') && quoted.ends_with('"'));
            assert_eq!(serde_yaml::from_str::<String>(&quoted).unwrap(), value);
        }
    }

    #[test]
    fn rendering_is_independent_of_hashmap_iteration_order() {
        let mut first = ComplianceReport::default();
        let mut second = ComplianceReport::default();
        for index in 0..40 {
            first
                .section_times
                .insert(format!("Section {}", index), index);
            first
                .branches_taken
                .insert(format!("Question {}?", index), "YES".into());
        }
        for index in (0..40).rev() {
            second
                .section_times
                .insert(format!("Section {}", index), index);
            second
                .branches_taken
                .insert(format!("Question {}?", index), "YES".into());
        }
        assert_eq!(first.render("2026-01-01"), second.render("2026-01-01"));
    }

    #[test]
    fn concurrent_reports_have_distinct_complete_files() {
        let dir = TestDir::new();
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(12));
        let handles: Vec<_> = (0..12)
            .map(|index| {
                let path = dir.0.clone();
                let barrier = barrier.clone();
                std::thread::spawn(move || {
                    let report = ComplianceReport {
                        script_title: "Same consultation".into(),
                        script_version: Some(index.to_string()),
                        ..Default::default()
                    };
                    barrier.wait();
                    let path = report.write_to_dir(&path).unwrap();
                    let content = std::fs::read_to_string(&path).unwrap();
                    assert_eq!(frontmatter(&content)["script_version"], index.to_string());
                    path
                })
            })
            .collect();
        let paths: std::collections::HashSet<_> = handles
            .into_iter()
            .map(|handle| handle.join().unwrap())
            .collect();
        assert_eq!(paths.len(), 12);
        assert_eq!(std::fs::read_dir(&dir.0).unwrap().count(), 12);
    }

    #[test]
    fn failed_replacement_preserves_destination_and_cleans_temporary_file() {
        let dir = TestDir::new();
        let destination = dir.0.join("existing-directory");
        std::fs::create_dir(&destination).unwrap();
        std::fs::write(destination.join("keep"), "original").unwrap();
        assert!(write_private(&destination, "replacement").is_err());
        assert_eq!(
            std::fs::read_to_string(destination.join("keep")).unwrap(),
            "original"
        );
        assert_eq!(std::fs::read_dir(&dir.0).unwrap().count(), 1);
    }

    #[cfg(unix)]
    #[test]
    fn private_files_and_new_directories_have_restricted_permissions() {
        use std::os::unix::fs::PermissionsExt;

        let dir = TestDir::new();
        let output = dir.0.join("reports").join("consultations");
        let report = ComplianceReport::default();
        let path = report.write_to_dir(&output).unwrap();
        for private_dir in [output.as_path(), output.parent().unwrap()] {
            assert_eq!(
                std::fs::metadata(private_dir).unwrap().permissions().mode() & 0o777,
                0o700
            );
        }
        assert_eq!(
            std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        report.rewrite(&path).unwrap();
        assert_eq!(
            std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );

        let pending = PrivateFile::new(&output, "patient conversation").unwrap();
        assert_eq!(
            std::fs::metadata(pending.path())
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
    }

    #[cfg(unix)]
    #[test]
    fn replacement_does_not_follow_symlinks_or_modify_other_hardlinks() {
        use std::os::unix::fs::symlink;

        let dir = TestDir::new();
        let original = dir.0.join("original");
        let symlink_path = dir.0.join("symlink");
        let hardlink_path = dir.0.join("hardlink");
        std::fs::write(&original, "original").unwrap();
        symlink(&original, &symlink_path).unwrap();
        std::fs::hard_link(&original, &hardlink_path).unwrap();
        write_private(&symlink_path, "new symlink path content").unwrap();
        write_private(&hardlink_path, "new hardlink path content").unwrap();
        assert_eq!(std::fs::read_to_string(&original).unwrap(), "original");
        assert_eq!(
            std::fs::read_to_string(&symlink_path).unwrap(),
            "new symlink path content"
        );
        assert_eq!(
            std::fs::read_to_string(&hardlink_path).unwrap(),
            "new hardlink path content"
        );
        assert!(!std::fs::symlink_metadata(&symlink_path)
            .unwrap()
            .file_type()
            .is_symlink());
    }

    #[cfg(unix)]
    #[test]
    fn report_collision_with_dangling_symlink_never_writes_its_target() {
        use std::os::unix::fs::symlink;

        let dir = TestDir::new();
        let report = ComplianceReport {
            script_title: "Collision".into(),
            ..Default::default()
        };
        let first_path = dir.0.join(format!("{}-collision.md", chrono_lite_date()));
        let target = dir.0.join("must-not-exist");
        symlink(&target, &first_path).unwrap();
        let path = report.write_to_dir(&dir.0).unwrap();
        assert_ne!(path, first_path);
        assert!(!target.exists());
        assert!(std::fs::symlink_metadata(first_path)
            .unwrap()
            .file_type()
            .is_symlink());
        assert_eq!(
            frontmatter(&std::fs::read_to_string(path).unwrap())["title"],
            "Collision"
        );
    }
}
