use std::path::{Path, PathBuf};

use oxc_allocator::Allocator;

use crate::languages::javascript::parse::parse;
use crate::languages::javascript::scan::comments;

use super::git::{GitEnv, Repositories};
use super::*;

const CORPORA: [&str; 4] = ["Kingshot", "meisai", "F&S", "caasx"];

const SCRIPT_EXTENSIONS: [&str; 8] = [".js", ".jsx", ".mjs", ".cjs", ".ts", ".tsx", ".mts", ".cts"];

const ANY: &str = r"[^\r\n\x{2028}\x{2029}]";

const GENERATED_TAG_SOURCE: &str = r"@generated\b";

const GENERATED_MARKER_SOURCES: [&str; 4] = [
    r"^code generated\b{any}*\bdo not edit\b",
    r"\b(?:this|the) (?:file|code|class) (?:is|was|has been) (?:auto(?:matically)?[- ]?)?generated\b",
    r"\bauto(?:matically)?[- ]?generated (?:file|code|class)\b",
    r"^(?:auto(?:matically)?[- ]?)generated\b",
];

const GENERATOR_ATTRIBUTION_SOURCE: &str = r"^(?:code )?generated (?:by|from|with|using|code)\b";

const EDIT_WARNING_SOURCE: &str = r"\bdo not (?:edit|modify|make (?:direct )?changes)\b";

const UNOPENED_DIRECTORY_SOURCE: &str = r"^warning: could not open directory '(.*)/': (.*)";

const UNREADABLE_FILE_SOURCE: &str = r"^warning: unable to access '(.*)': (.*)";

struct HeaderPair {
    source: &'static str,
    old: regex::bytes::Regex,
    new: fn(&str) -> bool,
}

struct WarningPair {
    source: &'static str,
    old: regex::Regex,
    new: Warning,
}

struct Patterns {
    headers: Vec<HeaderPair>,
    warnings: [WarningPair; 2],
}

impl Patterns {
    fn new() -> Self {
        let sources = [GENERATED_TAG_SOURCE]
            .into_iter()
            .chain(GENERATED_MARKER_SOURCES)
            .chain([GENERATOR_ATTRIBUTION_SOURCE, EDIT_WARNING_SOURCE]);

        let predicates = [has_generated_tag as fn(&str) -> bool]
            .into_iter()
            .chain(GENERATED_MARKERS)
            .chain([names_generator, warns_against_edits]);

        assert_eq!(sources.clone().count(), predicates.clone().count());
        let headers = sources
            .zip(predicates)
            .map(|(source, new)| HeaderPair {
                source,
                old: regex::bytes::Regex::new(
                    &source.replace("{any}", ANY).replace(r"\b", r"(?-u:\b)"),
                )
                .expect("the old header pattern compiles"),
                new,
            })
            .collect();

        let warning = |source, new| WarningPair {
            source,
            old: regex::Regex::new(source).expect("the old warning pattern compiles"),
            new,
        };

        Self {
            headers,
            warnings: [
                warning(UNOPENED_DIRECTORY_SOURCE, UNOPENED_DIRECTORY),
                warning(UNREADABLE_FILE_SOURCE, UNREADABLE_FILE),
            ],
        }
    }

    fn headers_agree(&self, text: &str) {
        for form in [text.to_owned(), text.to_ascii_lowercase()] {
            for pair in &self.headers {
                let old = pair.old.is_match(form.as_bytes());
                assert_eq!(old, (pair.new)(&form), "{} on {form:?}", pair.source);
            }
        }
    }

    fn warning_agrees(&self, line: &str) -> bool {
        let mut matched = false;
        for pair in &self.warnings {
            let old = pair.old.captures(line).map(|captures| {
                let group = |index| captures.get(index).map_or("", |group| group.as_str());
                (group(1), group(2))
            });

            assert_eq!(old, pair.new.fields(line), "{} on {line:?}", pair.source);
            matched |= old.is_some();
        }

        matched
    }

    fn all_agree(&self, text: &str) {
        self.headers_agree(text);
        self.warning_agrees(text);
    }
}

fn repository_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn script_files(root: &Path) -> Vec<PathBuf> {
    let args = [
        "ls-files",
        "-z",
        "--cached",
        "--others",
        "--exclude-standard",
    ];

    let listed = Repositories::new(None)
        .git(&root.to_string_lossy(), &args, None, &GitEnv::Inherited)
        .expect("git lists the files");

    String::from_utf8_lossy(&listed.output)
        .split('\0')
        .filter(|path| {
            SCRIPT_EXTENSIONS
                .iter()
                .any(|extension| path.ends_with(extension))
        })
        .map(|path| root.join(path))
        .collect()
}

fn corpus_roots() -> Vec<PathBuf> {
    let home = std::env::var_os("HOME").expect("HOME names the home directory");
    let projects = Path::new(&home).join("Projects");
    CORPORA.iter().map(|name| projects.join(name)).collect()
}

fn compare_comments(roots: &[PathBuf]) -> usize {
    let patterns = Patterns::new();
    let mut compared = 0;
    for root in roots {
        for path in script_files(root) {
            let Ok(text) = fs::read_to_string(&path) else {
                continue;
            };

            for comment in header_comments(&text) {
                patterns.headers_agree(comment);
            }

            let allocator = Allocator::default();
            let parsed = parse(&allocator, &path.to_string_lossy(), &text);
            for comment in comments(&text, &parsed.program) {
                patterns.all_agree(comment.value);
                for line in comment.value.split('\n') {
                    patterns.headers_agree(comment_body(line.strip_suffix('\r').unwrap_or(line)));
                }

                compared += 1;
            }
        }
    }

    compared
}

#[test]
fn patterns_match_as_the_regex_crate_did_on_every_repository_comment() {
    assert!(
        compare_comments(&[repository_root()]) > 0,
        "no comment compared"
    );
}

#[test]
#[ignore = "reads the parity corpora under ~/Projects, run with --ignored"]
fn patterns_match_as_the_regex_crate_did_on_every_corpus_comment() {
    assert!(compare_comments(&corpus_roots()) > 0, "no comment compared");
}

#[test]
fn patterns_match_as_the_regex_crate_did_on_written_headers() {
    let patterns = Patterns::new();
    let written = [
        "the file is autogenerated",
        "the file is auto_generated",
        "the file is -generated",
        "the file has been-generated",
        "this class was automatically generated",
        "xthe file is generated",
        "the file is generatedx",
        "code generated\u{2028}do not edit",
        "code generatedx do not edit",
        "code generated do not editx",
        "code generated; do not edit",
        "automaticallygenerated codex",
        "auto generated class",
        "generated by hand",
        "foo@generated",
        "do not make direct changes",
    ];

    for header in written {
        patterns.headers_agree(header);
    }
}

#[cfg(unix)]
fn unopened_directory_warnings() -> Vec<String> {
    use std::os::unix::fs::PermissionsExt;

    use crate::testing::scratch::Scratch;

    let scratch = Scratch::new("regex-parity");
    let locked = ["nr", "un", "deep/'quoted' dir/é\u{2028}"];
    for name in locked {
        scratch.write(&format!("{name}/a.ts"), "export const z = 1;\n");
    }

    let git = |args: &[&str]| {
        Repositories::new(None)
            .git(scratch.path(), args, None, &GitEnv::CLocale)
            .expect("git runs in the scratch repository")
    };

    git(&["init", "-q"]);
    for name in locked {
        fs::set_permissions(scratch.join(name), fs::Permissions::from_mode(0o000))
            .expect("the mode is set");
    }

    let listed = git(&[
        "-c",
        "core.excludesFile=/dev/null",
        "ls-files",
        "-z",
        "--cached",
        "--others",
        "--exclude-standard",
        "--",
        ".",
    ]);
    listed.warnings.lines().map(str::to_owned).collect()
}

#[cfg(not(unix))]
fn unopened_directory_warnings() -> Vec<String> {
    Vec::new()
}

#[test]
fn the_warning_patterns_match_as_the_regex_crate_did_on_git_warnings() {
    let patterns = Patterns::new();
    let written = [
        "warning: could not open directory 'nr/': Permission denied",
        "warning: could not open directory 'a/b/c/': Permission denied",
        "warning: could not open directory 'it/': is/': twice/': Permission denied",
        "warning: could not open directory '日本/\u{2028}/': Permission denied",
        "warning: could not open directory 'nr/':Permission denied",
        "warning: could not open directory '/': Permission denied",
        "warning: could not open directory 'cr\r/': Permission denied\r",
        "Warning: could not open directory 'nr/': Permission denied",
        " warning: could not open directory 'nr/': Permission denied",
        "warning: could not open directory 'nr': Permission denied",
        "warning: could not open directory 'a\nb/': Permission denied",
        "warning: unable to access 'a/.gitignore': Permission denied",
        "warning: unable to access 'it': is': twice': Permission denied",
        "warning: unable to access 'x/': y': Too many levels of symbolic links",
        "warning: unable to access '': Permission denied",
        "warning: unable to access ': x",
        "warning: unable to access 'cr\r': Permission denied\r",
        "warning: unable to access 'nr':Permission denied",
    ];

    for line in written {
        patterns.warning_agrees(line);
    }

    let real = unopened_directory_warnings();
    let is_root = unsafe { libc::geteuid() } == 0;
    let matched = real
        .iter()
        .filter(|line| patterns.warning_agrees(line))
        .count();
    assert!(is_root || cfg!(not(unix)) || matched == 3, "{real:?}");
}

const FRAGMENTS: &[&str] = &[
    "code",
    "Code",
    "CODE",
    "generated",
    "Generated",
    "GENERATED",
    "@generated",
    "@Generated",
    "do",
    "not",
    "DO NOT",
    "edit",
    "Edit",
    "modify",
    "make",
    "direct",
    "changes",
    "this",
    "This",
    "the",
    "file",
    "code",
    "class",
    "is",
    "was",
    "has been",
    "auto",
    "automatically",
    "auto-",
    "by",
    "from",
    "with",
    "using",
    " ",
    " ",
    " ",
    "-",
    "_",
    ".",
    "*",
    "/",
    "//",
    "é",
    "日本語",
    "Привет",
    "ـ",
    "٣",
    "\u{301}",
    "\u{200c}",
    "\u{200d}",
    "\u{2028}",
    "\u{2029}",
    "\r",
    "\n",
    "\t",
    "😀",
    "stanza-",
    "ignore",
    "off",
    "on",
    "warning: could not open directory '",
    "warning: unable to access '",
    "/': ",
    "/': /': ",
    "': ",
    "the file is -generated",
    "automatically-generated",
    "auto generated",
    "this file ",
    "has been ",
    "do not edit",
    "has",
    "been",
    "matically",
    "'",
    "x1",
    "\u{e000}",
];

#[test]
fn patterns_match_as_the_regex_crate_did_on_generated_lines() {
    let patterns = Patterns::new();
    let mut state: u64 = 0x9e37_79b9_7f4a_7c15;
    let mut next = |bound: usize| {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        (state % bound as u64) as usize
    };

    for _ in 0..10_000 {
        let count = 1 + next(12);
        let line: String = (0..count)
            .map(|_| FRAGMENTS[next(FRAGMENTS.len())])
            .collect();
        patterns.all_agree(&line);
    }
}
