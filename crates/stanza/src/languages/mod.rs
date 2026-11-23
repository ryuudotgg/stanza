pub mod javascript;

use crate::engine::doc::Ruler;
use crate::paths::{basename, extname};

use javascript::entry::Entry;

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

pub fn entry_for(path: &str) -> Option<&'static Entry> {
    let entry = &javascript::entry::ENTRY;
    entry.extensions.contains(&extname(path)).then_some(entry)
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
    use super::*;

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
