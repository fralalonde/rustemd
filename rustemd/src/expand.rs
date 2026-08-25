//! `$VAR` / `${VAR}` argv-token expansion, shared across platforms.
//!
//! Both the Unix and Windows spawn paths expand environment references in
//! `ExecStart=` argument vectors before exec. The rules are identical on every
//! platform, so the logic lives here exactly once; `platform::process` (Unix)
//! and `platform::windows::process` re-export it.

use std::collections::HashMap;

/// Expand `$VAR`/`${VAR}` in each argv token against `env`.
pub fn expand_env_argv(argv: &[String], env: &HashMap<String, String>) -> Vec<String> {
    argv.iter().map(|t| expand_env_token(t, env)).collect()
}

/// Expand `$VAR` and `${VAR}` in a single argv token against `env`.
/// Unset variables expand to the empty string.
pub fn expand_env_token(tok: &str, env: &HashMap<String, String>) -> String {
    let mut out = String::with_capacity(tok.len());
    let bytes = tok.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'$' {
            let rest = &tok[i + 1..];
            let (name, consumed) = if let Some(braced) = rest.strip_prefix('{') {
                match braced.find('}') {
                    // `consumed` advances `i` (which points at `$`) past the
                    // whole `${name}` sequence: `$` + `{` + name + `}`.
                    Some(end) => (&braced[..end], end + 3),
                    None => ("", rest.len() + 1),
                }
            } else {
                let end = rest
                    .find(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
                    .unwrap_or(rest.len());
                (&rest[..end], end + 1)
            };
            if name.is_empty() {
                out.push('$');
                out.push_str(rest);
                break;
            }
            out.push_str(env.get(name).map(String::as_str).unwrap_or(""));
            i += consumed;
            continue;
        }
        let ch_len = utf8_len(bytes[i]);
        out.push_str(&tok[i..i + ch_len]);
        i += ch_len;
    }
    out
}

fn utf8_len(b: u8) -> usize {
    if b < 0x80 {
        1
    } else if b >> 5 == 0b110 {
        2
    } else if b >> 4 == 0b1110 {
        3
    } else if b >> 3 == 0b11110 {
        4
    } else {
        1
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    #[test]
    fn expands_simple_and_braced() {
        let e = env(&[("HOME", "/home/me"), ("X", "y")]);
        assert_eq!(expand_env_token("$HOME", &e), "/home/me");
        assert_eq!(expand_env_token("${HOME}", &e), "/home/me");
        assert_eq!(expand_env_token("${X}", &e), "y");
    }

    #[test]
    fn unset_variable_expands_empty() {
        let e = env(&[("HOME", "/home/me")]);
        assert_eq!(expand_env_token("$MISSING", &e), "");
        assert_eq!(expand_env_token("${MISSING}", &e), "");
    }

    #[test]
    fn preserves_non_variable_dollar_sequences() {
        let e = env(&[]);
        assert_eq!(expand_env_token("$", &e), "$");
        assert_eq!(expand_env_token("$$", &e), "$$");
        assert_eq!(expand_env_token("a$!b", &e), "a$!b");
        assert_eq!(expand_env_token("${unclosed", &e), "${unclosed");
    }

    #[test]
    fn expands_inline_with_surrounding_text() {
        let e = env(&[("FOO", "x")]);
        assert_eq!(expand_env_token("pre$FOO/post", &e), "prex/post");
        assert_eq!(expand_env_token("${FOO}bar", &e), "xbar");
    }

    #[test]
    fn var_name_stops_at_non_alphanumeric() {
        let e = env(&[("A", "1"), ("A_B", "2")]);
        assert_eq!(expand_env_token("$A_B", &e), "2");
        assert_eq!(expand_env_token("$A.b", &e), "1.b");
    }

    #[test]
    fn multibyte_utf8_passthrough() {
        let e = env(&[]);
        assert_eq!(expand_env_token("héllo→world", &e), "héllo→world");
    }

    #[test]
    fn expand_argv_applies_to_every_token() {
        let e = env(&[("D", "/d")]);
        assert_eq!(
            expand_env_argv(&["$D".into(), "plain".into()], &e),
            vec!["/d", "plain"]
        );
    }
}
