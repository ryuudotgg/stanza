use std::fs;
use std::io;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use super::{code_or_message, exists, searchable, shown_path, with_code};
use crate::gitignore::{Rule, ignored_by_rules, parse_ignore};
use crate::languages::{is_candidate, prunes};
use crate::paths::{dirname, join, quoted, relative, relative_bytes};
use crate::text::code_unit_order;

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

struct Walk<'a> {
    dir: &'a str,
    cwd: &'a str,
    keep_generated: bool,
    files: Vec<String>,
    invalid_names: Vec<PathBuf>,
    errors: Vec<String>,
}

pub(super) struct Walked {
    pub(super) files: Vec<String>,
    pub(super) invalid_names: Vec<PathBuf>,
    pub(super) errors: Vec<String>,
}

impl Walk<'_> {
    fn walked(self) -> Walked {
        Walked {
            files: self.files,
            invalid_names: self.invalid_names,
            errors: self.errors,
        }
    }

    fn with_ignore_file(
        &mut self,
        raw: &Path,
        sources: &[Rc<IgnoreSource>],
    ) -> Vec<Rc<IgnoreSource>> {
        let mut active = sources.to_vec();
        let ignore_file = raw.join(".gitignore");
        if !ignore_file.exists() {
            return active;
        }

        match fs::read(&ignore_file) {
            Ok(bytes) => active.push(Rc::new(IgnoreSource {
                directory: raw.to_owned(),
                rules: parse_ignore(bytes),
            })),
            Err(error) => {
                let shown = relative_bytes(self.cwd, ignore_file.as_os_str().as_bytes());
                let shown = match String::from_utf8(shown) {
                    Ok(shown) => shown,
                    Err(shown) => quoted(shown.as_bytes()),
                };

                self.errors
                    .push(format!("cannot read {shown}: {}", code_or_message(&error)));
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
                self.errors
                    .push(with_code(&format!("cannot read {shown}"), &error));
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

pub(super) fn fallback_files(dir: &str, cwd: &str, keep_generated: bool) -> Walked {
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
        ancestors.insert(0, current.clone());
    }

    let mut inherited = Vec::new();
    for (index, ancestor) in ancestors.iter().enumerate() {
        inherited = walk.with_ignore_file(Path::new(ancestor), &inherited);
        let below = ancestors.get(index + 1).map_or(dir, String::as_str);
        if ignored_by_sources(&inherited, Path::new(below), true) {
            return walk.walked();
        }
    }

    walk.walk(dir, Path::new(dir), true, &inherited);
    walk.errors
        .sort_by(|left, right| code_unit_order(left, right));
    walk.walked()
}
