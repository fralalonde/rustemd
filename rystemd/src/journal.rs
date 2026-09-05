//! Persistent per-unit journal — the disk-backed side of rystemd logging.
//!
//! Modeled on syslog-ng's split between an in-memory ring (what `status`
//! shows) and a durable store (what `journalctl` shows): a [`Journal`] owns
//! the disk store, receives each captured output record, appends it to a
//! size-rotated file, and can be read back (with a `--since` filter) by the
//! CLI.
//!
//! Layout: one active file per unit. When it exceeds `max_segment_bytes` it
//! rotates to a numbered segment (syslog-ng `file { size() }` style), older
//! segments shift up, and the one beyond `max_segments` is dropped.
//!
//! Record format (one per line, `ts` = UNIX seconds):
//!   `<ts>\t<unit>\t<text>`
//! Duration/rotation give bounded disk use; the timestamp supports `--since`.

use std::fs::{self, File, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// A durable journal store for all units, under one directory.
pub struct Journal {
    dir: PathBuf,
    max_segment_bytes: u64,
    max_segments: usize,
}

/// One record read back from the store: `(unix_secs, unit, text)`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct JournalRecord {
    pub secs: u64,
    pub unit: String,
    pub text: String,
}

impl Journal {
    pub fn new(dir: PathBuf, max_segment_bytes: u64, max_segments: usize) -> Self {
        Journal {
            dir,
            max_segment_bytes,
            max_segments,
        }
    }

    /// Segment files for `unit`, oldest first (`.N` … `.1`, then active).
    fn segment_paths(&self, unit: &str) -> Vec<PathBuf> {
        let mut v = Vec::new();
        for i in (1..=self.max_segments).rev() {
            v.push(self.dir.join(format!("{unit}.{i}")));
        }
        v.push(self.dir.join(unit));
        v
    }

    /// Move `.1 → .2`, drop the tail, rename the active file to `.1`.
    fn rotate(&self, unit: &str) {
        let active = self.dir.join(unit);
        for i in (1..=self.max_segments).rev() {
            if i == self.max_segments {
                let _ = fs::remove_file(self.dir.join(format!("{unit}.{i}")));
            } else {
                let _ = fs::rename(
                    self.dir.join(format!("{unit}.{}", i + 1)),
                    self.dir.join(format!("{unit}.{i}")),
                );
            }
        }
        let _ = fs::rename(&active, self.dir.join(format!("{unit}.1")));
    }

    /// Append one line for `unit`, rotating first if the active file is at
    /// the size cap. Best-effort — disk errors drop the line rather than take
    /// down the manager.
    pub fn append(&mut self, unit: &str, unix_secs: u64, text: &str) {
        if !crate::names::is_plain_unit_name(unit) {
            return;
        }
        if !self.dir.is_dir() && fs::create_dir_all(&self.dir).is_err() {
            return;
        }
        let active = self.dir.join(unit);
        let reached_cap = active
            .metadata()
            .map(|m| m.len() >= self.max_segment_bytes)
            .unwrap_or(false);
        if reached_cap {
            self.rotate(unit);
        }
        let line = format!("{unix_secs}\t{unit}\t{}\n", text.replace('\n', " "));
        if let Ok(mut f) = OpenOptions::new().create(true).append(true).open(&active) {
            let _ = f.write_all(line.as_bytes());
        }
    }

    /// Read records for `unit` (all segments, oldest first), optionally
    /// filtered to `since_unix` or later.
    pub fn read(&self, unit: &str, since_unix: Option<u64>) -> Vec<JournalRecord> {
        if !crate::names::is_plain_unit_name(unit) {
            return Vec::new();
        }
        let mut out = Vec::new();
        for path in self.segment_paths(unit) {
            let Ok(f) = File::open(&path) else {
                continue;
            };
            for line in BufReader::new(f).lines().map_while(Result::ok) {
                if let Some(rec) = parse_record(&line)
                    && since_unix.is_none_or(|s| rec.secs >= s)
                {
                    out.push(rec);
                }
            }
        }
        out
    }

    /// Read the most recent `n` records for `unit`.
    pub fn tail(&self, unit: &str, n: usize) -> Vec<JournalRecord> {
        let all = self.read(unit, None);
        all.into_iter()
            .rev()
            .take(n)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect()
    }

    /// Whether the active file for `unit` exists (used to gate `-f`).
    pub fn exists(&self, unit: &str) -> bool {
        crate::names::is_plain_unit_name(unit) && self.dir.join(unit).exists()
    }

    /// Unit names that have journal data (active or rotated). A file ending in
    /// a numeric `.N` segment suffix is folded back to its unit name.
    pub fn units(&self) -> Vec<String> {
        let Ok(rd) = fs::read_dir(&self.dir) else {
            return Vec::new();
        };
        let mut names = std::collections::BTreeSet::new();
        for entry in rd.flatten() {
            let Some(name) = entry.file_name().to_str().map(str::to_string) else {
                continue;
            };
            let unit = match name.rsplit_once('.') {
                Some((stem, num)) if num.parse::<usize>().is_ok() => stem.to_string(),
                _ => name,
            };
            names.insert(unit);
        }
        names.into_iter().collect()
    }

    /// The journal directory (for tests / reporting).
    pub fn dir(&self) -> &Path {
        &self.dir
    }
}

/// Parse one on-disk `<ts>\t<unit>\t<text>` line.
fn parse_record(line: &str) -> Option<JournalRecord> {
    let mut parts = line.splitn(3, '\t');
    let ts = parts.next()?.parse::<u64>().ok()?;
    let unit = parts.next()?.to_string();
    let text = parts.next().unwrap_or("").to_string();
    Some(JournalRecord {
        secs: ts,
        unit,
        text,
    })
}

/// Timestamps are the caller's responsibility; this is the manager's clock.
pub fn timestamp_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmpdir(tag: &str) -> PathBuf {
        std::env::temp_dir().join(format!("rystemd-journal-{}-{}", std::process::id(), tag))
    }

    #[test]
    fn appends_and_reads_back() {
        let dir = tmpdir("append");
        let _ = fs::remove_dir_all(&dir);
        let mut j = Journal::new(dir.clone(), 10_000, 4);
        j.append("u", 1000, "hello");
        j.append("u", 1001, "world");
        let all = j.read("u", None);
        assert_eq!(all.len(), 2);
        assert_eq!(all[0].secs, 1000);
        assert_eq!(all[0].text, "hello");
        assert_eq!(all[1].unit, "u");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn read_rejects_unit_path_traversal() {
        let root = tmpdir("read-traversal");
        let dir = root.join("journal");
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&dir).unwrap();
        fs::write(
            root.join("outside.service"),
            "1000\toutside.service\tescaped\n",
        )
        .unwrap();
        let j = Journal::new(dir, 10_000, 4);

        assert!(j.read("../outside.service", None).is_empty());
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn append_rejects_unit_path_traversal() {
        let root = tmpdir("append-traversal");
        let dir = root.join("journal");
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&dir).unwrap();
        let mut j = Journal::new(dir, 10_000, 4);

        j.append("../outside.service", 1000, "escaped");

        assert!(!root.join("outside.service").exists());
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn exists_rejects_unit_path_traversal() {
        let root = tmpdir("exists-traversal");
        let dir = root.join("journal");
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&dir).unwrap();
        fs::write(root.join("outside.service"), "x").unwrap();
        let j = Journal::new(dir, 10_000, 4);

        assert!(!j.exists("../outside.service"));
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn since_filters_and_tail_works() {
        let dir = tmpdir("since");
        let _ = fs::remove_dir_all(&dir);
        let mut j = Journal::new(dir.clone(), 10_000, 4);
        for i in 0..10u64 {
            j.append("u", 1000 + i, &format!("line{i}"));
        }
        let since = j.read("u", Some(1005));
        assert_eq!(since.len(), 5);
        assert_eq!(since[0].secs, 1005);
        let tail = j.tail("u", 3);
        assert_eq!(tail.len(), 3);
        assert_eq!(tail[0].text, "line7");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn rotates_segments_by_size() {
        let dir = tmpdir("rotate");
        let _ = fs::remove_dir_all(&dir);
        let mut j = Journal::new(dir.clone(), 40, 4);
        for i in 0..12u64 {
            j.append("u", 1000 + i, &format!("line{i}"));
        }
        // Rotation must have produced segments; total records across segments
        // stays <= max_segments buckets worth, and the active file is present.
        let all = j.read("u", None);
        assert!(!all.is_empty(), "rotation should keep messages");
        assert!(dir.join("u").exists(), "active file exists");
        assert!(dir.join("u.1").exists(), "a rotated segment exists");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn record_line_survives_newlines() {
        let dir = tmpdir("nl");
        let _ = fs::remove_dir_all(&dir);
        let mut j = Journal::new(dir.clone(), 10_000, 4);
        j.append("u", 1, "a\nb\nc");
        let all = j.read("u", None);
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].text, "a b c");
        let _ = fs::remove_dir_all(&dir);
    }
}
