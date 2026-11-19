use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use super::layers::{Mode, Name, Reach};
use crate::languages::javascript::entry::ENTRY;
use crate::paths::extname;

const EXPANSION_LIMIT: usize = 256;
// Comma groups reach 256 alternatives through at most 510 candidates, none longer than the pattern.
const EXPANSION_BYTES_PER_BYTE: usize = 2 * EXPANSION_LIMIT;
// Nine comma groups already pass 256 alternatives, so only one-part or nested groups go deeper.
const EXPANSION_DEPTH: usize = 64;

enum Segment {
    Any,
    Literal(Vec<char>),
}

struct CompiledGlob {
    directory: Vec<Segment>,
    filename: Vec<char>,
    filenames: Vec<Reach>,
}

fn wildcard(pattern: &[char], text: &[char]) -> bool {
    let (mut at, mut from) = (0, 0);
    let mut star: Option<(usize, usize)> = None;
    while from < text.len() {
        if at < pattern.len() && pattern[at] != '*' && pattern[at] == text[from] {
            at += 1;
            from += 1;
        } else if at < pattern.len() && pattern[at] == '*' {
            star = Some((at, from));
            at += 1;
        } else if let Some((star_at, star_from)) = star {
            at = star_at + 1;
            from = star_from + 1;
            star = Some((star_at, star_from + 1));
        } else {
            return false;
        }
    }

    pattern[at..].iter().all(|&char| char == '*')
}

fn segments_match(pattern: &[Segment], dirs: &[Vec<char>]) -> bool {
    let mut reachable = vec![false; dirs.len() + 1];
    reachable[0] = true;
    for segment in pattern {
        let mut next = vec![false; dirs.len() + 1];
        for at in 0..=dirs.len() {
            if !reachable[at] {
                continue;
            }

            match segment {
                Segment::Any => {
                    next[at] = true;
                    let mut end = at;
                    while end < dirs.len() && !dirs[end].is_empty() {
                        end += 1;
                        next[end] = true;
                    }
                }
                Segment::Literal(chars) => {
                    if at < dirs.len() && wildcard(chars, &dirs[at]) {
                        next[at + 1] = true;
                    }
                }
            }
        }

        reachable = next;
    }

    reachable[dirs.len()]
}

struct Entry {
    negated: bool,
    glob: Rc<CompiledGlob>,
}

pub struct PatternList {
    entries: Vec<Entry>,
    uncertain: bool,
    negated: bool,
    directories: RefCell<HashMap<String, Rc<[bool]>>>,
}

impl PatternList {
    pub const fn names_decide(&self) -> bool {
        !self.uncertain
    }

    fn directory_matches(&self, rel_dir: &str) -> Rc<[bool]> {
        if let Some(known) = self.directories.borrow().get(rel_dir) {
            return known.clone();
        }

        let dirs: Vec<Vec<char>> = if rel_dir.is_empty() {
            Vec::new()
        } else {
            rel_dir
                .split('/')
                .map(|dir| dir.chars().collect())
                .collect()
        };

        let matches: Rc<[bool]> = self
            .entries
            .iter()
            .map(|entry| segments_match(&entry.glob.directory, &dirs))
            .collect();

        self.directories
            .borrow_mut()
            .insert(rel_dir.to_owned(), matches.clone());

        matches
    }
}

#[derive(Default)]
pub struct Globs {
    compiled: HashMap<String, Rc<CompiledGlob>>,
    lists: HashMap<(Vec<String>, Mode), Rc<PatternList>>,
}

fn expand(pattern: &str, limit: isize) -> Option<Vec<String>> {
    let mut budget = pattern.len().saturating_mul(EXPANSION_BYTES_PER_BYTE);
    expand_within(pattern, limit, &mut budget, EXPANSION_DEPTH)
}

fn expand_within(
    pattern: &str,
    limit: isize,
    budget: &mut usize,
    depth: usize,
) -> Option<Vec<String>> {
    let Some((open, close)) = innermost_braces(pattern) else {
        return Some(vec![pattern.to_owned()]);
    };

    let depth = depth.checked_sub(1)?;
    let mut result: Vec<String> = Vec::new();
    for part in pattern[open + 1..close].split(',') {
        let length = pattern.len() - (close + 1 - open) + part.len();
        *budget = budget.checked_sub(length)?;
        let candidate = format!("{}{part}{}", &pattern[..open], &pattern[close + 1..]);
        let expanded = expand_within(&candidate, limit - result.len() as isize, budget, depth)?;
        if (result.len() + expanded.len()) as isize > limit {
            return None;
        }

        result.extend(expanded);
    }

    Some(result)
}

fn innermost_braces(pattern: &str) -> Option<(usize, usize)> {
    let bytes = pattern.as_bytes();
    let mut open = None;
    for (index, &byte) in bytes.iter().enumerate() {
        match byte {
            b'{' => open = Some(index),
            b'}' => match open {
                Some(start) if index > start + 1 => return Some((start, index)),
                _ => open = None,
            },
            _ => {}
        }
    }

    None
}

fn filename_reach(pattern: &str, extension: &str) -> Reach {
    if pattern == "*" || pattern == "**" || pattern == format!("*{extension}") {
        return Reach::All;
    }

    let Some(star) = pattern.rfind('*') else {
        return if extname(pattern) == extension {
            Reach::Some
        } else {
            Reach::None
        };
    };

    let ending = &pattern[star + 1..];
    if ending.contains('.') && !ending.ends_with(extension) && !extension.ends_with(ending) {
        return Reach::None;
    }

    Reach::Some
}

fn compile(globs: &mut Globs, pattern: &str) -> Rc<CompiledGlob> {
    if let Some(cached) = globs.compiled.get(pattern) {
        return cached.clone();
    }

    let mut segments: Vec<&str> = pattern.split('/').collect();
    let filename = segments.pop().unwrap_or_default();
    if filename == "**" {
        segments.push("**");
    }

    let directory = segments
        .iter()
        .map(|&segment| {
            if segment == "**" {
                Segment::Any
            } else {
                Segment::Literal(segment.chars().collect())
            }
        })
        .collect();

    let glob = Rc::new(CompiledGlob {
        directory,
        filename: filename.chars().collect(),
        filenames: ENTRY
            .extensions
            .iter()
            .map(|extension| filename_reach(filename, extension))
            .collect(),
    });

    globs.compiled.insert(pattern.to_owned(), glob.clone());
    glob
}

fn uncertain_pattern(pattern: &str, mode: Mode) -> bool {
    let pattern = pattern.strip_prefix('!').unwrap_or(pattern);
    if pattern.contains(['?', '[', ']', '(', ')', '+', '@', '!', '\\'])
        || unbalanced_braces(pattern)
    {
        return true;
    }

    match mode {
        Mode::Anchored | Mode::Legacy => brace_group_without_comma(pattern),
        Mode::Biome => {
            partial_globstar(pattern) || nested_brace_group(pattern) || empty_alternative(pattern)
        }
        Mode::Oxlint | Mode::Biome1 => false,
    }
}

fn unbalanced_braces(pattern: &str) -> bool {
    let mut depth = 0usize;
    for char in pattern.chars() {
        match char {
            '{' => depth += 1,
            '}' if depth == 0 => return true,
            '}' => depth -= 1,
            _ => {}
        }
    }

    depth > 0
}

fn empty_alternative(pattern: &str) -> bool {
    ["{,", ",,", ",}", "{}"]
        .iter()
        .any(|empty| pattern.contains(empty))
}

fn partial_globstar(pattern: &str) -> bool {
    pattern
        .split('/')
        .any(|segment| segment.contains("**") && segment != "**")
}

fn nested_brace_group(pattern: &str) -> bool {
    let mut depth = 0usize;
    for char in pattern.chars() {
        match char {
            '{' if depth > 0 => return true,
            '{' => depth += 1,
            '}' => depth = depth.saturating_sub(1),
            _ => {}
        }
    }

    false
}

fn brace_group_without_comma(pattern: &str) -> bool {
    let mut groups: Vec<bool> = Vec::new();
    for char in pattern.chars() {
        match char {
            '{' => groups.push(false),
            ',' => {
                if let Some(comma) = groups.last_mut() {
                    *comma = true;
                }
            }
            '}' if groups.pop() == Some(false) => return true,
            _ => {}
        }
    }

    false
}

fn dot_or_empty_segment(pattern: &str) -> bool {
    pattern
        .strip_prefix("./")
        .unwrap_or(pattern)
        .split('/')
        .any(|segment| matches!(segment, "" | "." | ".."))
}

pub fn pattern_list(globs: &mut Globs, patterns: &[String], mode: Mode) -> Rc<PatternList> {
    let key = (patterns.to_vec(), mode);
    if let Some(cached) = globs.lists.get(&key) {
        return cached.clone();
    }

    let mut expanded: Option<Vec<(bool, String, bool)>> = Some(Vec::new());
    for pattern in patterns {
        let Some(found) = expanded.as_mut() else {
            break;
        };

        let (negated, body) = pattern
            .strip_prefix('!')
            .map_or((false, pattern.as_str()), |body| (true, body));

        match expand(body, EXPANSION_LIMIT as isize - found.len() as isize) {
            Some(alternatives) => found.extend(alternatives.into_iter().map(|alternative| {
                let by_name = match mode {
                    Mode::Legacy => !alternative.contains('/'),
                    Mode::Oxlint => !body.contains('/'),
                    _ => false,
                };

                (negated, alternative, by_name)
            })),
            None => expanded = None,
        }
    }

    let negated = patterns.iter().any(|pattern| pattern.starts_with('!'));
    let minimatch = matches!(mode, Mode::Anchored | Mode::Legacy);
    let uncertain = expanded.as_ref().is_none_or(|expanded| {
        minimatch
            && expanded
                .iter()
                .any(|(_, pattern, _)| dot_or_empty_segment(pattern))
    }) || (negated && mode != Mode::Biome)
        || patterns
            .iter()
            .any(|pattern| uncertain_pattern(pattern, mode));

    let entries = match expanded {
        Some(expanded) if !uncertain => expanded
            .iter()
            .map(|(negated, input, by_name)| {
                let pattern = input.strip_prefix("./").unwrap_or(input);
                let glob = if *by_name {
                    compile(globs, &format!("**/{pattern}"))
                } else {
                    compile(globs, pattern)
                };

                Entry {
                    negated: *negated,
                    glob,
                }
            })
            .collect(),
        _ => Vec::new(),
    };

    let list = Rc::new(PatternList {
        entries,
        uncertain,
        negated,
        directories: RefCell::default(),
    });

    globs.lists.insert(key, list.clone());
    list
}

pub fn glob_reach(list: &PatternList, rel_dir: &str, name: &Name) -> Reach {
    if list.uncertain {
        return Reach::Some;
    }

    let directories = list.directory_matches(rel_dir);
    let mut candidates = list
        .entries
        .iter()
        .zip(directories.iter())
        .filter(|(_, matched)| **matched)
        .map(|(entry, _)| entry);

    match name {
        Name::File(name) => {
            let name: Vec<char> = name.chars().collect();
            let last = candidates.rfind(|entry| wildcard(&entry.glob.filename, &name));

            if last.is_some_and(|entry| !entry.negated) {
                Reach::All
            } else {
                Reach::None
            }
        }
        Name::Extension(extension) => extension_reach(
            list,
            candidates,
            ENTRY.extensions.iter().position(|known| known == extension),
        ),
        Name::Any => extension_reach(list, candidates, None),
    }
}

fn extension_reach<'a>(
    list: &PatternList,
    candidates: impl Iterator<Item = &'a Entry>,
    index: Option<usize>,
) -> Reach {
    let considered: Vec<usize> = match index {
        Some(index) => vec![index],
        None => (0..ENTRY.extensions.len()).collect(),
    };

    let matching: Vec<&CompiledGlob> = candidates
        .filter(|entry| !entry.negated)
        .map(|entry| entry.glob.as_ref())
        .collect();

    let all = considered
        .iter()
        .all(|&at| matching.iter().any(|glob| glob.filenames[at] == Reach::All));

    let some = matching.iter().any(|glob| {
        considered
            .iter()
            .any(|&at| glob.filenames[at] != Reach::None)
    });

    if all && !list.negated {
        Reach::All
    } else if some {
        Reach::Some
    } else {
        Reach::None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn reach(patterns: &[&str], dir: &str, mode: Mode, extension: Option<&str>) -> Reach {
        let patterns: Vec<String> = patterns.iter().map(|&pattern| pattern.to_owned()).collect();
        let list = pattern_list(&mut Globs::default(), &patterns, mode);
        let name = extension.map_or(Name::Any, |extension| Name::Extension(extension.to_owned()));
        glob_reach(&list, dir, &name)
    }

    #[test]
    fn expansion_stops_at_the_limit() {
        let wide = format!("{{{}}}", vec!["a"; 257].join(","));
        assert_eq!(expand(&wide, 256), None);
        assert_eq!(expand("{a,b}/{c,d}", 256).map(|found| found.len()), Some(4));
        assert_eq!(
            expand("{a{b,c}}", 256),
            Some(vec!["ab".to_owned(), "ac".to_owned()])
        );
        assert_eq!(expand("{}{a}", 256), Some(vec!["{}a".to_owned()]));
    }

    fn file_reach(patterns: &[&str], dir: &str, mode: Mode, name: &str) -> Reach {
        let patterns: Vec<String> = patterns.iter().map(|&pattern| pattern.to_owned()).collect();
        let list = pattern_list(&mut Globs::default(), &patterns, mode);
        glob_reach(&list, dir, &Name::File(name.to_owned()))
    }

    #[test]
    fn a_named_file_takes_the_last_biome_pattern_it_matches() {
        for (patterns, path, expected) in [
            (&["src/**", "!src/a.ts"][..], "src/a.ts", Reach::None),
            (&["src/**", "!src/a.ts"][..], "src/b.ts", Reach::All),
            (&["!src/a.ts", "src/**"][..], "src/a.ts", Reach::All),
            (
                &["src/**", "!src/*.ts", "src/a.ts"][..],
                "src/a.ts",
                Reach::All,
            ),
            (
                &["src/**", "!src/*.ts", "src/a.ts"][..],
                "src/b.ts",
                Reach::None,
            ),
            (&["*.config.ts"][..], "src/vite.config.ts", Reach::None),
            (&["src/**", "!src/gen"][..], "src/gen/a.ts", Reach::All),
            (&["src/**", "!src/gen/"][..], "src/gen/a.ts", Reach::All),
            (&["src/**", "!src/*"][..], "src/x/a.ts", Reach::All),
            (&["src/**", "!src/gen/**"][..], "src/gen/a.ts", Reach::None),
        ] {
            let (dir, name) = path.rsplit_once('/').unwrap();
            assert_eq!(
                file_reach(patterns, dir, Mode::Biome, name),
                expected,
                "{patterns:?} {path}"
            );
        }
    }

    #[test]
    fn a_named_file_matches_starless_oxlint_and_legacy_patterns_exactly() {
        for mode in [Mode::Legacy, Mode::Oxlint] {
            for (pattern, dir, name, expected) in [
                ("src/a.ts", "src", "a.ts", Reach::All),
                ("src/a.ts", "src", "b.ts", Reach::None),
                ("a.ts", "src/deep", "a.ts", Reach::All),
                ("a.ts", "src", "a.js", Reach::None),
                ("src", "src", "a.ts", Reach::None),
                ("src/a", "src/a", "x.ts", Reach::None),
                ("**/*.config.ts", "src", "vite.config.ts", Reach::All),
                ("**/*.config.ts", "src", "a.ts", Reach::None),
            ] {
                assert_eq!(
                    file_reach(&[pattern], dir, mode, name),
                    expected,
                    "{pattern} {dir}/{name}"
                );
            }
        }
    }

    #[test]
    fn oxlint_anchors_a_brace_group_by_its_whole_pattern() {
        for (pattern, path, expected) in [
            ("{a.ts,src/a.ts}", "a.ts", Reach::All),
            ("{a.ts,src/a.ts}", "src/a.ts", Reach::All),
            ("{a.ts,src/a.ts}", "lib/a.ts", Reach::None),
            ("{a,b}.ts", "lib/a.ts", Reach::All),
            ("src/**a.ts", "src/xa.ts", Reach::All),
            ("src/**a.ts", "src/d/a.ts", Reach::None),
            ("src/a\\.ts", "src/a.ts", Reach::Some),
        ] {
            let (dir, name) = path.rsplit_once('/').unwrap_or(("", path));
            assert_eq!(
                file_reach(&[pattern], dir, Mode::Oxlint, name),
                expected,
                "{pattern} {path}"
            );
        }
    }

    #[test]
    fn biome_patterns_biome_rejects_stay_uncertain() {
        for pattern in [
            "src/**a.ts",
            "src/a\\.ts",
            "src/{a,{b,c}}.ts",
            "src/{,b}a.ts",
            "src/{a.ts",
        ] {
            assert_eq!(
                file_reach(&[pattern], "src", Mode::Biome, "a.ts"),
                Reach::Some,
                "{pattern}"
            );
        }

        assert_eq!(
            file_reach(&["src/{a,b}.ts"], "src", Mode::Biome, "a.ts"),
            Reach::All
        );
        assert_eq!(
            file_reach(&["src/{a,b}.ts"], "src", Mode::Biome, "c.ts"),
            Reach::None
        );
    }

    #[test]
    fn a_settled_folder_answer_holds_for_every_file_in_it() {
        let lists: [&[&str]; 18] = [
            &["*"],
            &["**"],
            &["*.ts"],
            &["**/*.ts"],
            &["src/**"],
            &["src/*.ts"],
            &["**/*.config.ts"],
            &["*.config.ts"],
            &["src/a.ts"],
            &["a.ts"],
            &["{a,b}.ts"],
            &["src/{a,b}.ts"],
            &["*.d.ts"],
            &["*.tsx"],
            &["src/**/*.test.ts"],
            &["src/**", "!src/a.ts"],
            &["**", "!src/gen/**"],
            &["!src/a.ts", "src/**"],
        ];

        let names = [
            "a.ts",
            "b.ts",
            ".x.ts",
            "a.config.ts",
            "a.test.ts",
            "a.d.ts",
            "a.tsx",
            "a.js",
            "x.mts",
        ];

        for patterns in lists {
            let patterns: Vec<String> =
                patterns.iter().map(|&pattern| pattern.to_owned()).collect();
            for mode in [Mode::Anchored, Mode::Legacy, Mode::Oxlint, Mode::Biome] {
                let list = pattern_list(&mut Globs::default(), &patterns, mode);
                for dir in ["", "src", "src/gen", "lib"] {
                    for name in names {
                        let folder =
                            glob_reach(&list, dir, &Name::Extension(extname(name).to_owned()));
                        if folder != Reach::Some {
                            assert_eq!(
                                glob_reach(&list, dir, &Name::File(name.to_owned())),
                                folder,
                                "{patterns:?} {mode:?} {dir}/{name}"
                            );
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn uncertain_characters_reach_some() {
        for pattern in [
            "src/?.ts",
            "src/[ab].ts",
            "src/(a).ts",
            "src/+(a).ts",
            "src/@(a).ts",
            "src/!(a).ts",
        ] {
            for negated in [pattern.to_owned(), format!("!{pattern}")] {
                for mode in [Mode::Anchored, Mode::Legacy, Mode::Biome] {
                    assert_eq!(
                        reach(&[&negated], "src", mode, None),
                        Reach::Some,
                        "{negated}"
                    );
                }
            }
        }
    }

    #[test]
    fn too_many_brace_alternatives_are_uncertain_instead_of_expanded() {
        let groups = "{a,b}".repeat(30);
        let started = crate::engine::tests::thread_cpu_ms();
        assert_eq!(
            reach(
                &[&format!("src/{groups}/*.ts")],
                "src",
                Mode::Anchored,
                None
            ),
            Reach::Some
        );
        assert_eq!(
            reach(&[&format!("{groups}/**")], "x", Mode::Anchored, None),
            Reach::Some
        );

        let spent = crate::engine::tests::thread_cpu_ms() - started;
        assert!(spent < 200.0, "{spent} ms of CPU");
    }

    #[test]
    fn long_globs_and_regex_characters_match_literally() {
        let long = format!("{}x/**", "a*/".repeat(5000));
        assert_eq!(reach(&[&long], "src", Mode::Anchored, None), Reach::None);
        assert_eq!(
            reach(&["a.b$|c/**"], "a.b$|c/d", Mode::Anchored, None),
            Reach::All
        );
        assert_eq!(
            reach(&["a.b$|c/**"], "aXb$|c", Mode::Anchored, None),
            Reach::None
        );
        assert_eq!(
            reach(&["a/**/b/*.ts"], "a/x/y/b", Mode::Anchored, Some(".ts")),
            Reach::All
        );
        assert_eq!(
            reach(&["a/**/b/*.ts"], "a/b", Mode::Anchored, Some(".ts")),
            Reach::All
        );
        assert_eq!(
            reach(&["a*c/**"], "abbc/d", Mode::Anchored, None),
            Reach::All
        );
        assert_eq!(
            reach(&["a*c/**"], "abbd", Mode::Anchored, None),
            Reach::None
        );
    }

    #[test]
    fn negation_is_uncertain_outside_biome() {
        for mode in [Mode::Anchored, Mode::Legacy, Mode::Biome] {
            assert_eq!(reach(&["**/*.ts"], "src", mode, Some(".ts")), Reach::All);
            assert_eq!(
                reach(&["**/*.ts", "!src/**"], "src", mode, Some(".ts")),
                Reach::Some
            );
        }

        assert_eq!(
            reach(&["!src/**"], "lib", Mode::Biome, Some(".ts")),
            Reach::None
        );
        for mode in [Mode::Anchored, Mode::Legacy] {
            assert_eq!(reach(&["!src/**"], "lib", mode, Some(".ts")), Reach::Some);
            assert_eq!(reach(&["!src/**"], "src", mode, Some(".ts")), Reach::Some);
        }
    }

    #[test]
    fn anchored_and_legacy_patterns() {
        assert_eq!(
            reach(&["src/**"], "src/deep", Mode::Anchored, None),
            Reach::All
        );
        assert_eq!(reach(&["src/**"], "lib", Mode::Anchored, None), Reach::None);
        assert_eq!(
            reach(&["*.ts"], "src", Mode::Anchored, Some(".ts")),
            Reach::None
        );
        assert_eq!(
            reach(&["*.ts"], "src", Mode::Legacy, Some(".ts")),
            Reach::All
        );
        assert_eq!(
            reach(&["*.ts"], "src", Mode::Legacy, Some(".js")),
            Reach::None
        );
        assert_eq!(
            reach(&["a.ts"], "anywhere", Mode::Legacy, Some(".js")),
            Reach::None
        );
        assert_eq!(
            reach(&["a.ts"], "anywhere", Mode::Legacy, Some(".ts")),
            Reach::Some
        );
        assert_eq!(
            reach(&["./src/*.js"], "src", Mode::Anchored, Some(".ts")),
            Reach::None
        );
        assert_eq!(
            reach(&["src/*.{ts,tsx}"], "src", Mode::Anchored, Some(".tsx")),
            Reach::All
        );
    }
}
