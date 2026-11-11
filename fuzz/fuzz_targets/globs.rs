#![no_main]

use std::time::{Duration, Instant};

use libfuzzer_sys::fuzz_target;
use stanza::gitignore::{ignored_by_rules, parse_ignore};
use stanza::languages::javascript::config::bun_glob::bun_match;
use stanza::languages::javascript::config::editor_glob::EditorGlob;
use stanza::languages::javascript::config::glob::{Globs, glob_reach, pattern_list};

const PATTERN_CAP: usize = 1024;
const PATH_CAP: usize = 4096;
const BUDGET: Duration = Duration::from_millis(100);
const ATTEMPTS: usize = 3;

fn timed(compiler: &str, work: impl Fn()) {
    let mut fastest = Duration::MAX;
    for _ in 0..ATTEMPTS {
        let started = Instant::now();
        work();

        fastest = fastest.min(started.elapsed());
        if fastest < BUDGET {
            return;
        }
    }

    panic!("{compiler} took {fastest:?} at best of {ATTEMPTS}");
}

fuzz_target!(|bytes: &[u8]| {
    let Some(split) = bytes.iter().position(|&byte| byte == 0) else {
        return;
    };

    let (pattern, path) = (&bytes[..split], &bytes[split + 1..]);
    if pattern.len() > PATTERN_CAP || path.len() > PATH_CAP {
        return;
    }

    for is_directory in [false, true] {
        timed("gitignore", || {
            ignored_by_rules(&parse_ignore(pattern), path, is_directory);
        });
    }

    let (Ok(pattern), Ok(path)) = (std::str::from_utf8(pattern), std::str::from_utf8(path)) else {
        return;
    };

    timed("editorconfig", || {
        EditorGlob::new(pattern).matches(path);
    });

    timed("bun", || {
        bun_match(pattern, path);
    });

    for legacy in [false, true] {
        timed("pattern list", || {
            let list = pattern_list(&mut Globs::default(), &[pattern.to_owned()], legacy);
            glob_reach(&list, path, None);
        });
    }
});
