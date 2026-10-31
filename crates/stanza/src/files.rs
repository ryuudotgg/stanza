use std::collections::{HashMap, HashSet};
use std::fs;
use std::io;
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::LazyLock;

use regex::bytes::Regex;

use crate::languages::{entry_for, is_candidate};
use crate::paths::{basename, dirname, is_absolute, join, relative, resolve};
use crate::text::{js_trim, js_trim_start, locale_compare};

const ANY: &str = r"[^\r\n\x{2028}\x{2029}]";

fn pattern(source: &str) -> Regex {
    Regex::new(&source.replace("{any}", ANY).replace(r"\b", r"(?-u:\b)"))
        .expect("the header patterns compile")
}

// JavaScript /i without u folds only ASCII here, so the comment is ASCII lowercased and the patterns are lowercase.
static GENERATED_TAG: LazyLock<Regex> = LazyLock::new(|| pattern(r"@generated\b"));

static GENERATED_MARKERS: LazyLock<[Regex; 4]> = LazyLock::new(|| {
    [
        pattern(r"^code generated\b{any}*\bdo not edit\b"),
        pattern(
            r"\b(?:this|the) (?:file|code|class) (?:is|was|has been) (?:auto(?:matically)?[- ]?)?generated\b",
        ),
        pattern(r"\bauto(?:matically)?[- ]?generated (?:file|code|class)\b"),
        pattern(r"^(?:auto(?:matically)?[- ]?)generated\b"),
    ]
});

static GENERATOR_ATTRIBUTION: LazyLock<Regex> =
    LazyLock::new(|| pattern(r"^(?:code )?generated (?:by|from|with|using|code)\b"));

static EDIT_WARNING: LazyLock<Regex> =
    LazyLock::new(|| pattern(r"\bdo not (?:edit|modify|make (?:direct )?changes)\b"));

pub fn is_generated_header(text: &str) -> bool {
    let comments = header_comments(text);
    if comments
        .iter()
        .any(|comment| GENERATED_TAG.is_match(comment.as_bytes()))
    {
        return true;
    }

    let folded: Vec<Vec<u8>> = comments
        .iter()
        .map(|comment| comment.to_ascii_lowercase().into_bytes())
        .collect();

    let any = |regex: &Regex| folded.iter().any(|comment| regex.is_match(comment));
    GENERATED_MARKERS.iter().any(any) || (any(&GENERATOR_ATTRIBUTION) && any(&EDIT_WARNING))
}

fn header_comments(text: &str) -> Vec<&str> {
    let mut comments = Vec::new();
    let mut in_block = false;
    for line in text.split('\n').take(10) {
        let mut rest = js_trim(line.strip_suffix('\r').unwrap_or(line));
        while !rest.is_empty() {
            if !in_block {
                if let Some(body) = rest.strip_prefix("//") {
                    comments.push(comment_body(body));
                    break;
                }

                let Some(body) = rest.strip_prefix("/*") else {
                    break;
                };

                rest = body;
            }

            let end = rest.find("*/");
            in_block = end.is_none();
            comments.push(comment_body(end.map_or(rest, |end| &rest[..end])));
            rest = end.map_or("", |end| js_trim_start(&rest[end + 2..]));
        }
    }

    comments
}

fn comment_body(text: &str) -> &str {
    js_trim(text.trim_start_matches(['/', '*']))
}

pub fn error_code(error: &io::Error) -> Option<&'static str> {
    let code = match error.raw_os_error()? {
        libc::EACCES => "EACCES",
        libc::EPERM => "EPERM",
        libc::ENOENT => "ENOENT",
        libc::ENOTDIR => "ENOTDIR",
        libc::EISDIR => "EISDIR",
        libc::ELOOP => "ELOOP",
        libc::ENAMETOOLONG => "ENAMETOOLONG",
        libc::EIO => "EIO",
        libc::EMFILE => "EMFILE",
        libc::ENFILE => "ENFILE",
        libc::EBADF => "EBADF",
        libc::EBUSY => "EBUSY",
        libc::EAGAIN => "EAGAIN",
        libc::ENOMEM => "ENOMEM",
        libc::EINVAL => "EINVAL",
        libc::ENXIO => "ENXIO",
        libc::ENODEV => "ENODEV",
        libc::ETIMEDOUT => "ETIMEDOUT",
        libc::EFBIG => "EFBIG",
        libc::EOVERFLOW => "EOVERFLOW",
        libc::EOPNOTSUPP => "EOPNOTSUPP",
        libc::EROFS => "EROFS",
        libc::ENOSPC => "ENOSPC",
        libc::EEXIST => "EEXIST",
        libc::ESPIPE => "ESPIPE",
        libc::EPIPE => "EPIPE",
        libc::EINTR => "EINTR",
        libc::ECONNRESET => "ECONNRESET",
        _ => return None,
    };

    Some(code)
}

pub fn with_code(text: &str, error: &io::Error) -> String {
    match error_code(error) {
        Some(code) => format!("{text}: {code}"),
        None => text.to_owned(),
    }
}

fn missing(error: &io::Error) -> bool {
    matches!(error_code(error), Some("ENOENT" | "ENOTDIR"))
}

fn realpath(path: &str) -> io::Result<String> {
    Ok(fs::canonicalize(path)?.to_string_lossy().into_owned())
}

fn supplied(cwd: &str, input: &str) -> String {
    if is_absolute(input) {
        input.to_owned()
    } else {
        resolve(cwd, input)
    }
}

fn exists(path: &str) -> bool {
    Path::new(path).exists()
}

static GIT_AVAILABLE: LazyLock<bool> = LazyLock::new(|| {
    std::env::var_os("PATH").is_some_and(|path| {
        std::env::split_paths(&path).any(|dir| {
            let candidate = dir.join("git");
            fs::metadata(&candidate).is_ok_and(|meta| {
                use std::os::unix::fs::PermissionsExt;
                meta.is_file() && meta.permissions().mode() & 0o111 != 0
            })
        })
    })
});

pub enum Location {
    Repository,
    Outside,
    Failed(String),
}

fn show_toplevel(dir: &str) -> Result<String, String> {
    let failed = |reason: String| format!("git rev-parse failed in {dir}: {reason}");
    let output = Command::new("git")
        .args(["-C", dir, "rev-parse", "--show-toplevel"])
        .stdin(Stdio::null())
        .output()
        .map_err(|error| failed(error.to_string()))?;

    if output.status.success() {
        return Ok(String::from_utf8_lossy(&output.stdout).into_owned());
    }

    let stderr = String::from_utf8_lossy(&output.stderr);
    let reason = stderr
        .split('\n')
        .map(js_trim)
        .find(|line| !line.is_empty())
        .map(str::to_owned)
        .unwrap_or_else(|| match output.status.code() {
            Some(code) => format!("exit {code}"),
            None => "exit null".to_owned(),
        });

    Err(failed(reason))
}

fn ceiling_directories() -> HashSet<String> {
    let value = std::env::var("GIT_CEILING_DIRECTORIES").unwrap_or_default();
    value
        .split(':')
        .filter(|path| is_absolute(path))
        .map(|path| realpath(path).unwrap_or_else(|_| resolve("/", path)))
        .collect()
}

fn has_git_marker(dir: &str) -> bool {
    if dir.split('/').any(|segment| segment == ".git") {
        return false;
    }

    let ceilings = ceiling_directories();
    let Ok(mut current) = realpath(dir) else {
        return false;
    };

    loop {
        if exists(&join(&current, ".git")) {
            return true;
        }

        let parent = dirname(&current);
        if parent == current || ceilings.contains(&parent) {
            return false;
        }

        current = parent;
    }
}

#[derive(Default)]
pub struct Locations(HashMap<String, Location>);

impl Locations {
    pub fn locate(&mut self, dir: &str) -> &Location {
        self.0
            .entry(dir.to_owned())
            .or_insert_with(|| discover_location(dir))
    }
}

fn discover_location(dir: &str) -> Location {
    if !*GIT_AVAILABLE {
        return Location::Outside;
    }

    match show_toplevel(dir) {
        Ok(output) if !output.strip_suffix('\n').unwrap_or(&output).is_empty() => {
            Location::Repository
        }
        Err(error) if has_git_marker(dir) => Location::Failed(error),
        _ => Location::Outside,
    }
}

fn file_realpath(path: &str) -> io::Result<String> {
    realpath(path).or_else(|error| {
        if fs::symlink_metadata(path).is_ok_and(|meta| meta.is_file()) {
            Ok(join(&realpath(&dirname(path))?, basename(path)))
        } else {
            Err(error)
        }
    })
}

fn supplied_below(anchor: &str, path: &str) -> String {
    let mut current = dirname(path);
    loop {
        if exists(&current) && realpath(&current).is_ok_and(|real| real == anchor) {
            return relative(&current, path);
        }

        let parent = dirname(&current);
        if parent == current {
            return basename(path).to_owned();
        }

        current = parent;
    }
}

fn selected_below(anchor: &str, supplied: &str, file: &str, keep_generated: bool) -> bool {
    is_candidate(&relative(anchor, file), keep_generated)
        && is_candidate(&supplied_below(anchor, supplied), keep_generated)
}

fn inside_repository(input: &str) -> String {
    format!("this build cannot format files inside a git repository yet: {input}")
}

pub struct Collected {
    pub files: Vec<String>,
    pub errors: Vec<String>,
    pub warnings: Vec<String>,
}

pub fn collect_files(
    paths: &[String],
    cwd: &str,
    keep_generated: bool,
    locations: &mut Locations,
) -> Collected {
    let mut files = Vec::new();
    let mut errors = Vec::new();
    let warnings = if *GIT_AVAILABLE {
        Vec::new()
    } else {
        vec!["git not found on PATH, file selection fell back to the directory walk".to_owned()]
    };

    for input in paths {
        let path = supplied(cwd, input);
        match fs::metadata(&path) {
            Err(error) if missing(&error) => {
                errors.push(format!("no such file: {input}"));
                continue;
            }
            Err(error) => {
                errors.push(with_code(&format!("cannot read {input}"), &error));
                continue;
            }
            Ok(meta) if meta.is_dir() => {
                errors.push(format!("this build cannot format directories yet: {input}"));
                continue;
            }
            Ok(_) => {}
        }

        if entry_for(&path).is_none() {
            errors.push(format!("not a TypeScript or JavaScript file: {input}"));
            continue;
        }

        let file = match file_realpath(&path) {
            Ok(file) => file,
            Err(error) => {
                errors.push(with_code(&format!("cannot read {input}"), &error));
                continue;
            }
        };

        match locations.locate(&dirname(&file)) {
            Location::Failed(error) => {
                errors.push(error.clone());
                continue;
            }
            Location::Repository => {
                errors.push(inside_repository(input));
                continue;
            }
            Location::Outside => {}
        }

        if selected_below(&dirname(&file), &path, &file, keep_generated) {
            files.push(file);
        }
    }

    let mut seen = HashSet::new();
    files.retain(|file| seen.insert(file.clone()));
    files.sort_by(|left, right| locale_compare(left, right));

    let mut seen = HashSet::new();
    errors.retain(|error| seen.insert(error.clone()));

    Collected {
        files,
        errors,
        warnings,
    }
}

pub enum EmptyReason {
    Generated,
    Skipped,
    None,
}

pub fn empty_reason(paths: &[String], cwd: &str, locations: &mut Locations) -> EmptyReason {
    if !collect_files(paths, cwd, true, locations).files.is_empty() {
        return EmptyReason::Generated;
    }

    let supported = paths.iter().any(|input| {
        let path = supplied(cwd, input);
        fs::metadata(&path).is_ok_and(|meta| meta.is_file()) && entry_for(&path).is_some()
    });

    if supported {
        EmptyReason::Skipped
    } else {
        EmptyReason::None
    }
}

pub enum StdinTarget {
    Format(String),
    Skip,
    Unsupported(String),
    Failed(String),
}

fn existing_ancestor(path: &str) -> String {
    let parent = dirname(path);
    if exists(path) || parent == path {
        path.to_owned()
    } else {
        existing_ancestor(&parent)
    }
}

pub fn stdin_target(input: &str, cwd: &str, locations: &mut Locations) -> StdinTarget {
    let path = supplied(cwd, input);
    if entry_for(&path).is_none() {
        return StdinTarget::Unsupported(format!("not a TypeScript or JavaScript file: {input}"));
    }

    let directory = existing_ancestor(&dirname(&path));
    let real = match realpath(&directory) {
        Ok(real) => real,
        Err(error) => {
            return StdinTarget::Failed(with_code(&format!("cannot read {input}"), &error));
        }
    };

    let file = join(&real, &relative(&directory, &path));
    match locations.locate(&real) {
        Location::Failed(error) => StdinTarget::Failed(error.clone()),
        Location::Repository => StdinTarget::Failed(inside_repository(input)),
        Location::Outside if selected_below(&dirname(&file), &path, &file, false) => {
            StdinTarget::Format(file)
        }
        Location::Outside => StdinTarget::Skip,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recognizes_generated_headers() {
        let recognized = [
            "// Code generated by protoc-gen-go. DO NOT EDIT.",
            "// @generated",
            "/* eslint-disable */\n// This file was automatically generated. Do not edit.",
            "// THIS FILE IS AUTOMATICALLY GENERATED. DO NOT EDIT.",
            "/**\n * This file was auto-generated by openapi-typescript.\n * Do not make direct changes to the file.\n */",
            "/* eslint-disable */\n// @ts-nocheck\n\n// noinspection JSUnusedGlobalSymbols\n\n// This file was automatically generated by TanStack Router.",
            "/* tslint:disable */\n/**\n * This file was automatically generated by Payload.\n */",
            "/* THIS FILE WAS GENERATED AUTOMATICALLY BY PAYLOAD. */",
            "// This is an automatically generated file. Do not edit.",
            "/**\n * This class is auto generated by OpenAPI Generator.\n * Do not edit the class manually.\n */",
            "/**\n * Generated by orval v6.31.0\n * Do not edit manually.\n */",
            "// GENERATED CODE -- DO NOT EDIT!",
            "/* eslint-disable */ /* @generated */",
            "// Code generated by sqlc.\n// DO NOT EDIT.",
            "\u{feff}// @generated",
            "// @generated\u{e9}",
        ];

        let ten_lines = format!("{}\n// @generated", ["export {};"; 10].join("\n"));
        let unrecognized = [
            "// Do not edit the order below",
            "// TODO: do not edit this until the API settles",
            "// DO NOT EDIT",
            "/**\n * Generated by Claude Opus 4.6 verified by Undo\n */",
            "// Generated by Claude Opus 4.6 verified by Undo",
            "const note = \"This file was automatically generated\";",
            &ten_lines,
            "export {};",
            "// @generatedness",
            "// \u{17f}his file was generated",
            "// @Generated",
            "/* @GENERATED by tool */",
        ];

        for header in recognized {
            assert!(
                is_generated_header(&format!("{header}\nexport {{}};\n")),
                "{header}"
            );
        }

        for header in unrecognized {
            assert!(
                !is_generated_header(&format!("{header}\nexport {{}};\n")),
                "{header}"
            );
        }
    }
}
