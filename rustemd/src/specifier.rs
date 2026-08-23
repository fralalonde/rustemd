//! systemd unit-file specifier (`%`) expansion, per `systemd.unit(5)`.
//!
//! Specifiers are expanded when a unit file is *loaded* (before the argv
//! is split and before environment variables are substituted at exec time).
//! Unknown specifiers are left untouched, matching systemd's forgiving
//! behaviour.

/// Everything needed to expand specifiers for one unit.
#[derive(Debug, Clone)]
pub struct SpecifierContext {
    /// Full unit name, e.g. `foo@bar.service`.
    pub unit_name: String,
    /// Runtime directory (`%t`): `/run` for system, `$XDG_RUNTIME_DIR` for user.
    pub runtime_dir: String,
    /// User name (`%u`).
    pub user_name: String,
    /// UID as decimal (`%U`).
    pub uid: String,
    /// Home directory (`%h`).
    pub home: String,
    /// Hostname (`%H`).
    pub hostname: String,
    /// Machine ID (`%m`), hex; `unknown` if not available.
    pub machine_id: String,
}

impl SpecifierContext {
    /// The unit prefix (`%p`): component before `@`, or before the type suffix.
    pub fn prefix(&self) -> &str {
        let name = self.unit_name.as_str();
        if let Some(at) = name.find('@') {
            &name[..at]
        } else {
            name.split('.').next().unwrap_or(name)
        }
    }

    /// The instance name (`%i`): text between `@` and the type suffix,
    /// empty when the unit is not templated.
    pub fn instance(&self) -> &str {
        let name = self.unit_name.as_str();
        let Some(at) = name.find('@') else {
            return "";
        };
        let rest = &name[at + 1..];
        match rest.find('.') {
            Some(dot) => &rest[..dot],
            None => "",
        }
    }

    /// Expand all specifiers in `s`.
    pub fn expand(&self, s: &str) -> String {
        let mut out = String::with_capacity(s.len());
        let mut chars = s.chars();
        while let Some(c) = chars.next() {
            if c != '%' {
                out.push(c);
                continue;
            }
            let Some(spec) = chars.next() else {
                out.push('%');
                break;
            };
            match spec {
                '%' => out.push('%'),
                'n' | 'N' => out.push_str(&self.unit_name),
                'p' | 'P' => out.push_str(self.prefix()),
                'i' | 'I' => out.push_str(self.instance()),
                't' => out.push_str(&self.runtime_dir),
                'u' => out.push_str(&self.user_name),
                'U' => out.push_str(&self.uid),
                'h' => out.push_str(&self.home),
                'H' => out.push_str(&self.hostname),
                'm' => out.push_str(&self.machine_id),
                other => {
                    // Unknown specifier: leave as-is.
                    out.push('%');
                    out.push(other);
                }
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx() -> SpecifierContext {
        SpecifierContext {
            unit_name: "backup@home.service".into(),
            runtime_dir: "/run".into(),
            user_name: "alice".into(),
            uid: "1000".into(),
            home: "/home/alice".into(),
            hostname: "box".into(),
            machine_id: "abcd".into(),
        }
    }

    #[test]
    fn name_parts() {
        let c = ctx();
        assert_eq!(c.prefix(), "backup");
        assert_eq!(c.instance(), "home");
        assert_eq!(c.expand("%n"), "backup@home.service");
        assert_eq!(c.expand("%p"), "backup");
        assert_eq!(c.expand("%i"), "home");
    }

    #[test]
    fn non_templated() {
        let mut c = ctx();
        c.unit_name = "backup.service".into();
        assert_eq!(c.prefix(), "backup");
        assert_eq!(c.instance(), "");
        assert_eq!(c.expand("%i"), "");
    }

    #[test]
    fn literals() {
        let c = ctx();
        assert_eq!(c.expand("%%"), "%");
        assert_eq!(c.expand("100%%"), "100%");
        assert_eq!(c.expand("a%zc"), "a%zc"); // %z unknown -> kept literally
    }

    #[test]
    fn mixed() {
        let c = ctx();
        assert_eq!(c.expand("/home/%u/bin"), "/home/alice/bin");
        assert_eq!(c.expand("%t/rustemd"), "/run/rustemd");
        assert_eq!(c.expand("%U"), "1000");
        assert_eq!(c.expand("%H %m"), "box abcd");
        assert_eq!(c.expand("%h"), "/home/alice");
        assert_eq!(c.expand("trailing %"), "trailing %");
    }
}
