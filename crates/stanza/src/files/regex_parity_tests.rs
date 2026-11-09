use std::path::{Path, PathBuf};

use oxc_allocator::Allocator;
use regex_syntax::hir::{Class, HirKind};

use crate::engine::directives::continues_identifier;
use crate::languages::javascript::parse::parse;
use crate::languages::javascript::scan::comments;

use super::git::{GitEnv, run_git_with};
use super::*;

const CORPORA: [&str; 4] = ["Kingshot", "meisai", "F&S", "caasx"];

const SCRIPT_EXTENSIONS: [&str; 8] = [".js", ".jsx", ".mjs", ".cjs", ".ts", ".tsx", ".mts", ".cts"];

struct HeaderPair {
    source: &'static str,
    old: regex::bytes::Regex,
    new: Regex,
}

struct UnicodeClass(Vec<(char, char)>);

impl UnicodeClass {
    fn new(source: &str) -> Self {
        let hir = regex_syntax::parse(source).expect("the class parses");
        let HirKind::Class(Class::Unicode(class)) = hir.kind() else {
            panic!("{source} is a Unicode class");
        };

        Self(
            class
                .ranges()
                .iter()
                .map(|range| (range.start(), range.end()))
                .collect(),
        )
    }

    fn contains(&self, char: char) -> bool {
        self.0
            .binary_search_by(|&(start, end)| {
                if end < char {
                    Ordering::Less
                } else if start > char {
                    Ordering::Greater
                } else {
                    Ordering::Equal
                }
            })
            .is_ok()
    }
}

struct Patterns {
    headers: Vec<HeaderPair>,
    old_unopened: regex::Regex,
    old_id_continue: UnicodeClass,
    old_unassigned: UnicodeClass,
}

impl Patterns {
    fn new() -> Self {
        let sources = [GENERATED_TAG_SOURCE]
            .into_iter()
            .chain(GENERATED_MARKER_SOURCES)
            .chain([GENERATOR_ATTRIBUTION_SOURCE, EDIT_WARNING_SOURCE]);

        let headers = sources
            .map(|source| HeaderPair {
                source,
                old: regex::bytes::Regex::new(
                    &source.replace("{any}", ANY).replace(r"\b", r"(?-u:\b)"),
                )
                .expect("the old header pattern compiles"),
                new: pattern(source),
            })
            .collect();

        Self {
            headers,
            old_unopened: regex::Regex::new(UNOPENED_DIRECTORY_SOURCE)
                .expect("the old warning pattern compiles"),
            old_id_continue: UnicodeClass::new(r"\p{ID_Continue}"),
            old_unassigned: UnicodeClass::new(r"\p{Cn}"),
        }
    }

    fn headers_agree(&self, text: &str) {
        for form in [text.to_owned(), text.to_ascii_lowercase()] {
            for pair in &self.headers {
                let old = pair.old.find(form.as_bytes()).map(|found| found.range());
                let new = pair.new.find(&form).map(|found| found.range());
                assert_eq!(old, new, "{} on {form:?}", pair.source);
            }
        }
    }

    fn warning_agrees(&self, line: &str) -> bool {
        let old: Option<Vec<_>> = self.old_unopened.captures(line).map(|captures| {
            captures
                .iter()
                .map(|group| group.map(|group| group.range()))
                .collect()
        });

        let new: Option<Vec<_>> = UNOPENED_DIRECTORY.captures(line).map(|captures| {
            captures
                .iter()
                .map(|group| group.map(|group| group.range()))
                .collect()
        });

        assert_eq!(old, new, "warning {line:?}");
        old.is_some()
    }

    fn identifier_agrees(&self, text: &str) {
        let first = text.chars().next();
        let old = first.is_some_and(|char| self.old_id_continue.contains(char));
        if old != continues_identifier(text) {
            assert!(
                first.is_some_and(|char| self.old_unassigned.contains(char)),
                "identifier continuation differs on assigned {text:?}"
            );
        }
    }

    fn identifiers_agree(&self, text: &str) {
        for (index, _) in text.char_indices() {
            self.identifier_agrees(&text[index..]);
        }
    }

    fn all_agree(&self, text: &str) {
        self.headers_agree(text);
        self.warning_agrees(text);
        self.identifiers_agree(text);
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

    let listed = run_git_with(&root.to_string_lossy(), &args, None, GitEnv::Inherited)
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

#[test]
fn identifier_continuation_differs_from_the_regex_crate_only_on_unassigned_code_points() {
    let patterns = Patterns::new();
    let old = regex::Regex::new(r"^\p{ID_Continue}").expect("the old pattern compiles");
    let mut buffer = [0; 4];
    for char in (0..=0x10_ffff).filter_map(char::from_u32) {
        let text = char.encode_utf8(&mut buffer);
        if (char as u32).is_multiple_of(61) {
            assert_eq!(
                old.is_match(text),
                patterns.old_id_continue.contains(char),
                "the class table matches the regex crate on {text:?}"
            );
        }

        patterns.identifier_agrees(text);
    }
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

#[cfg(unix)]
fn unopened_directory_warnings() -> Vec<String> {
    use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
    use std::time::{SystemTime, UNIX_EPOCH};

    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("the clock is past the epoch")
        .as_nanos();

    let dir = std::env::temp_dir().join(format!(
        "stanza-regex-parity-{}-{nanos}",
        std::process::id()
    ));

    fs::DirBuilder::new()
        .mode(0o700)
        .create(&dir)
        .expect("a fresh private scratch directory is made");

    let locked = ["nr", "un", "deep/'quoted' dir/é\u{2028}"];
    for name in locked {
        fs::create_dir_all(dir.join(name)).expect("the directory is made");
        fs::write(dir.join(name).join("a.ts"), "export const z = 1;\n")
            .expect("the file is written");
    }

    let cwd = dir.to_string_lossy();
    let git = |args: &[&str]| {
        run_git_with(&cwd, args, None, GitEnv::CLocale).expect("git runs in the scratch repository")
    };

    git(&["init", "-q"]);
    let mode = |mode| {
        for name in locked {
            fs::set_permissions(dir.join(name), fs::Permissions::from_mode(mode))
                .expect("the mode is set");
        }
    };

    mode(0o000);
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
    mode(0o755);
    fs::remove_dir_all(&dir).expect("the scratch directory is removed");
    listed.warnings.lines().map(str::to_owned).collect()
}

#[cfg(not(unix))]
fn unopened_directory_warnings() -> Vec<String> {
    Vec::new()
}

#[test]
fn the_warning_pattern_matches_as_the_regex_crate_did_on_git_warnings() {
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
    "/': ",
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
