use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use super::layers::Reach;
use crate::languages::javascript::entry::ENTRY;
use crate::paths::extname;

const EXPANSION_LIMIT: usize = 256;

enum Segment {
    Any,
    Literal(Vec<char>),
}

struct CompiledGlob {
    directory: Option<Vec<Segment>>,
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

struct PatternList {
    positive: Vec<Rc<CompiledGlob>>,
    uncertain: bool,
    negated: bool,
    reaches: RefCell<HashMap<(String, Option<String>), Reach>>,
}

#[derive(Default)]
struct Globs {
    compiled: HashMap<String, Rc<CompiledGlob>>,
    lists: HashMap<(Vec<String>, bool), Rc<PatternList>>,
}

thread_local! {
    static GLOBS: RefCell<Globs> = RefCell::default();
}

fn expand(pattern: &str, limit: isize) -> Option<Vec<String>> {
    let Some((open, close)) = innermost_braces(pattern) else {
        return Some(vec![pattern.to_owned()]);
    };

    let mut result: Vec<String> = Vec::new();
    for part in pattern[open + 1..close].split(',') {
        let candidate = format!("{}{part}{}", &pattern[..open], &pattern[close + 1..]);
        let expanded = expand(&candidate, limit - result.len() as isize)?;
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
        directory: Some(directory),
        filenames: ENTRY
            .extensions
            .iter()
            .map(|extension| filename_reach(filename, extension))
            .collect(),
    });

    globs.compiled.insert(pattern.to_owned(), glob.clone());
    glob
}

fn uncertain_pattern(pattern: &str) -> bool {
    pattern
        .strip_prefix('!')
        .unwrap_or(pattern)
        .contains(['?', '[', ']', '(', ')', '+', '@', '!'])
}

fn pattern_list(globs: &mut Globs, patterns: &[String], legacy: bool) -> Rc<PatternList> {
    let key = (patterns.to_vec(), legacy);
    if let Some(cached) = globs.lists.get(&key) {
        return cached.clone();
    }

    let mut expanded: Option<Vec<String>> = Some(Vec::new());
    for pattern in patterns.iter().filter(|pattern| !pattern.starts_with('!')) {
        let Some(found) = expanded.as_mut() else {
            break;
        };

        match expand(pattern, EXPANSION_LIMIT as isize - found.len() as isize) {
            Some(alternatives) => found.extend(alternatives),
            None => expanded = None,
        }
    }

    let uncertain = expanded.is_none() || patterns.iter().any(|pattern| uncertain_pattern(pattern));
    let positive = match expanded {
        Some(expanded) if !uncertain => expanded
            .iter()
            .map(|input| {
                let pattern = input.strip_prefix("./").unwrap_or(input);
                if legacy && !pattern.contains('*') {
                    return Rc::new(CompiledGlob {
                        directory: None,
                        filenames: vec![Reach::Some; ENTRY.extensions.len()],
                    });
                }

                if legacy && !pattern.contains('/') {
                    compile(globs, &format!("**/{pattern}"))
                } else {
                    compile(globs, pattern)
                }
            })
            .collect(),
        _ => Vec::new(),
    };

    let list = Rc::new(PatternList {
        positive,
        uncertain,
        negated: patterns.iter().any(|pattern| pattern.starts_with('!')),
        reaches: RefCell::default(),
    });

    globs.lists.insert(key, list.clone());
    list
}

pub fn glob_reach(
    patterns: &[String],
    rel_dir: &str,
    legacy: bool,
    extension: Option<&str>,
) -> Reach {
    let list = GLOBS.with_borrow_mut(|globs| pattern_list(globs, patterns, legacy));
    let key = (rel_dir.to_owned(), extension.map(str::to_owned));
    if let Some(&cached) = list.reaches.borrow().get(&key) {
        return cached;
    }

    let index = extension.and_then(|extension| {
        ENTRY
            .extensions
            .iter()
            .position(|known| *known == extension)
    });

    let considered: Vec<usize> = match index {
        Some(index) => vec![index],
        None => (0..ENTRY.extensions.len()).collect(),
    };

    let reach = if list.uncertain {
        Reach::Some
    } else {
        let dirs: Vec<Vec<char>> = if rel_dir.is_empty() {
            Vec::new()
        } else {
            rel_dir
                .split('/')
                .map(|dir| dir.chars().collect())
                .collect()
        };

        let matching: Vec<&CompiledGlob> = list
            .positive
            .iter()
            .filter(|glob| {
                glob.directory
                    .as_ref()
                    .is_none_or(|pattern| segments_match(pattern, &dirs))
            })
            .map(Rc::as_ref)
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
    };

    list.reaches.borrow_mut().insert(key, reach);
    reach
}

#[cfg(test)]
mod tests {
    use super::*;

    fn reach(patterns: &[&str], dir: &str, legacy: bool, extension: Option<&str>) -> Reach {
        let patterns: Vec<String> = patterns.iter().map(|&pattern| pattern.to_owned()).collect();
        glob_reach(&patterns, dir, legacy, extension)
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
                for legacy in [false, true] {
                    assert_eq!(
                        reach(&[&negated], "src", legacy, None),
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
            reach(&[&format!("src/{groups}/*.ts")], "src", false, None),
            Reach::Some
        );
        assert_eq!(
            reach(&[&format!("{groups}/**")], "x", false, None),
            Reach::Some
        );

        let spent = crate::engine::tests::thread_cpu_ms() - started;
        assert!(spent < 200.0, "{spent} ms of CPU");
    }

    #[test]
    fn long_globs_and_regex_characters_match_literally() {
        let long = format!("{}x/**", "a*/".repeat(5000));
        assert_eq!(reach(&[&long], "src", false, None), Reach::None);
        assert_eq!(reach(&["a.b$|c/**"], "a.b$|c/d", false, None), Reach::All);
        assert_eq!(reach(&["a.b$|c/**"], "aXb$|c", false, None), Reach::None);
        assert_eq!(
            reach(&["a/**/b/*.ts"], "a/x/y/b", false, Some(".ts")),
            Reach::All
        );
        assert_eq!(
            reach(&["a/**/b/*.ts"], "a/b", false, Some(".ts")),
            Reach::All
        );
        assert_eq!(reach(&["a*c/**"], "abbc/d", false, None), Reach::All);
        assert_eq!(reach(&["a*c/**"], "abbd", false, None), Reach::None);
    }

    #[test]
    fn negation_never_means_all() {
        assert_eq!(reach(&["**/*.ts"], "src", false, Some(".ts")), Reach::All);
        assert_eq!(
            reach(&["**/*.ts", "!src/**"], "src", false, Some(".ts")),
            Reach::Some
        );
        assert_eq!(reach(&["!src/**"], "src", false, Some(".ts")), Reach::None);
    }

    #[test]
    fn anchored_and_legacy_patterns() {
        assert_eq!(reach(&["src/**"], "src/deep", false, None), Reach::All);
        assert_eq!(reach(&["src/**"], "lib", false, None), Reach::None);
        assert_eq!(reach(&["*.ts"], "src", false, Some(".ts")), Reach::None);
        assert_eq!(reach(&["*.ts"], "src", true, Some(".ts")), Reach::All);
        assert_eq!(reach(&["*.ts"], "src", true, Some(".js")), Reach::None);
        assert_eq!(reach(&["a.ts"], "anywhere", true, Some(".js")), Reach::Some);
        assert_eq!(
            reach(&["./src/*.js"], "src", false, Some(".ts")),
            Reach::None
        );
        assert_eq!(
            reach(&["src/*.{ts,tsx}"], "src", false, Some(".tsx")),
            Reach::All
        );
    }
}
