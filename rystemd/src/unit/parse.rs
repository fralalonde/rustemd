//! Low-level systemd unit-file syntax parsing, per `systemd.syntax(7)`.
//!
//! This module is deliberately *structural*: it turns a unit file into an
//! ordered list of `(section, key, value)` triples without interpreting any
//! directive. The typed interpretation happens in `unit/mod.rs`.
//!
//! Supported syntax:
//! - `[Section]` headers, `#`/`;` comments, blank lines
//! - `Key=Value` lines with whole-value single/double quoting and C-style
//!   escapes (`\n`, `\t`, `\x41`, `\"`, `\\`, ...)
//! - repeated keys preserved in order (consumers decide last-wins vs append)

use std::fmt;
use std::path::Path;

/// A parse error with the 1-based line number it occurred on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParseError {
    pub line: usize,
    pub msg: String,
}

impl fmt::Display for ParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "line {}: {}", self.line, self.msg)
    }
}
impl std::error::Error for ParseError {}

#[derive(Debug, Clone, Default)]
pub struct RawSection {
    pub name: String,
    /// `(key, raw value)` pairs in file order.
    pub entries: Vec<(String, String)>,
}

#[derive(Debug, Clone, Default)]
pub struct RawUnitFile {
    pub sections: Vec<RawSection>,
}

/// Convert the repository-owned parsed document into the semantic builder's
/// existing structural input without reading or parsing unit text again.
pub fn from_repository_document(document: crate::repo::UnitDocument) -> RawUnitFile {
    RawUnitFile {
        sections: document
            .sections
            .into_iter()
            .map(|section| RawSection {
                name: section.name,
                entries: section
                    .entries
                    .into_iter()
                    .map(|entry| (entry.key, entry.value))
                    .collect(),
            })
            .collect(),
    }
}

impl RawUnitFile {
    /// Iterate `(key, value)` pairs across all matching sections in order.
    pub fn entries<'a>(
        &'a self,
        section: &'a str,
    ) -> impl Iterator<Item = (&'a str, &'a str)> + 'a {
        self.sections
            .iter()
            .filter(move |s| s.name == section)
            .flat_map(|s| s.entries.iter())
            .map(|(k, v)| (k.as_str(), v.as_str()))
    }

    /// Last value for a scalar key in a section.
    pub fn scalar<'a>(&'a self, section: &'a str, key: &str) -> Option<&'a str> {
        self.entries(section)
            .filter(|(k, _)| *k == key)
            .map(|(_, v)| v)
            .last()
    }

    /// All values for a repeated key in a section.
    pub fn list<'a>(&'a self, section: &'a str, key: &str) -> Vec<&'a str> {
        self.entries(section)
            .filter(|(k, _)| *k == key)
            .map(|(_, v)| v)
            .collect()
    }
}

fn valid_key(key: &str) -> bool {
    !key.is_empty()
        && key
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-' || c == '.')
}

/// Parse unit-file text into raw sections.
pub fn parse(text: &str) -> Result<RawUnitFile, ParseError> {
    let mut file = RawUnitFile::default();
    let mut current: Option<usize> = None; // index into file.sections

    for (idx, raw) in text.lines().enumerate() {
        let line = idx + 1;
        let raw = raw.strip_suffix('\r').unwrap_or(raw);
        let trimmed = raw.trim();

        if trimmed.is_empty() {
            continue;
        }
        let first = trimmed.chars().next().unwrap();
        if first == '#' || first == ';' {
            continue;
        }

        if first == '[' {
            let close = trimmed.find(']').ok_or_else(|| ParseError {
                line,
                msg: "section header missing closing ']'".into(),
            })?;
            let name = trimmed[1..close].trim().to_string();
            // Anything after ']' that isn't whitespace is a syntax error.
            if trimmed[close + 1..].trim() != "" {
                return Err(ParseError {
                    line,
                    msg: format!("trailing garbage after section header `[{name}]`"),
                });
            }
            if name.is_empty() {
                return Err(ParseError {
                    line,
                    msg: "empty section name".into(),
                });
            }
            file.sections.push(RawSection {
                name,
                entries: Vec::new(),
            });
            current = Some(file.sections.len() - 1);
            continue;
        }

        if let Some(eq) = trimmed.find('=') {
            let key = trimmed[..eq].trim();
            if !valid_key(key) {
                return Err(ParseError {
                    line,
                    msg: format!("invalid key `{key}`"),
                });
            }
            let value = trimmed[eq + 1..].trim_start().to_string();
            match current {
                Some(sec) => file.sections[sec].entries.push((key.to_string(), value)),
                None => {
                    return Err(ParseError {
                        line,
                        msg: format!("`{key}=` appears before any [Section] header"),
                    });
                }
            }
            continue;
        }

        return Err(ParseError {
            line,
            msg: format!("invalid line `{trimmed}`"),
        });
    }

    Ok(file)
}

/// Read and parse a unit file from disk.
pub fn parse_file(path: &Path) -> Result<RawUnitFile, String> {
    let text =
        std::fs::read_to_string(path).map_err(|e| format!("can't read {}: {e}", path.display()))?;
    parse(&text).map_err(|e| format!("{}: {e}", path.display()))
}

/// C-style unescape: `\n \t \r \\ \" \' \a \b \f \v \s \xHH`.
/// Unknown escapes are preserved as backslash + char.
pub fn cunescape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        let Some(e) = chars.next() else {
            out.push('\\');
            break;
        };
        match e {
            'a' => out.push('\x07'),
            'b' => out.push('\x08'),
            'f' => out.push('\x0c'),
            'n' => out.push('\n'),
            'r' => out.push('\r'),
            't' => out.push('\t'),
            'v' => out.push('\x0b'),
            's' => out.push(' '),
            '\\' => out.push('\\'),
            '"' => out.push('"'),
            '\'' => out.push('\''),
            'x' => {
                let hex: String = chars.by_ref().take(2).collect();
                if hex.len() == 2 {
                    if let Ok(v) = u8::from_str_radix(&hex, 16) {
                        out.push(char::from(v));
                    } else {
                        out.push_str("\\x");
                        out.push_str(&hex);
                    }
                } else {
                    out.push_str("\\x");
                    out.push_str(&hex);
                }
            }
            other => {
                out.push('\\');
                out.push(other);
            }
        }
    }
    out
}

/// Interpret a scalar directive value: strip one level of whole-value
/// quoting, then C-unescape. Used for `Description`, `WorkingDirectory`,
/// `User`, and similar single-string directives.
pub fn unquote_scalar(raw: &str) -> Result<String, String> {
    if let Some(inner) = strip_whole_quotes(raw) {
        return Ok(inner);
    }
    Ok(cunescape(raw))
}

/// If the entire raw value is wrapped in a single pair of matching quotes,
/// return the inner (un-escaped) text.
fn strip_whole_quotes(raw: &str) -> Option<String> {
    let b = raw.as_bytes();
    if b.len() >= 2 && b[0] == b'"' && b[b.len() - 1] == b'"' {
        Some(cunescape(&raw[1..raw.len() - 1]))
    } else if b.len() >= 2 && b[0] == b'\'' && b[b.len() - 1] == b'\'' {
        Some(raw[1..raw.len() - 1].to_string())
    } else {
        None
    }
}

/// Split a value into words the way systemd's `extract_first_word` does for
/// list-ish directives (`ExecStart`, `Environment`, `EnvironmentFile`).
///
/// - unquoted whitespace separates words
/// - `'...'` groups literally (no escapes)
/// - `"..."` groups with C escapes processed
/// - backslash escapes outside quotes are processed (`\ ` -> space, `\\`, ...)
///
/// Returns an error on unbalanced quotes. An empty result (no words) is
/// allowed for some directives; callers decide.
pub fn tokenize(raw: &str) -> Result<Vec<String>, String> {
    let mut words: Vec<String> = Vec::new();
    let mut cur = String::new();
    let mut chars = raw.chars().peekable();

    enum Mode {
        Unquoted,
        Single,
        Double,
    }
    let mut mode = Mode::Unquoted;

    while let Some(c) = chars.next() {
        match mode {
            Mode::Unquoted => match c {
                '\'' => mode = Mode::Single,
                '"' => mode = Mode::Double,
                c if c.is_whitespace() => {
                    if !cur.is_empty() {
                        words.push(std::mem::take(&mut cur));
                    }
                }
                '\\' => {
                    // Escape next char (or the verbatim '\' at end).
                    match chars.next() {
                        Some(e) => cur.push(unescape_one(e)),
                        None => cur.push('\\'),
                    }
                }
                other => cur.push(other),
            },
            Mode::Single => match c {
                '\'' => mode = Mode::Unquoted,
                other => cur.push(other),
            },
            Mode::Double => match c {
                '"' => mode = Mode::Unquoted,
                '\\' => match chars.next() {
                    Some(e) => cur.push(unescape_one(e)),
                    None => cur.push('\\'),
                },
                other => cur.push(other),
            },
        }
    }

    match mode {
        Mode::Single => return Err("unbalanced single quote".into()),
        Mode::Double => return Err("unbalanced double quote".into()),
        Mode::Unquoted => {}
    }
    if !cur.is_empty() {
        words.push(cur);
    }
    Ok(words)
}

/// Single-char escape used inside `tokenize`.
fn unescape_one(c: char) -> char {
    match c {
        'a' => '\x07',
        'b' => '\x08',
        'f' => '\x0c',
        'n' => '\n',
        'r' => '\r',
        't' => '\t',
        'v' => '\x0b',
        's' => ' ',
        other => other, // \\ \" \' etc. map to themselves
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_repository_document_without_reparsing_text() {
        let document = crate::repo::UnitDocument {
            sections: vec![crate::repo::UnitSection {
                name: "Unit".into(),
                entries: vec![crate::repo::UnitEntry {
                    key: "Description".into(),
                    value: "typed".into(),
                }],
            }],
        };
        let raw = from_repository_document(document);
        assert_eq!(raw.scalar("Unit", "Description"), Some("typed"));
    }

    #[test]
    fn sections_and_keys() {
        let f = parse("[Unit]\nDescription=hi\n[A]\nX=1\nX=2\n").unwrap();
        assert_eq!(f.scalar("Unit", "Description"), Some("hi"));
        assert_eq!(f.list("A", "X"), vec!["1", "2"]);
    }

    #[test]
    fn comments_and_blanks() {
        let f = parse("# c1\n; c2\n\n[Unit]\n  # indent\n  Description=ok\n").unwrap();
        assert_eq!(f.scalar("Unit", "Description"), Some("ok"));
    }

    #[test]
    fn value_whitespace_trimming() {
        let f = parse("[Unit]\nA =  value with spaces  \n").unwrap();
        // key trimmed; leading and trailing value whitespace trimmed (unquoted)
        assert_eq!(f.scalar("Unit", "A"), Some("value with spaces"));
    }

    #[test]
    fn quoted_scalar() {
        let f = parse("[Unit]\nDescription=\"hello world\"\nB='lit \\n'\n").unwrap();
        assert_eq!(
            unquote_scalar(f.scalar("Unit", "Description").unwrap()).unwrap(),
            "hello world"
        );
        assert_eq!(
            unquote_scalar(f.scalar("Unit", "B").unwrap()).unwrap(),
            "lit \\n"
        );
    }

    #[test]
    fn cunescape_basic() {
        assert_eq!(cunescape(r"a\nb"), "a\nb");
        assert_eq!(cunescape(r"\t"), "\t");
        assert_eq!(cunescape(r"\x41"), "A");
        assert_eq!(cunescape(r"\\"), "\\");
        assert_eq!(cunescape(r"a\qb"), "a\\qb"); // unknown escape preserved
    }

    #[test]
    fn errors() {
        assert!(parse("[Unit\nX=1").is_err());
        assert!(parse("X=1").is_err()); // before any section
        assert!(parse("[Unit]\nbad key=1").is_err());
        assert!(parse("[Unit]\n=noval").is_err());
        assert!(parse("[Unit]\nnot a kv line").is_err());
    }

    #[test]
    fn tokenize_words() {
        assert_eq!(
            tokenize("/bin/foo --bar 'x y'").unwrap(),
            vec!["/bin/foo", "--bar", "x y"]
        );
        assert_eq!(tokenize("a \"b c\" d").unwrap(), vec!["a", "b c", "d"]);
        assert_eq!(tokenize(r"a\ b").unwrap(), vec!["a b"]);
        assert_eq!(tokenize(r##"\"quoted\""##).unwrap(), vec!["\"quoted\""]);
    }

    #[test]
    fn tokenize_errors() {
        assert!(tokenize("a 'b").is_err());
        assert!(tokenize("a \"b").is_err());
    }
}
