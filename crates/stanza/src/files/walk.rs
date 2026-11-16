use std::fs::{self, OpenOptions};
use std::io::{self, Read};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use super::{Failure, exists, reason, searchable, shown_path};
use crate::gitignore::{Rule, ignored_by_rules, parse_ignore};
use crate::languages::{is_candidate, prunes};
use crate::paths::{dirname, join, quoted, relative, relative_bytes};

struct IgnoreSource {
    directory: PathBuf,
    rules: Vec<Rule>,
}

fn ignored_by_sources(sources: &[Rc<IgnoreSource>], path: &Path, is_directory: bool) -> bool {
    sources.iter().fold(false, |ignored, source| {
        let below = path.strip_prefix(&source.directory).unwrap_or(path);
        ignored_by_rules(&source.rules, below.as_os_str().as_bytes(), is_directory)
            .unwrap_or(ignored)
    })
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
    cwd: &'a str,
    keep_generated: bool,
    pub(super) files: Vec<String>,
    pub(super) invalid_names: Vec<PathBuf>,
    pub(super) errors: Vec<Failure>,
}

impl Walk<'_> {
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
                let raw = relative_bytes(self.cwd, ignore_file.as_os_str().as_bytes());
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
                self.invalid_names.push(raw.to_owned());
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
            let raw_file = raw.join(name);
            if prunes(&rel) || ignored_by_sources(&active, &raw_file, kind.is_dir()) {
                continue;
            }

            if kind.is_dir() {
                self.walk(&file, &raw_file, named, &active);
            } else if kind.is_file() && is_candidate(&rel, self.keep_generated) {
                if named {
                    self.files.push(file);
                } else {
                    self.invalid_names.push(raw_file);
                }
            }
        }
    }
}

pub(super) fn fallback_files<'a>(dir: &'a str, cwd: &'a str, keep_generated: bool) -> Walk<'a> {
    let mut walk = Walk {
        dir,
        cwd,
        keep_generated,
        files: Vec::new(),
        invalid_names: Vec::new(),
        errors: Vec::new(),
    };

    let mut ancestors = Vec::new();
    let mut current = dir.to_owned();
    while !exists(&join(&current, ".git")) && dirname(&current) != current {
        current = dirname(&current);
        ancestors.push(current.clone());
    }

    ancestors.reverse();
    let mut inherited = Vec::new();
    for (index, ancestor) in ancestors.iter().enumerate() {
        inherited = walk.with_ignore_file(Path::new(ancestor), &inherited);
        let below = ancestors.get(index + 1).map_or(dir, String::as_str);
        if ignored_by_sources(&inherited, Path::new(below), true) {
            return walk;
        }
    }

    walk.walk(dir, Path::new(dir), true, &inherited);
    walk
}
