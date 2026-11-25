use std::collections::HashSet;

use super::coverage::{Setting, Target};
use super::evaluate::{CHAIN_CAP, Value, property};
use super::layers::{Config, Found, Identity, identity};
use super::module::Session;
use super::{biome, eslint, oxlint};

pub(super) const READERS: [Family; 4] =
    [Family::Flat, Family::Legacy, Family::Biome, Family::Oxlint];

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Family {
    Flat,
    Legacy,
    Biome,
    Oxlint,
}

impl Family {
    pub const fn files(self) -> &'static [&'static str] {
        match self {
            Self::Flat => &eslint::FLAT_FILES,
            Self::Legacy => &eslint::LEGACY_FILES,
            Self::Biome => &biome::FILES,
            Self::Oxlint => &oxlint::FILES,
        }
    }

    pub(super) const fn chain_cap(self) -> usize {
        match self {
            Self::Biome => biome::NESTED_EXTENDS + 1,
            _ => CHAIN_CAP,
        }
    }

    pub(super) fn candidate(self, config: &Config, file: &str) -> bool {
        match self {
            Self::Legacy => eslint::legacy_candidate(config, file),
            _ => true,
        }
    }

    pub fn decide(self, config: &Config, target: &Target) -> Option<Decision> {
        match self {
            Self::Flat => eslint::flat_decide(config, target),
            Self::Legacy => eslint::legacy_decide(config, target),
            Self::Biome => biome::decide(config, target),
            Self::Oxlint => oxlint::decide(config, target),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Decision {
    pub family: Family,
    pub setting: Setting,
    pub files: Vec<String>,
}

pub fn severity(session: &Session, value: &Value, family: Family) -> Setting {
    let mut value = value.clone();
    let mut seen: HashSet<Identity> = HashSet::new();
    loop {
        let next = match (&value, family) {
            (Value::String(text), _) => {
                return match (text.as_ref(), family) {
                    ("off", _) | ("allow", Family::Oxlint) => Setting::Off,
                    ("warn" | "error" | "deny", _)
                    | (_, Family::Flat | Family::Legacy | Family::Biome) => Setting::On,
                    _ => Setting::Unknown,
                };
            }
            (Value::Number(number), _) => {
                return match (*number, family) {
                    (0.0, _) => Setting::Off,
                    (1.0 | 2.0, _) | (_, Family::Flat | Family::Legacy | Family::Biome) => {
                        Setting::On
                    }
                    _ => Setting::Unknown,
                };
            }
            (Value::Array(items), _) => items.first().cloned().unwrap_or_default(),
            (Value::Object(_), Family::Flat | Family::Legacy | Family::Biome) => {
                property(session, &value, "level")
            }
            _ => return Setting::Unknown,
        };

        if !seen.insert(identity(&value)) {
            return Setting::Unknown;
        }

        value = next;
    }
}

pub fn decided(reader: Family, found: Option<&Found>) -> Option<Decision> {
    let found = found?;
    Some(Decision {
        family: reader,
        setting: found.setting?,
        files: vec![found.file.clone()],
    })
}

pub fn enforcing(found: &[Found]) -> Option<&Found> {
    let nearest = found.iter().find(|item| item.setting.is_some());
    found
        .iter()
        .find(|item| matches!(item.setting, Some(Setting::On | Setting::Unknown)))
        .or(nearest)
}

#[cfg(test)]
mod tests {
    use std::rc::Rc;

    use super::*;
    use crate::languages::javascript::config::object::Object;

    #[test]
    fn severity_policies_keep_family_answers() {
        let session = Session::default();
        let looped = Value::Object(Object::holding_itself("level"));
        let Value::Object(object) = &looped else {
            unreachable!();
        };

        assert!(matches!(
            property(&session, &looped, "level"),
            Value::Object(level) if Rc::ptr_eq(&level, object)
        ));

        let word = |text: &str| Value::String(text.into());
        let list = |items: Vec<Value>| Value::Array(Rc::new(items));
        let level = |text: &str| Value::Object(Object::plain(vec![("level", word(text))]));
        let cases = [
            ("\"off\"", word("off"), Setting::Off, Setting::Off),
            ("0", Value::Number(0.0), Setting::Off, Setting::Off),
            (
                "[\"off\"]",
                list(vec![word("off")]),
                Setting::Off,
                Setting::Off,
            ),
            (
                "{ level: \"off\" }",
                level("off"),
                Setting::Off,
                Setting::Unknown,
            ),
            ("\"allow\"", word("allow"), Setting::On, Setting::Off),
            ("3", Value::Number(3.0), Setting::On, Setting::Unknown),
            ("\"deny\"", word("deny"), Setting::On, Setting::On),
            ("\"loud\"", word("loud"), Setting::On, Setting::Unknown),
            ("1", Value::Number(1.0), Setting::On, Setting::On),
            ("2", Value::Number(2.0), Setting::On, Setting::On),
            (
                "[\"warn\"]",
                list(vec![word("warn")]),
                Setting::On,
                Setting::On,
            ),
            ("[]", list(Vec::new()), Setting::Unknown, Setting::Unknown),
            (
                "x = { level: x }",
                looped.clone(),
                Setting::Unknown,
                Setting::Unknown,
            ),
        ];

        for (name, value, eslint, oxlint) in cases {
            for family in [Family::Flat, Family::Legacy, Family::Biome] {
                assert_eq!(
                    severity(&session, &value, family),
                    eslint,
                    "{family:?} {name}"
                );
            }

            assert_eq!(
                severity(&session, &value, Family::Oxlint),
                oxlint,
                "oxlint {name}"
            );
        }
    }
}
