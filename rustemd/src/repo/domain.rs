//! Typed unit values and a small systemd-style text codec.

use crate::repo::Error;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum UnitType {
    Service,
    Timer,
    Target,
    Socket,
    Mount,
    Device,
}

impl UnitType {
    pub const ALL: [Self; 6] = [
        Self::Service,
        Self::Timer,
        Self::Target,
        Self::Socket,
        Self::Mount,
        Self::Device,
    ];
    pub fn suffix(self) -> &'static str {
        match self {
            Self::Service => "service",
            Self::Timer => "timer",
            Self::Target => "target",
            Self::Socket => "socket",
            Self::Mount => "mount",
            Self::Device => "device",
        }
    }
    pub fn from_suffix(value: &str) -> Option<Self> {
        Some(match value {
            "service" => Self::Service,
            "timer" => Self::Timer,
            "target" => Self::Target,
            "socket" => Self::Socket,
            "mount" => Self::Mount,
            "device" => Self::Device,
            _ => return None,
        })
    }
    pub fn from_unit_name(name: &str) -> Option<Self> {
        Self::from_suffix(name.rsplit_once('.')?.1)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnitEntry {
    pub key: String,
    pub value: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnitSection {
    pub name: String,
    pub entries: Vec<UnitEntry>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct UnitDocument {
    pub sections: Vec<UnitSection>,
}

impl UnitDocument {
    pub fn parse(text: &str) -> Result<Self, Error> {
        let mut document = Self::default();
        let mut current = None;
        for (index, raw) in text.lines().enumerate() {
            let line = index + 1;
            let trimmed = raw.strip_suffix('\r').unwrap_or(raw).trim();
            if trimmed.is_empty() || trimmed.starts_with('#') || trimmed.starts_with(';') {
                continue;
            }
            if let Some(rest) = trimmed.strip_prefix('[') {
                let Some((name, trailing)) = rest.split_once(']') else {
                    return Err(Error::Parse {
                        line,
                        message: "section header missing closing `]`".into(),
                    });
                };
                let name = name.trim();
                if name.is_empty() || !trailing.trim().is_empty() {
                    return Err(Error::Parse {
                        line,
                        message: format!("invalid section header `{trimmed}`"),
                    });
                }
                document.sections.push(UnitSection {
                    name: name.into(),
                    entries: vec![],
                });
                current = Some(document.sections.len() - 1);
                continue;
            }
            let Some((key, value)) = trimmed.split_once('=') else {
                return Err(Error::Parse {
                    line,
                    message: format!("invalid line `{trimmed}`"),
                });
            };
            let key = key.trim();
            if key.is_empty()
                || !key
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.'))
            {
                return Err(Error::Parse {
                    line,
                    message: format!("invalid key `{key}`"),
                });
            }
            let Some(section) = current else {
                return Err(Error::Parse {
                    line,
                    message: format!("`{key}=` appears before any [Section] header"),
                });
            };
            document.sections[section].entries.push(UnitEntry {
                key: key.into(),
                value: value.trim_start().into(),
            });
        }
        Ok(document)
    }

    pub fn to_text(&self) -> String {
        let mut out = String::new();
        for (index, section) in self.sections.iter().enumerate() {
            if index > 0 {
                out.push('\n');
            }
            out.push('[');
            out.push_str(&section.name);
            out.push_str("]\n");
            for entry in &section.entries {
                out.push_str(&entry.key);
                out.push('=');
                out.push_str(&entry.value);
                out.push('\n');
            }
        }
        out
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnitDefinition {
    pub name: String,
    pub kind: UnitType,
    pub document: UnitDocument,
}
impl UnitDefinition {
    pub fn parse(name: impl Into<String>, text: &str) -> Result<Self, Error> {
        let name = name.into();
        let Some(kind) = UnitType::from_unit_name(&name) else {
            return Err(Error::InvalidName(format!(
                "unit name `{name}` has no recognized unit suffix"
            )));
        };
        Ok(Self {
            name,
            kind,
            document: UnitDocument::parse(text)?,
        })
    }
    pub fn to_text(&self) -> String {
        self.document.to_text()
    }
}
