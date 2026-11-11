pub mod biome;
pub mod bun_glob;
pub mod data;
pub mod editor_glob;
pub mod eslint;
pub mod evaluate;
pub mod find;
pub mod glob;
pub mod json;
pub mod layers;
pub mod module;
pub mod object;
pub mod oxlint;
pub mod references;
pub mod shared;
pub mod syntax;
pub mod width;

use std::sync::LazyLock;

use layers::{Decision, Setting};
use shared::Memo;

use crate::directories::real_directory;
use crate::paths::{is_absolute, resolve};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BracesSetting {
    pub enforced: bool,
    pub unread: Vec<String>,
}

static SETTINGS: LazyLock<Memo<(String, Option<String>), BracesSetting>> =
    LazyLock::new(Memo::default);

pub fn brace_decisions(dir: &str, extension: Option<&str>) -> std::io::Result<Vec<Decision>> {
    let absolute = if is_absolute(dir) {
        resolve("/", dir)
    } else {
        resolve(&std::env::current_dir()?.to_string_lossy(), dir)
    };

    shared::decisions(&real_directory(&absolute)?, extension)
}

pub fn braces_setting(dir: &str, extension: Option<&str>) -> BracesSetting {
    let requested = (dir.to_owned(), extension.map(str::to_owned));
    SETTINGS.get(&requested, || resolved_setting(dir, extension))
}

fn resolved_setting(dir: &str, extension: Option<&str>) -> BracesSetting {
    brace_decisions(dir, extension).map_or(
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
    )
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod growth_tests;
