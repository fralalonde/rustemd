//! Logging: a per-unit ring buffer (captured child output, shown by
//! `status`) plus a simple timestamped manager logger to stderr.

use std::collections::VecDeque;
use std::time::{SystemTime, UNIX_EPOCH};

/// Bounded, in-memory log ring for one unit's captured output.
#[derive(Debug, Clone, Default)]
pub struct LogRing {
    max_lines: usize,
    lines: VecDeque<String>,
}

impl LogRing {
    pub fn new(max_lines: usize) -> Self {
        LogRing {
            max_lines,
            lines: VecDeque::with_capacity(max_lines.min(64)),
        }
    }

    pub fn push(&mut self, line: String) {
        if self.max_lines == 0 {
            return;
        }
        if self.lines.len() >= self.max_lines {
            self.lines.pop_front();
        }
        self.lines.push_back(line);
    }

    /// Append a chunk (which may contain newlines), splitting into lines.
    pub fn push_chunk(&mut self, chunk: &str) {
        for line in chunk.split_inclusive('\n') {
            self.push(line.trim_end_matches('\n').to_string());
        }
    }

    pub fn lines(&self) -> impl Iterator<Item = &str> {
        self.lines.iter().map(String::as_str)
    }

    /// Buffer + borrow-free snapshot.
    pub fn snapshot(&self) -> Vec<String> {
        self.lines.iter().cloned().collect()
    }
}

/// Write a timestamped manager log line to stderr.
pub fn mgr_log(msg: &str) {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    eprintln!("[{now}] {msg}");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ring_caps() {
        let mut r = LogRing::new(3);
        r.push("a".into());
        r.push("b".into());
        r.push("c".into());
        r.push("d".into());
        assert_eq!(r.snapshot(), vec!["b", "c", "d"]);
    }

    #[test]
    fn chunk_split() {
        let mut r = LogRing::new(8);
        r.push_chunk("one\ntwo\nthree\n");
        assert_eq!(r.snapshot(), vec!["one", "two", "three"]);
        r.push_chunk("four");
        assert_eq!(r.snapshot(), vec!["one", "two", "three", "four"]);
    }
}
