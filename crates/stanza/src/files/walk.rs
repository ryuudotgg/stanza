use std::fs;
use std::io;
use std::rc::Rc;

use super::{code_or_message, code_unit_order, exists, searchable, shown_path, with_code};
use crate::gitignore::{Rule, ignored_by_rules, parse_ignore};
use crate::languages::{is_candidate, prunes};
use crate::paths::{dirname, join, relative};

struct IgnoreSource {
    directory: String,
    rules: Vec<Rule>,
}

fn ignored_by_sources(sources: &[Rc<IgnoreSource>], path: &str, is_directory: bool) -> bool {
    sources.iter().fold(false, |ignored, source| {
        let relative_path = relative(&source.directory, path);
        ignored_by_rules(&source.rules, &relative_path, is_directory).unwrap_or(ignored)
    })
}

struct Walk<'a> {
    dir: &'a str,
    cwd: &'a str,
    keep_generated: bool,
    files: Vec<String>,
    errors: Vec<String>,
}

impl Walk<'_> {
    fn with_ignore_file(
        &mut self,
        current: &str,
        sources: &[Rc<IgnoreSource>],
    ) -> Vec<Rc<IgnoreSource>> {
        let mut active = sources.to_vec();
        let ignore_file = join(current, ".gitignore");
        if !exists(&ignore_file) {
            return active;
        }

        match fs::read(&ignore_file) {
            Ok(bytes) => active.push(Rc::new(IgnoreSource {
                directory: current.to_owned(),
                rules: parse_ignore(&String::from_utf8_lossy(&bytes)),
            })),
            Err(error) => self.errors.push(format!(
                "cannot read {}: {}",
                relative(self.cwd, &ignore_file),
                code_or_message(&error)
            )),
        }

        active
    }

    fn walk(&mut self, current: &str, parents: &[Rc<IgnoreSource>]) {
        let active = self.with_ignore_file(current, parents);
        let entries = searchable(current).and_then(|()| {
            fs::read_dir(current)?
                .map(|entry| {
                    let entry = entry?;
                    let name = entry.file_name().to_string_lossy().into_owned();
                    Ok((name, entry.file_type()?))
                })
                .collect::<io::Result<Vec<_>>>()
        });

        let entries = match entries {
            Ok(entries) => entries,
            Err(error) => {
                let shown = shown_path(self.cwd, current);
                self.errors
                    .push(with_code(&format!("cannot read {shown}"), &error));
                return;
            }
        };

        for (name, kind) in entries {
            let file = join(current, &name);
            let rel = relative(self.dir, &file);
            if prunes(&rel) || ignored_by_sources(&active, &file, kind.is_dir()) {
                continue;
            }

            if kind.is_dir() {
                self.walk(&file, &active);
            } else if kind.is_file() && is_candidate(&rel, self.keep_generated) {
                self.files.push(file);
            }
        }
    }
}

pub(super) fn fallback_files(
    dir: &str,
    cwd: &str,
    keep_generated: bool,
) -> (Vec<String>, Vec<String>) {
    let mut walk = Walk {
        dir,
        cwd,
        keep_generated,
        files: Vec::new(),
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
        inherited = walk.with_ignore_file(ancestor, &inherited);
        let below = ancestors.get(index + 1).map_or(dir, String::as_str);
        if ignored_by_sources(&inherited, below, true) {
            return (walk.files, walk.errors);
        }
    }

    walk.walk(dir, &inherited);
    walk.errors
        .sort_by(|left, right| code_unit_order(left, right));
    (walk.files, walk.errors)
}
