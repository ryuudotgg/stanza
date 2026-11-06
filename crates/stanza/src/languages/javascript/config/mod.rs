pub mod biome;
pub mod bun_glob;
pub mod editor_glob;
pub mod eslint;
pub mod estree;
pub mod evaluate;
pub mod find;
pub mod glob;
pub mod json;
pub mod layers;
pub mod module;
pub mod oxlint;
pub mod width;

use std::cell::RefCell;
use std::collections::HashMap;

use layers::{Config, Decision, Family, Setting};

use crate::directories::real_directory;
use crate::paths::{is_absolute, resolve};

const READERS: [Family; 4] = [Family::Flat, Family::Legacy, Family::Biome, Family::Oxlint];

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BracesSetting {
    pub enforced: bool,
    pub unread: Vec<String>,
}

thread_local! {
    pub(super) static CONFIG: Config = Config::default();
    static SETTINGS: RefCell<HashMap<(String, Option<String>), BracesSetting>> = RefCell::default();
}

pub fn brace_decisions(dir: &str, extension: Option<&str>) -> std::io::Result<Vec<Decision>> {
    let absolute = if is_absolute(dir) {
        resolve("/", dir)
    } else {
        resolve(&std::env::current_dir()?.to_string_lossy(), dir)
    };

    let dir = real_directory(&absolute)?;
    CONFIG.with(|config| {
        config.overflowed.set(false);
        let mut decisions = Vec::new();
        for reader in READERS {
            let decision = reader.decide(config, &dir, extension);
            if config.overflowed.replace(false) {
                return Err(std::io::Error::other("a config value contains itself"));
            }

            decisions.extend(decision);
        }

        Ok(decisions)
    })
}

pub fn braces_setting(dir: &str, extension: Option<&str>) -> BracesSetting {
    let requested = (dir.to_owned(), extension.map(str::to_owned));
    if let Some(cached) = SETTINGS.with_borrow(|settings| settings.get(&requested).cloned()) {
        return cached;
    }

    let result = brace_decisions(dir, extension).map_or(
        BracesSetting {
            enforced: true,
            unread: Vec::new(),
        },
        |decisions| {
            let enforced = decisions
                .iter()
                .any(|decision| decision.setting != Setting::Off);

            let unread = if decisions
                .iter()
                .any(|decision| decision.setting == Setting::On)
            {
                Vec::new()
            } else {
                decisions
                    .iter()
                    .filter(|decision| decision.setting == Setting::Unknown)
                    .flat_map(|decision| decision.files.iter().cloned())
                    .collect()
            };

            BracesSetting { enforced, unread }
        },
    );

    SETTINGS.with_borrow_mut(|settings| settings.insert(requested, result.clone()));
    result
}

#[cfg(test)]
mod tests;
