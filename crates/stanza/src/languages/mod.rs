pub mod javascript;

use std::io;
use std::num::NonZeroUsize;

use crate::engine::doc::Ruler;
use crate::engine::explain::{Line, Opening, Request, Verdict};
use crate::engine::model::{BracePass, Doc, Reads, Rejection, Scan};
use crate::engine::types::{Braces, FileResult, Mode};
use crate::files::Changed;
use crate::paths::{basename, extname};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Limit {
    Columns(usize),
    Unlimited,
}

impl Limit {
    pub const fn holds(self, columns: usize) -> bool {
        match self {
            Self::Columns(limit) => columns <= limit,
            Self::Unlimited => true,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Layout {
    pub limit: Limit,
    pub tab: usize,
    pub ruler: Ruler,
}

pub const FALLBACK_COLUMNS: usize = 80;

pub const FALLBACK_LAYOUT: Layout = Layout {
    limit: Limit::Columns(FALLBACK_COLUMNS),
    tab: 2,
    ruler: Ruler::Prettier,
};

impl Layout {
    pub fn measure(self, line: &str) -> usize {
        self.ruler.measure(line, self.tab)
    }

    pub fn fits(self, line: &str) -> bool {
        self.limit.holds(self.measure(line))
    }
}

pub(crate) type Examined<'a, L> = Scan<<L as Language>::Node<'a>, <L as Language>::Body<'a>>;

pub(crate) trait Language: Sized {
    const EXTENSIONS: &'static [&'static str];
    const DIRECTORIES: &'static [&'static str];

    type Node<'a>: Reads;
    type Body<'a>: Copy;

    fn skips_name(name: &str, keep_generated: bool) -> bool;

    fn anchor(node: &Self::Node<'_>) -> usize;

    fn body_span(body: &Self::Body<'_>) -> (usize, usize);

    fn body_owner(body: &Self::Body<'_>) -> usize;

    fn examine<R>(
        path: &str,
        text: &str,
        layout: Layout,
        then: impl FnOnce(&Doc, &Examined<'_, Self>, Option<String>) -> R,
    ) -> R;

    fn examine_source<R>(
        path: &str,
        text: &str,
        layout: Layout,
        then: impl FnOnce(&Doc, &Examined<'_, Self>) -> R,
    ) -> Result<R, Rejection>;

    fn brace_pass(doc: &Doc, bodies: &[Self::Body<'_>], layout: Layout) -> BracePass;

    fn openings(doc: &Doc, bodies: &[Self::Body<'_>], line: usize) -> Vec<Opening>;

    fn verdict(
        doc: &Doc,
        eligible: &[Self::Body<'_>],
        offset: usize,
        at: &dyn Fn(usize) -> usize,
    ) -> Verdict;

    fn width(path: &str) -> WidthSetting;

    fn braces(path: &str) -> BracesSetting;

    fn brace_reasons(path: &str) -> io::Result<Vec<ConfigHold>>;
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum WidthFrom {
    Config(String),
    Default(&'static str),
    Fallback,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WidthSetting {
    pub layout: Layout,
    pub columns_text: String,
    pub source: WidthFrom,
    pub unread: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BracesSetting {
    pub enforced: bool,
    pub unread: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BraceRule {
    Flag(Braces),
    Config(BracesSetting),
}

impl BraceRule {
    pub fn keeps(&self) -> bool {
        match self {
            Self::Flag(braces) => *braces == Braces::Off,
            Self::Config(setting) => setting.enforced,
        }
    }
}

pub struct ConfigHold {
    pub enforced: bool,
    pub files: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Settings {
    pub width: WidthSetting,
    pub braces: BraceRule,
}

impl Settings {
    pub fn given(layout: Layout, keep_braces: bool) -> Self {
        let columns_text = match layout.limit {
            Limit::Columns(columns) => columns.to_string(),
            Limit::Unlimited => "unlimited".to_owned(),
        };

        Self {
            width: WidthSetting {
                layout,
                columns_text,
                source: WidthFrom::Fallback,
                unread: Vec::new(),
            },
            braces: BraceRule::Flag(if keep_braces { Braces::Off } else { Braces::On }),
        }
    }

    pub const fn layout(&self) -> Layout {
        self.width.layout
    }
}

type Explanations = Result<Vec<Line>, String>;

pub struct Entry {
    pub extensions: &'static [&'static str],
    pub directories: &'static [&'static str],
    pub skips_name: fn(&str, bool) -> bool,
    width: fn(&str) -> WidthSetting,
    braces: fn(&str) -> BracesSetting,
    format: fn(&str, &str, Mode, &Settings, Option<Changed>) -> FileResult,
    explain: fn(&Request, &Settings, &[NonZeroUsize]) -> Explanations,
}

impl Entry {
    pub(crate) const fn of<L: Language>() -> Self {
        Self {
            extensions: L::EXTENSIONS,
            directories: L::DIRECTORIES,
            skips_name: L::skips_name,
            width: L::width,
            braces: L::braces,
            format: crate::engine::process::<L>,
            explain: crate::engine::explain::explain::<L>,
        }
    }

    pub fn width(&self, path: &str) -> WidthSetting {
        (self.width)(path)
    }

    pub fn braces(&self, path: &str, flag: Option<Braces>) -> BraceRule {
        flag.map_or_else(|| BraceRule::Config((self.braces)(path)), BraceRule::Flag)
    }

    pub fn settings(&self, path: &str, flag: Option<Braces>) -> Settings {
        let width = self.width(path);
        let braces = self.braces(path, flag);
        Settings { width, braces }
    }

    pub fn format(
        &self,
        path: &str,
        text: &str,
        mode: Mode,
        settings: &Settings,
        lines: Option<Changed>,
    ) -> FileResult {
        (self.format)(path, text, mode, settings, lines)
    }

    pub fn explain(
        &self,
        request: &Request,
        settings: &Settings,
        lines: &[NonZeroUsize],
    ) -> Explanations {
        (self.explain)(request, settings, lines)
    }
}

pub fn entry_for(path: &str) -> Option<&'static Entry> {
    let entry = &javascript::entry::ENTRY;
    entry.extensions.contains(&extname(path)).then_some(entry)
}

pub fn source_entry(path: &str) -> &'static Entry {
    entry_for(path).unwrap_or(&javascript::entry::ENTRY)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Candidacy {
    No,
    InSkippedFolder,
    Yes,
}

pub fn candidacy(relative_path: &str, keep_generated: bool) -> Candidacy {
    let Some(entry) = entry_for(relative_path) else {
        return Candidacy::No;
    };

    if (entry.skips_name)(basename(relative_path), keep_generated) {
        Candidacy::No
    } else if prunes(relative_path) {
        Candidacy::InSkippedFolder
    } else {
        Candidacy::Yes
    }
}

pub fn is_candidate(relative_path: &str, keep_generated: bool) -> bool {
    candidacy(relative_path, keep_generated) == Candidacy::Yes
}

pub fn prunes(relative_directory: &str) -> bool {
    let entry = &javascript::entry::ENTRY;
    relative_directory
        .split('/')
        .any(|segment| entry.directories.contains(&segment))
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::ops::ControlFlow;

    use super::*;
    use crate::engine::attach::attach;
    use crate::engine::doc::document;
    use crate::engine::model::{Facts, Item, Kind, ListKind, StatementList};

    struct Toy;

    #[derive(Clone, Copy)]
    struct Call;

    #[derive(Clone, Copy)]
    enum NoBody {}

    impl Reads for Call {
        fn references(&self, _: &[&str], _: &mut dyn FnMut(&str) -> ControlFlow<()>) {}

        fn reads_path(&self, _: &Doc, _: &[String]) -> bool {
            false
        }

        fn operation(&self, _: &Doc) -> Option<String> {
            None
        }

        fn finally_repeats(&self, _: &Doc, _: &str) -> bool {
            false
        }
    }

    fn block(doc: &Doc) -> Scan<Call, NoBody> {
        let open = doc.text.find('{').expect("a braced block");
        let close = doc.text.rfind('}').expect("a closing brace");
        let items = doc
            .lines
            .iter()
            .zip(&doc.line_starts)
            .filter(|&(_, &start)| start > open && start < close)
            .filter(|(line, _)| !line.trim().is_empty())
            .map(|(line, &start)| {
                let indent = line.len() - line.trim_start().len();
                Item {
                    node: Call,
                    start: start + indent,
                    end: start + line.trim_end().len(),
                    facts: Facts {
                        kind: Kind::Other,
                        word: None,
                        binds: None,
                        is_let: false,
                        guard: None,
                        compact: true,
                        joined: None,
                        case_body: false,
                    },
                }
            });

        let list = StatementList {
            kind: ListKind::Block,
            start: open,
            end: close + 1,
            braced: true,
            stmts: attach(doc, items, open + 1, close),
        };

        Scan {
            lists: vec![list],
            frozen: Vec::new(),
            bodies: Vec::new(),
            eligible: Vec::new(),
            chains: HashMap::new(),
        }
    }

    impl Language for Toy {
        const EXTENSIONS: &'static [&'static str] = &[".toy"];
        const DIRECTORIES: &'static [&'static str] = &[];

        type Node<'a> = Call;
        type Body<'a> = NoBody;

        fn skips_name(_: &str, _: bool) -> bool {
            false
        }

        fn anchor(_: &Call) -> usize {
            unreachable!("only hunk scoping reads anchors")
        }

        fn body_span(body: &NoBody) -> (usize, usize) {
            match *body {}
        }

        fn body_owner(body: &NoBody) -> usize {
            match *body {}
        }

        fn examine<R>(
            _: &str,
            text: &str,
            _: Layout,
            then: impl FnOnce(&Doc, &Examined<'_, Self>, Option<String>) -> R,
        ) -> R {
            let doc = document(text, Vec::new());
            then(&doc, &block(&doc), None)
        }

        fn examine_source<R>(
            _: &str,
            text: &str,
            _: Layout,
            then: impl FnOnce(&Doc, &Examined<'_, Self>) -> R,
        ) -> Result<R, Rejection> {
            let doc = document(text, Vec::new());
            Ok(then(&doc, &block(&doc)))
        }

        fn brace_pass(_: &Doc, bodies: &[NoBody], _: Layout) -> BracePass {
            assert!(bodies.is_empty(), "a toy block has no controlled bodies");
            BracePass {
                edits: Vec::new(),
                starts: Vec::new(),
            }
        }

        fn openings(_: &Doc, _: &[NoBody], _: usize) -> Vec<Opening> {
            Vec::new()
        }

        fn verdict(_: &Doc, _: &[NoBody], _: usize, _: &dyn Fn(usize) -> usize) -> Verdict {
            unreachable!("a toy block opens no braced body")
        }

        fn width(_: &str) -> WidthSetting {
            WidthSetting {
                layout: FALLBACK_LAYOUT,
                columns_text: FALLBACK_COLUMNS.to_string(),
                source: WidthFrom::Fallback,
                unread: Vec::new(),
            }
        }

        fn braces(_: &str) -> BracesSetting {
            BracesSetting {
                enforced: false,
                unread: Vec::new(),
            }
        }

        fn brace_reasons(_: &str) -> io::Result<Vec<ConfigHold>> {
            Ok(Vec::new())
        }
    }

    static TOY: Entry = Entry::of::<Toy>();

    #[test]
    fn language_entry_drives_format_and_explain() {
        let path = "block.toy";
        let text = "{\n  a();\n\n  b();\n}\n";
        let settings = TOY.settings(path, None);
        let layout = settings.layout();
        assert_eq!(
            (layout.limit, layout.tab),
            (Limit::Columns(80), 2),
            "the toy entry resolves the fallback layout"
        );

        Toy::examine_source(path, text, layout, |doc, scan| {
            let [list] = scan.lists.as_slice() else {
                panic!("one statement list");
            };

            assert_eq!((list.kind, list.braced), (ListKind::Block, true));
            assert_eq!(list.stmts.len(), 2);
            assert!(list.stmts.iter().all(|stmt| stmt.facts.compact));
            assert!(
                Toy::brace_pass(doc, &scan.eligible, layout)
                    .edits
                    .is_empty()
            );
        })
        .unwrap_or_else(|_| panic!("the toy block examines"));

        let result = TOY.format(path, text, Mode::Fix, &settings, None);
        assert!(!result.parse_error);
        assert_eq!(
            result.fixed.map(|fixed| fixed.text).as_deref(),
            Some("{\n  a();\n  b();\n}\n")
        );

        let display = |path: &str| path.to_owned();
        let request = Request {
            path,
            text,
            display: &display,
        };

        let line = NonZeroUsize::new(4).expect("line 4");
        let explained = TOY
            .explain(&request, &settings, &[line])
            .unwrap_or_else(|error| panic!("{error}"))
            .remove(0)
            .unwrap_or_else(|past| panic!("{}", past.message(4)))
            .lines
            .join("\n");

        assert!(explained.contains("short-body"), "{explained}");
        assert!(
            explained.contains("--fix removes 1 blank line"),
            "{explained}"
        );
    }

    #[test]
    fn accepts_supported_extensions_and_skips_generated_paths() {
        let accepted = [
            "source/file.ts",
            "source/file.tsx",
            "source/file.mts",
            "source/file.cts",
            "source/file.js",
            "source/file.jsx",
            "source/file.mjs",
            "source/file.cjs",
            "dist\\a.ts",
            "src\\dist/a.ts",
        ];

        let rejected = [
            "source/file.d.ts",
            "source/file.gen.ts",
            "source/file.generated.ts",
            "source/file.min.js",
            "node_modules/file.ts",
            "src/node_modules/c.ts",
            "source/file.txt",
        ];

        for path in accepted {
            assert!(is_candidate(path, false), "{path}");
        }

        for path in rejected {
            assert!(!is_candidate(path, false), "{path}");
        }
    }

    #[test]
    fn a_name_skip_outranks_a_skipped_folder() {
        let cases = [
            ("src/a.ts", Candidacy::Yes),
            ("dist/a.ts", Candidacy::InSkippedFolder),
            ("src/node_modules/x/a.ts", Candidacy::InSkippedFolder),
            ("dist/a.d.ts", Candidacy::No),
            ("dist/a.gen.ts", Candidacy::No),
            ("dist/a.txt", Candidacy::No),
        ];

        for (path, expected) in cases {
            assert_eq!(candidacy(path, false), expected, "{path}");
        }

        assert_eq!(candidacy("dist/a.gen.ts", true), Candidacy::InSkippedFolder);
        assert_eq!(
            Candidacy::Yes.min(Candidacy::InSkippedFolder),
            Candidacy::InSkippedFolder
        );
        assert_eq!(Candidacy::InSkippedFolder.min(Candidacy::No), Candidacy::No);
    }

    #[test]
    fn prunes_only_skipped_directory_segments() {
        assert!(prunes("pkg/node_modules"));
        assert!(prunes("dist"));
        assert!(prunes("src/node_modules"));
        assert!(!prunes("pkg/un"));
        assert!(!prunes("src\\dist"));
        assert!(!prunes(""));
    }

    #[test]
    fn statement_anchors_unwrap_labels() {
        let text = "outer: inner: if (a) { b(); }\n";
        let anchors = javascript::scan::scanned("a.ts", text, |_, scan| {
            scan.lists
                .iter()
                .flat_map(|list| &list.stmts)
                .map(|stmt| (stmt.start, stmt.node.anchor()))
                .collect::<Vec<_>>()
        });

        let call = text.find("b();").unwrap();
        assert!(
            anchors.contains(&(0, text.find("if").unwrap())),
            "{anchors:?}"
        );
        assert!(anchors.contains(&(call, call)), "{anchors:?}");
    }
}
