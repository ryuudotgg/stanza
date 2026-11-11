#![no_main]

use libfuzzer_sys::fuzz_target;
use stanza::files::diff::{diff_changes, diff_sections};

fuzz_target!(|bytes: &[u8]| {
    let text = String::from_utf8_lossy(bytes);
    for section in diff_sections(&text) {
        diff_changes(&section);
    }
});
