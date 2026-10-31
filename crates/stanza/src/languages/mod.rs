pub mod javascript;

use crate::paths::{basename, extname};

use javascript::entry::Entry;

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
