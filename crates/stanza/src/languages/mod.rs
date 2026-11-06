pub mod javascript;

use crate::paths::{basename, extname};

use javascript::entry::Entry;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Layout {
    pub columns: usize,
    pub tab: usize,
}

pub const FALLBACK_LAYOUT: Layout = Layout {
    columns: 80,
    tab: 2,
};

pub fn entry_for(path: &str) -> Option<&'static Entry> {
    let entry = &javascript::entry::ENTRY;
    entry.extensions.contains(&extname(path)).then_some(entry)
}

pub fn is_candidate(relative_path: &str, keep_generated: bool) -> bool {
    entry_for(relative_path).is_some_and(|entry| {
        !(entry.skips_name)(basename(relative_path), keep_generated)
            && !relative_path
                .split(['/', '\\'])
                .any(|segment| entry.directories.contains(&segment))
    })
}

pub fn prunes(relative_directory: &str) -> bool {
    let entry = &javascript::entry::ENTRY;
    relative_directory
        .split(['/', '\\'])
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
        ];

        let rejected = [
            "source/file.d.ts",
            "source/file.gen.ts",
            "source/file.generated.ts",
            "source/file.min.js",
            "node_modules/file.ts",
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
    fn prunes_only_skipped_directory_segments() {
        assert!(prunes("pkg/node_modules"));
        assert!(prunes("dist"));
        assert!(!prunes("pkg/un"));
        assert!(!prunes(""));
    }
}
