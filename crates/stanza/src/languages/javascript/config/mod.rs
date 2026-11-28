pub mod aliases;
pub mod biome;
pub mod coverage;
pub mod data;
pub mod depth;
pub mod editor_glob;
pub mod effects;
pub mod eslint;
pub mod evaluate;
pub mod evaluator;
pub mod family;
pub mod find;
pub mod glob;
pub mod layers;
pub mod module;
pub mod object;
pub mod override_glob;
pub mod oxlint;
pub mod references;
pub mod syntax;
mod token_glob;
pub mod width;
pub mod yaml;

use std::sync::LazyLock;

use coverage::{Setting, Target};
use family::Decision;

use crate::directories::real_directory;
use crate::languages::BracesSetting;
use crate::languages::javascript::entry::ENTRY;
use crate::memo::Memo;
use crate::paths::{dirname, extname, is_absolute, resolve};

#[derive(Clone)]
enum Folder {
    Settled(BracesSetting),
    PerFile,
}

static FOLDERS: LazyLock<Memo<Target, Folder>> = LazyLock::new(Memo::default);

const KEPT: BracesSetting = BracesSetting {
    enforced: true,
    unread: Vec::new(),
};

pub fn brace_decisions(target: &Target) -> std::io::Result<Vec<Decision>> {
    read_decisions(target).map(|(decisions, _)| decisions)
}

fn read_decisions(target: &Target) -> std::io::Result<(Vec<Decision>, bool)> {
    let absolute = if is_absolute(&target.dir) {
        resolve("/", &target.dir)
    } else {
        resolve(&std::env::current_dir()?.to_string_lossy(), &target.dir)
    };

    let resolved = Target {
        dir: real_directory(&absolute)?,
        name: target.name.clone(),
    };

    Ok(evaluator::decisions(&resolved))
}

pub fn braces_setting(path: &str) -> BracesSetting {
    let extension = extname(path);
    if ENTRY.extensions.contains(&extension) {
        let folder = Target::files_in(dirname(path), Some(extension));
        if let Folder::Settled(setting) = FOLDERS.get(&folder, || folder_setting(&folder)) {
            return setting;
        }
    }

    brace_decisions(&Target::file(path)).map_or(KEPT, |decisions| settled(&decisions))
}

fn folder_setting(folder: &Target) -> Folder {
    match read_decisions(folder) {
        Ok((_, true)) => Folder::PerFile,
        Ok((decisions, false)) => Folder::Settled(settled(&decisions)),
        Err(_) => Folder::Settled(KEPT),
    }
}

fn settled(decisions: &[Decision]) -> BracesSetting {
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
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod growth_tests;
