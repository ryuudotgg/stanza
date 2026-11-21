use std::collections::HashSet;
use std::fs;
use std::io::{self, Read};
use std::os::unix::fs::MetadataExt;

use super::git::{GitVariable, GitVariables};
use super::{access, exists, lstat, lstat_if_present, realpath};
use crate::paths::{dirname, is_absolute, join, resolve};

pub(super) enum Discovered {
    Repository(Walked),
    Outside,
}

pub(super) struct Walked {
    pub(super) root: String,
    dirs: GitDirs,
}

impl Walked {
    pub(super) fn plain_config(&self, variables: &GitVariables) -> bool {
        let names_root = |value: &str| {
            // pointed_path drops a trailing line break as a pointer file needs, but git keeps one in a config value.
            if value.bytes().any(|byte| byte.is_ascii_control()) {
                return false;
            }

            let named = if is_absolute(value) {
                value.trim_end_matches('/') == self.root
            } else {
                pointed_path(&self.dirs.git_dir, value.as_bytes())
                    .is_some_and(|path| path == self.root)
            };

            named && single_line_config(self.dirs.common_dir())
        };

        variables.readable && !discovery_config(&variables.entries, self.dirs.linked(), names_root)
    }
}

pub(super) fn walk(dir: &str, user: u32, allowed: bool) -> Option<Discovered> {
    if !allowed {
        return None;
    }

    let start = realpath(dir).ok()?;
    let start_meta = fs::metadata(&start).ok()?;
    let control = start.bytes().any(|byte| byte.is_ascii_control());
    if !start_meta.is_dir() || control || start.split('/').any(|part| part == ".git") {
        return None;
    }

    let device = start_meta.dev();
    let mut current = start.as_str();
    loop {
        let marker = join(current, ".git");
        if let Some(entry) = lstat_if_present(&marker).ok()? {
            let dirs = followed_git_dirs(&marker, &entry)?;
            let plain = plain_repository(current, &entry, &dirs, user).ok()?;
            return plain.then(|| {
                Discovered::Repository(Walked {
                    root: current.to_owned(),
                    dirs,
                })
            });
        }

        if lstat_if_present(&join(current, "HEAD")).ok()?.is_some() {
            return None;
        }

        let parent = dirname(current);
        if parent == current {
            return Some(Discovered::Outside);
        }

        // git stops at a mount point, and its rev-parse there fails only when a .git sits above it.
        if fs::metadata(parent).ok()?.dev() != device {
            let stops = std::env::var_os("GIT_DISCOVERY_ACROSS_FILESYSTEM").is_none();
            return (stops && !has_git_marker(parent)).then_some(Discovered::Outside);
        }

        current = parent;
    }
}

pub(super) struct GitDirs {
    pub(super) git_dir: String,
    commondir: Option<String>,
}

impl GitDirs {
    pub(super) fn common_dir(&self) -> &str {
        self.commondir.as_deref().unwrap_or(&self.git_dir)
    }

    pub(super) fn linked(&self) -> bool {
        self.commondir.is_some()
    }
}

pub(super) fn git_dirs(worktree: &str) -> Option<GitDirs> {
    let marker = join(worktree, ".git");
    let entry = lstat_if_present(&marker).ok()??;
    let dirs = followed_git_dirs(&marker, &entry)?;
    git_directory(&dirs).ok()?.then_some(dirs)
}

const GIT_FILE_LIMIT: u64 = 4096;

fn followed_git_dirs(marker: &str, entry: &fs::Metadata) -> Option<GitDirs> {
    let git_dir = if entry.is_dir() {
        marker.to_owned()
    } else if entry.is_file() && entry.len() <= GIT_FILE_LIMIT {
        let text = fs::read(marker).ok()?;
        pointed_path(dirname(marker), text.strip_prefix(b"gitdir: ")?)?
    } else {
        return None;
    };

    let commondir = join(&git_dir, "commondir");
    let commondir = match lstat_if_present(&commondir).ok()? {
        Some(_) => Some(pointed_path(&git_dir, &fs::read(&commondir).ok()?)?),
        None => None,
    };

    Some(GitDirs { git_dir, commondir })
}

// git's discovery reads core.worktree from this file alone, and var -l splits a value holding an escaped line break.
fn single_line_config(common_dir: &str) -> bool {
    fs::read(join(common_dir, "config")).is_ok_and(|text| !text.contains(&b'\\'))
}

// Not paths::join: git resolves .. in a pointer through symlinks, which folding it as text would not.
fn pointed_path(base: &str, pointer: &[u8]) -> Option<String> {
    let mut end = pointer.len();
    while end > 0 && matches!(pointer[end - 1], b'\n' | b'\r') {
        end -= 1;
    }

    let pointer = &pointer[..end];
    if pointer.is_empty() || pointer.iter().any(u8::is_ascii_control) {
        return None;
    }

    let path = std::str::from_utf8(pointer).ok()?;
    if is_absolute(path) {
        realpath(path).ok()
    } else {
        realpath(&format!("{base}/{path}")).ok()
    }
}

fn plain_repository(
    worktree: &str,
    marker_meta: &fs::Metadata,
    dirs: &GitDirs,
    user: u32,
) -> io::Result<bool> {
    let owned = marker_meta.uid() == user
        && lstat(worktree)?.uid() == user
        && lstat(&dirs.git_dir)?.uid() == user
        && lstat(dirs.common_dir())?.uid() == user;

    Ok(owned && git_directory(dirs)?)
}

fn git_directory(dirs: &GitDirs) -> io::Result<bool> {
    let head = join(&dirs.git_dir, "HEAD");
    if !lstat(&head)?.is_file() {
        return Ok(false);
    }

    let mut bytes = [0u8; 255];
    let read = fs::File::open(&head)?.read(&mut bytes)?;
    if !valid_head(&bytes[..read]) {
        return Ok(false);
    }

    access(join(dirs.common_dir(), "objects"), libc::X_OK)?;
    access(join(dirs.common_dir(), "refs"), libc::X_OK)?;
    Ok(true)
}

fn valid_head(bytes: &[u8]) -> bool {
    bytes.starts_with(b"ref: refs/")
        || (bytes.len() >= 40
            && bytes[..40]
                .iter()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(byte)))
}

fn setting_matches(value: Option<&str>, exempt: &[&str]) -> bool {
    !value.is_some_and(|value| exempt.iter().any(|word| value.eq_ignore_ascii_case(word)))
}

fn discovery_config(
    entries: &[GitVariable],
    linked: bool,
    names_worktree: impl Fn(&str) -> bool,
) -> bool {
    entries.iter().any(|entry| {
        let value = entry.value.as_deref();
        match entry.key.as_str() {
            "core.worktree" => !linked && !value.is_some_and(&names_worktree),
            "core.bare" => !linked && setting_matches(value, &["false", "no", "off", "0"]),
            "core.repositoryformatversion" => setting_matches(value, &["0", "1"]),
            key => key.starts_with("extensions."),
        }
    })
}

fn ceiling_directories() -> HashSet<String> {
    let value = std::env::var_os("GIT_CEILING_DIRECTORIES")
        .map(|value| value.to_string_lossy().into_owned())
        .unwrap_or_default();

    value
        .split(':')
        .filter(|path| is_absolute(path))
        .map(|path| {
            if exists(path) {
                realpath(path).unwrap_or_else(|_| resolve("/", path))
            } else {
                resolve("/", path)
            }
        })
        .collect()
}

pub(super) fn has_git_marker(dir: &str) -> bool {
    if dir.split('/').any(|segment| segment == ".git") {
        return false;
    }

    let ceilings = ceiling_directories();
    let Ok(real) = realpath(dir) else {
        return false;
    };

    let mut current = real.as_str();
    loop {
        if exists(&join(current, ".git")) {
            return true;
        }

        let parent = dirname(current);
        if parent == current || ceilings.contains(parent) {
            return false;
        }

        current = parent;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::files::git::{GitOutput, parsed_variables};
    use crate::files::selection_tests::Tree;

    fn entries(text: &str) -> Vec<GitVariable> {
        let output = GitOutput {
            output: text.as_bytes().to_vec(),
            warnings: String::new(),
        };

        parsed_variables(Ok(output)).entries
    }

    #[test]
    fn a_dangling_commondir_is_no_layout_as_git_dies_on_it() {
        let root = Tree::new();
        let git_dir = join(&root.root, "gitdir");
        fs::create_dir(&git_dir).unwrap();
        let entry = lstat(&git_dir).unwrap();
        assert!(followed_git_dirs(&git_dir, &entry).is_some_and(|dirs| !dirs.linked()));

        std::os::unix::fs::symlink(join(&root.root, "missing"), join(&git_dir, "commondir"))
            .unwrap();
        assert!(followed_git_dirs(&git_dir, &entry).is_none());
    }

    #[test]
    fn a_pointer_resolves_like_git_or_not_at_all() {
        let root = Tree::new();
        let target = join(&root.root, "target");
        fs::create_dir_all(join(&target, "inner")).unwrap();
        std::os::unix::fs::symlink(join(&target, "inner"), join(&root.root, "link")).unwrap();

        assert_eq!(
            pointed_path(&root.root, b"link/..\r\n"),
            Some(target.clone())
        );
        assert_eq!(pointed_path("/elsewhere", target.as_bytes()), Some(target));
        assert_eq!(pointed_path(&root.root, b"\n"), None);
        assert_eq!(pointed_path(&root.root, b"target\0x"), None);
        assert_eq!(pointed_path(&root.root, b"missing"), None);
    }

    #[test]
    fn a_utf8_pointer_resolves_with_any_trailing_line_ending() {
        let root = Tree::new();
        let target = join(&root.root, "日本");
        fs::create_dir(&target).unwrap();

        for ending in ["", "\n", "\r", "\r\n"] {
            let relative = format!("日本{ending}");
            let absolute = format!("{target}{ending}");
            assert_eq!(
                pointed_path(&root.root, relative.as_bytes()),
                Some(target.clone())
            );
            assert_eq!(
                pointed_path("/elsewhere", absolute.as_bytes()),
                Some(target.clone())
            );
        }

        assert_eq!(pointed_path(&root.root, b"\xe6\x97\xa5\0"), None);
        assert_eq!(pointed_path(&root.root, b"\xff"), None);
    }

    #[test]
    fn a_pointer_with_a_control_byte_is_refused() {
        let root = Tree::new();
        for name in ["a\u{1}b", "a\u{7f}b"] {
            fs::create_dir(join(&root.root, name)).unwrap();
            assert_eq!(pointed_path(&root.root, name.as_bytes()), None);
            assert_eq!(pointed_path("/", join(&root.root, name).as_bytes()), None);
        }
    }

    #[test]
    fn reads_discovery_settings_like_the_reference_patterns() {
        let main = |text: &str| discovery_config(&entries(text), false, |_| false);
        assert!(main("core.worktree=/x\n"));
        assert!(main("x=1\ncore.bare\n"));
        assert!(!main("x=1\nCORE.BARE\n"));
        assert!(main("core.bare=true"));
        assert!(!main("core.bare=FALSE\ncore.bare=0"));
        assert!(main("core.repositoryformatversion=2"));
        assert!(main("core.repositoryformatversion\n"));
        assert!(!main("core.repositoryformatversion=1\ncore.worktrees=x"));
        assert!(!main("user.name=a\rextensions.objectformat=sha256"));
        assert!(main("user.name=a\nextensions.objectformat=sha256"));
    }

    #[test]
    fn a_worktree_setting_naming_the_root_or_shadowed_by_commondir_is_plain() {
        let root = |value: &str| value == "/x";
        let config = |text: &str, linked: bool| discovery_config(&entries(text), linked, root);
        assert!(!config("core.worktree=/x\n", false));
        assert!(!config("core.worktree=/x\ncore.worktree=/x", false));
        assert!(config("core.worktree=/x\ncore.worktree=/y", false));
        assert!(!config("core.worktreex=/y", false));
        assert!(discovery_config(&entries("core.worktree\n"), false, |_| {
            true
        }));
        assert!(!config("core.bare=true\ncore.worktree=/y", true));
        assert!(!config("core.bare\ncore.worktree", true));
        assert!(config("extensions.worktreeconfig=true", true));
        assert!(config("core.repositoryformatversion=2", true));
    }
}
