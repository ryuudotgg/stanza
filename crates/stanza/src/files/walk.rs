use std::fs::{self, OpenOptions};
use std::io::{self, Read};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use super::gitignore::{Rule, ignored_by_rules, parse_ignore};
use super::{Failure, Location, Repositories, has_git, searchable, shown_path};
use crate::languages::{is_candidate, prunes};
use crate::os::reason;
use crate::paths::{dirname, join, quoted, relative, relative_bytes};

struct IgnoreSource {
    directory: PathBuf,
    rules: Vec<Rule>,
}

fn ignored_by_sources(sources: &[Rc<IgnoreSource>], path: &Path, is_directory: bool) -> bool {
    sources
        .iter()
        .rev()
        .find_map(|source| {
            let below = path.strip_prefix(&source.directory).unwrap_or(path);
            ignored_by_rules(&source.rules, below.as_os_str().as_bytes(), is_directory)
        })
        .unwrap_or(false)
}

fn read_ignore_file(path: &Path) -> Option<io::Result<Vec<u8>>> {
    let opened_kind = |meta: &fs::Metadata| meta.is_file() || meta.is_dir();
    if !fs::symlink_metadata(path).is_ok_and(|meta| opened_kind(&meta)) {
        return None;
    }

    let opened = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path);

    let mut file = match opened {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return None,
        Err(error) if error.raw_os_error() == Some(libc::ELOOP) => return None,
        Err(error) => return Some(Err(error)),
    };

    match file.metadata() {
        Ok(meta) if !opened_kind(&meta) => return None,
        Err(error) => return Some(Err(error)),
        Ok(_) => {}
    }

    let mut bytes = Vec::new();
    Some(file.read_to_end(&mut bytes).map(|_| bytes))
}

pub(super) struct Walk<'a> {
    dir: &'a str,
    real: &'a str,
    cwd: &'a str,
    keep_generated: bool,
    repositories: &'a mut Repositories,
    pub(super) files: Vec<String>,
    pub(super) generated: bool,
    pub(super) nested: Vec<(String, String)>,
    pub(super) non_utf8_names: Vec<PathBuf>,
    pub(super) errors: Vec<Failure>,
}

impl Walk<'_> {
    fn as_supplied(&self, raw: &Path) -> PathBuf {
        match raw.strip_prefix(self.real) {
            Ok(below) => Path::new(self.dir).join(below),
            Err(_) => raw.to_owned(),
        }
    }

    fn with_ignore_file(
        &mut self,
        raw: &Path,
        sources: &[Rc<IgnoreSource>],
    ) -> Vec<Rc<IgnoreSource>> {
        let mut active = sources.to_vec();
        let ignore_file = raw.join(".gitignore");
        let Some(read) = read_ignore_file(&ignore_file) else {
            return active;
        };

        match read {
            Ok(bytes) => active.push(Rc::new(IgnoreSource {
                directory: raw.to_owned(),
                rules: parse_ignore(bytes),
            })),
            Err(error) => {
                let supplied = self.as_supplied(&ignore_file);
                let raw = relative_bytes(self.cwd, supplied.as_os_str().as_bytes());
                let shown = match std::str::from_utf8(&raw) {
                    Ok(shown) => shown.to_owned(),
                    Err(_) => quoted(&raw),
                };

                let line = format!("cannot read {shown}: {}", reason(&error));
                self.errors.push(Failure::new(raw, line));
            }
        }

        active
    }

    fn handed_to_git(&mut self, dir: &str) -> bool {
        if !has_git() || !Path::new(&join(dir, ".git")).exists() {
            return false;
        }

        let location = match searchable(dir) {
            Ok(()) => self.repositories.locate(dir),
            Err(error) => {
                let shown = shown_path(self.cwd, dir);
                Location::Failed(format!("cannot read {shown}: {}", reason(&error)))
            }
        };

        match location {
            Location::Outside => false,
            Location::Repository(root) => {
                self.nested.push((dir.to_owned(), root));
                true
            }
            Location::Failed(error) => {
                self.errors
                    .push(Failure::new(shown_path(self.cwd, dir), error));
                true
            }
        }
    }

    fn walk(&mut self, current: &str, raw: &Path, valid: bool, parents: &[Rc<IgnoreSource>]) {
        let active = self.with_ignore_file(raw, parents);
        let entries = searchable(raw).and_then(|()| {
            fs::read_dir(raw)?
                .map(|entry| {
                    let entry = entry?;
                    Ok((entry.file_name(), entry.file_type()?))
                })
                .collect::<io::Result<Vec<_>>>()
        });

        let entries = match entries {
            Ok(entries) => entries,
            Err(_) if !valid => {
                self.non_utf8_names.push(raw.to_owned());
                return;
            }
            Err(error) => {
                let shown = shown_path(self.cwd, current);
                let line = format!("cannot read {shown}: {}", reason(&error));
                self.errors.push(Failure::new(shown, line));
                return;
            }
        };

        for (name, kind) in entries {
            let file = join(current, &name.to_string_lossy());
            let rel = relative(self.dir, &file);
            let named = valid && name.to_str().is_some();
            let entered = kind.is_dir();
            let regular = kind.is_file();
            let kept = regular && is_candidate(&rel, self.keep_generated);
            let generated = regular && !kept && !self.keep_generated && is_candidate(&rel, true);
            if !entered && !kept && !generated {
                continue;
            }

            let raw_file = raw.join(name);
            if prunes(&rel) || ignored_by_sources(&active, &raw_file, entered) {
                continue;
            }

            if entered && named && self.handed_to_git(&file) {
                continue;
            }

            if entered {
                self.walk(&file, &raw_file, named, &active);
            } else if generated {
                self.generated = true;
            } else if named {
                self.files.push(raw_file.to_string_lossy().into_owned());
            } else {
                self.non_utf8_names.push(raw_file);
            }
        }
    }
}

pub(super) fn fallback_files<'a>(
    dir: &'a str,
    real: &'a str,
    cwd: &'a str,
    keep_generated: bool,
    repositories: &'a mut Repositories,
) -> Walk<'a> {
    let mut walk = Walk {
        dir,
        real,
        cwd,
        keep_generated,
        repositories,
        files: Vec::new(),
        generated: false,
        nested: Vec::new(),
        non_utf8_names: Vec::new(),
        errors: Vec::new(),
    };

    let mut ancestors = Vec::new();
    let mut current = real;
    while !Path::new(&join(current, ".git")).exists() && dirname(current) != current {
        current = dirname(current);
        ancestors.push(current);
    }

    ancestors.reverse();
    let mut inherited = Vec::new();
    for (index, ancestor) in ancestors.iter().enumerate() {
        inherited = walk.with_ignore_file(Path::new(ancestor), &inherited);
        let below = ancestors.get(index + 1).copied().unwrap_or(real);
        if ignored_by_sources(&inherited, Path::new(below), true) {
            return walk;
        }
    }

    walk.walk(dir, Path::new(real), true, &inherited);
    walk
}
