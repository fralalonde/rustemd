//! systemd time-span parsing, per `systemd.time(7)`.
//!
//! Examples: `5s`, `500ms`, `1h 30min`, `2d 4h`, `infinity`.
//! Months and years are not fixed-length: 1 month = 365.25/12 days,
//! 1 year = 365.25 days (same constants as systemd's `time-util.c`).

use std::fmt;
use std::time::Duration;

pub const INFINITY_USEC: u64 = u64::MAX;

const USEC_PER_SEC: u64 = 1_000_000;
const USEC_PER_MIN: u64 = 60 * USEC_PER_SEC;
const USEC_PER_HOUR: u64 = 60 * USEC_PER_MIN;
const USEC_PER_DAY: u64 = 24 * USEC_PER_HOUR;
const USEC_PER_WEEK: u64 = 7 * USEC_PER_DAY;
const USEC_PER_MONTH: u64 = (USEC_PER_DAY as u128 * 36525 / 12 / 100) as u64; // 30.4375 d
const USEC_PER_YEAR: u64 = (USEC_PER_DAY as u128 * 36525 / 100) as u64; // 365.25 d

/// A parsed systemd time span, in microseconds. `u64::MAX` = `infinity`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub struct TimeSpan {
    pub usec: u64,
}

impl TimeSpan {
    pub const ZERO: TimeSpan = TimeSpan { usec: 0 };
    pub const INFINITY: TimeSpan = TimeSpan {
        usec: INFINITY_USEC,
    };

    pub fn from_usec(usec: u64) -> Self {
        TimeSpan { usec }
    }
    pub fn from_duration(d: Duration) -> Self {
        TimeSpan {
            usec: d.as_secs().saturating_mul(USEC_PER_SEC) + u64::from(d.subsec_micros()),
        }
    }

    pub fn is_infinite(&self) -> bool {
        self.usec == INFINITY_USEC
    }

    /// Finite duration, or `None` for `infinity`.
    pub fn as_duration(&self) -> Option<Duration> {
        if self.is_infinite() {
            None
        } else {
            Some(Duration::from_micros(self.usec))
        }
    }

    /// Parse a systemd time span. `infinity` (case-insensitive) maps to
    /// `TimeSpan::INFINITY`. Multiple space-separated `N unit` pairs are
    /// summed. Unit names are case-insensitive.
    pub fn parse(s: &str) -> Result<TimeSpan, String> {
        let s = s.trim();
        if s.eq_ignore_ascii_case("infinity") {
            return Ok(TimeSpan::INFINITY);
        }
        if s.is_empty() {
            return Err("empty time span".into());
        }

        let chars: Vec<char> = s.chars().collect();
        let mut total: u128 = 0;
        let mut i = 0usize;
        while i < chars.len() {
            // Skip whitespace between components.
            while i < chars.len() && chars[i].is_whitespace() {
                i += 1;
            }
            if i >= chars.len() {
                break;
            }
            // Number (integer or decimal), then a unit name.
            let mut num = String::new();
            while i < chars.len() && (chars[i].is_ascii_digit() || chars[i] == '.') {
                num.push(chars[i]);
                i += 1;
            }
            if num.is_empty() || num == "." || num.ends_with('.') {
                return Err(format!("missing number in time span `{s}`"));
            }
            let value: f64 = num
                .parse()
                .map_err(|_| format!("invalid number `{num}` in time span `{s}`"))?;
            let mut unit = String::new();
            while i < chars.len() && chars[i].is_ascii_alphabetic() {
                unit.push(chars[i]);
                i += 1;
            }
            if unit.is_empty() {
                return Err(format!("missing unit after `{num}` in time span `{s}`"));
            }
            let factor = unit_factor(&unit)
                .ok_or_else(|| format!("invalid time unit `{unit}` in time span `{s}`"))?;
            total += (value * factor as f64) as u128;
            if total > u128::from(INFINITY_USEC) {
                return Err(format!("time span `{s}` overflows"));
            }
        }
        Ok(TimeSpan { usec: total as u64 })
    }
}

fn unit_factor(unit: &str) -> Option<u64> {
    // systemd accepts both short and long unit names, case-insensitively.
    // Note: in *time spans*, `m` means minutes (unlike calendar specs).
    match unit.to_ascii_lowercase().as_str() {
        "usec" | "us" | "µs" => Some(1),
        "msec" | "ms" => Some(1_000),
        "seconds" | "second" | "sec" | "s" => Some(USEC_PER_SEC),
        "minutes" | "minute" | "min" | "m" => Some(USEC_PER_MIN),
        "hours" | "hour" | "hr" | "h" => Some(USEC_PER_HOUR),
        "days" | "day" | "d" => Some(USEC_PER_DAY),
        "weeks" | "week" | "w" => Some(USEC_PER_WEEK),
        "months" | "month" | "mon" | "M" => Some(USEC_PER_MONTH),
        "years" | "year" | "y" => Some(USEC_PER_YEAR),
        _ => None,
    }
}

/// Human-readable duration for the `LEFT` column of `list-timers`:
/// `23s`, `5min`, `1h 30min`, `2 days`.
pub fn fmt_left(d: Duration) -> String {
    let secs = d.as_secs();
    if secs >= 86_400 {
        let days = secs / 86_400;
        format!("{days} day{}", if days == 1 { "" } else { "s" })
    } else if secs >= 3600 {
        let h = secs / 3600;
        let m = (secs % 3600) / 60;
        if m == 0 {
            format!("{h}h")
        } else {
            format!("{h}h {m}min")
        }
    } else if secs >= 60 {
        format!("{}min", secs / 60)
    } else {
        format!("{secs}s")
    }
}

/// Human-readable elapsed time for the `PASSED` column of `list-timers`:
/// `23s ago`, `5min ago`, `2h ago`, `3 days ago`.
pub fn fmt_ago(d: Duration) -> String {
    let secs = d.as_secs();
    if secs >= 86_400 {
        let days = secs / 86_400;
        format!("{days} day{} ago", if days == 1 { "" } else { "s" })
    } else if secs >= 3600 {
        let h = secs / 3600;
        let m = (secs % 3600) / 60;
        if m == 0 {
            format!("{h}h ago")
        } else {
            format!("{h}h {m}min ago")
        }
    } else if secs >= 60 {
        format!("{}min ago", secs / 60)
    } else {
        format!("{secs}s ago")
    }
}

impl fmt::Display for TimeSpan {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.is_infinite() {
            write!(f, "infinity")
        } else if let Some(d) = self.as_duration() {
            write!(f, "{}", fmt_left(d))
        } else {
            write!(f, "?")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn us(s: &str) -> u64 {
        TimeSpan::parse(s).unwrap().usec
    }

    #[test]
    fn basic_units() {
        assert_eq!(us("5s"), 5_000_000);
        assert_eq!(us("500ms"), 500_000);
        assert_eq!(us("1min"), 60_000_000);
        assert_eq!(us("1m"), 60_000_000); // minutes in time spans
        assert_eq!(us("1h"), 3_600_000_000);
        assert_eq!(us("2d"), 2 * 86_400_000_000);
        assert_eq!(us("1w"), 7 * 86_400_000_000);
    }

    #[test]
    fn long_names_and_case() {
        assert_eq!(us("1second"), 1_000_000);
        assert_eq!(us("2seconds"), 2_000_000);
        assert_eq!(us("1MINUTE"), 60_000_000);
        assert_eq!(us("1Hour"), 3_600_000_000);
        assert_eq!(us("1day"), 86_400_000_000);
        assert_eq!(us("1MON"), 30 * 86_400_000_000 + 43_750 * 864_000);
    }

    #[test]
    fn combined() {
        assert_eq!(us("1h 30min"), 90 * 60_000_000);
        assert_eq!(us("1h30min"), 90 * 60_000_000);
        assert_eq!(
            us("2d 4h 5s"),
            2 * 86_400_000_000 + 4 * 3_600_000_000 + 5_000_000
        );
        assert_eq!(us("90s"), 90_000_000);
    }

    #[test]
    fn infinity_and_errors() {
        assert!(TimeSpan::parse("infinity").unwrap().is_infinite());
        assert!(TimeSpan::parse("Infinity").unwrap().is_infinite());
        assert!(TimeSpan::parse("").is_err());
        assert!(TimeSpan::parse("5").is_err()); // bare number, no unit
        assert_eq!(TimeSpan::parse("1.5s").unwrap().usec, 1_500_000); // fractional
        assert!(TimeSpan::parse("banana").is_err());
        assert!(TimeSpan::parse("5 parsecs").is_err());
    }

    #[test]
    fn month_year_lengths() {
        // 1 month = 365.25/12 days = 30.4375 d; 1 year = 365.25 d.
        let month = us("1mon");
        let year = us("1y");
        assert_eq!(month, 30 * 86_400_000_000 + 43_750 * 864_000);
        assert_eq!(year, 365 * 86_400_000_000 + 21_600_000_000);
        assert_eq!(year % 12, 0); // 365.25 d divisible by 12 -> 30.4375 d
        assert_eq!(month * 12, year);
    }

    #[test]
    fn display() {
        assert_eq!(fmt_left(Duration::from_secs(23)), "23s");
        assert_eq!(fmt_left(Duration::from_secs(300)), "5min");
        assert_eq!(fmt_left(Duration::from_secs(5400)), "1h 30min");
        assert_eq!(fmt_left(Duration::from_secs(3600)), "1h");
        assert_eq!(fmt_left(Duration::from_secs(172800)), "2 days");
        assert_eq!(fmt_ago(Duration::from_secs(300)), "5min ago");
        assert_eq!(fmt_ago(Duration::from_secs(86400)), "1 day ago");
    }
}
