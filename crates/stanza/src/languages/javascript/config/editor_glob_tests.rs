use super::*;
use crate::engine::tests::{LINEAR_GROWTH_CAP, assert_bounded, cpu_ms, ratio};

#[test]
fn numeric_ranges_compare_whole_integers() {
    let glob = EditorGlob::new("{-3..4294967295}.ts");
    assert!(glob.matches("4294967295.ts"));
    assert!(glob.matches("-2.ts"));
    assert!(!glob.matches("4294967296.ts"));
    assert!(!glob.matches("+7.ts"));
    assert!(!glob.matches("07.ts"));
}

#[test]
fn a_greedy_capture_is_not_retried_after_the_range_check() {
    let glob = EditorGlob::new("{1..3}*.ts");
    assert!(glob.matches("1x.ts"));
    assert!(!glob.matches("12.ts"));
}

#[test]
fn classes_follow_javascript_class_syntax() {
    assert!(EditorGlob::new("[!a].ts").matches("b.ts"));
    assert!(EditorGlob::new("[\\d].ts").matches("7.ts"));
    assert!(EditorGlob::new("[a-].ts").matches("-.ts"));
    assert!(!EditorGlob::new("?.ts").matches("😀.ts"));
    assert!(EditorGlob::new("??.ts").matches("😀.ts"));
    assert!(!EditorGlob::new("[z-a]").matches("z"));
}

#[test]
fn an_escaped_bracket_keeps_the_class_open() {
    let glob = EditorGlob::new("[a\\]x[0-9].ts");
    assert!(glob.matches("a.ts"));
    assert!(glob.matches("].ts"));
    assert!(glob.matches("[.ts"));
    assert!(!glob.matches("b.ts"));
    assert!(EditorGlob::new("[\\\\].ts").matches("\\.ts"));
}

#[test]
fn globstars_cross_directories() {
    assert!(EditorGlob::new("**/x.ts").matches("x.ts"));
    assert!(EditorGlob::new("/src/**/x.ts").matches("src/a/b/x.ts"));
    assert!(EditorGlob::new("**.ts").matches("a/b.ts"));
    assert!(!EditorGlob::new("*.ts").matches("a/b.ts"));
    assert!(EditorGlob::new("{a,b{c}.ts").matches("b{c.ts"));
}

struct Bounds<'n> {
    node: &'n Node,
    min: usize,
    max: Option<usize>,
    greedy: bool,
}

type Then<'k> = &'k mut dyn FnMut(&mut Reference, usize) -> bool;

struct Reference<'t> {
    text: &'t [u16],
    captures: Vec<Option<(usize, usize)>>,
}

impl Reference<'_> {
    fn sequence(&mut self, nodes: &[Node], at: usize, then: Then) -> bool {
        match nodes.split_first() {
            None => then(self, at),
            Some((first, rest)) => self.node(first, at, &mut |matcher, next| {
                matcher.sequence(rest, next, then)
            }),
        }
    }

    fn word_at(&self, at: usize) -> bool {
        self.text.get(at).copied().is_some_and(is_word)
    }

    fn node(&mut self, node: &Node, at: usize, then: Then) -> bool {
        let found = self.text.get(at).copied();
        match node {
            Node::Unit(expected) => found == Some(*expected) && then(self, at + 1),
            Node::Any => {
                found.is_some_and(|found| !is_line_terminator(found)) && then(self, at + 1)
            }
            Node::Class(class) => {
                found.is_some_and(|found| class.matches(found)) && then(self, at + 1)
            }
            Node::Start => at == 0 && then(self, at),
            Node::End => at == self.text.len() && then(self, at),
            Node::Boundary(wanted) => {
                let boundary = (at > 0 && self.word_at(at - 1)) != self.word_at(at);
                boundary == *wanted && then(self, at)
            }
            Node::Group {
                capture,
                alternatives,
            } => alternatives.iter().any(|alternative| {
                let saved = self.captures.clone();
                let matched = self.sequence(alternative, at, &mut |matcher, end| {
                    let previous = capture.map(|index| matcher.captures[index]);
                    if let Some(index) = *capture {
                        matcher.captures[index] = Some((at, end));
                    }

                    then(matcher, end) || {
                        if let (Some(index), Some(previous)) = (*capture, previous) {
                            matcher.captures[index] = previous;
                        }

                        false
                    }
                });

                matched || {
                    self.captures = saved;
                    false
                }
            }),
            Node::Repeat {
                node,
                min,
                max,
                greedy,
            } => {
                let bounds = Bounds {
                    node,
                    min: *min,
                    max: *max,
                    greedy: *greedy,
                };

                self.repeat(&bounds, 0, at, then)
            }
        }
    }

    fn repeat(&mut self, bounds: &Bounds, count: usize, at: usize, then: Then) -> bool {
        let again = |matcher: &mut Self, then: Then| {
            bounds.max.is_none_or(|max| count < max)
                && matcher.node(bounds.node, at, &mut |matcher, next| {
                    (count < bounds.min || next != at)
                        && matcher.repeat(bounds, count + 1, next, then)
                })
        };

        if count < bounds.min {
            return again(self, then);
        }

        if bounds.greedy {
            return again(self, then) || then(self, at);
        }

        then(self, at) || again(self, then)
    }
}

fn reference_captures(source: &[u16], text: &[u16]) -> Option<Vec<Option<(usize, usize)>>> {
    let (alternatives, groups) = parse(source)?;
    let mut matcher = Reference {
        text,
        captures: vec![None; groups],
    };

    let mut accept = |_: &mut Reference, _: usize| true;
    let matched = (0..=text.len()).any(|at| {
        matcher.captures.fill(None);
        alternatives
            .iter()
            .any(|alternative| matcher.sequence(alternative, at, &mut accept))
    });

    matched.then_some(matcher.captures)
}

fn reference(pattern: &str, target: &str) -> bool {
    let (regex, ranges) = regex(pattern);
    let text: Vec<u16> = target.encode_utf16().collect();
    reference_captures(&regex, &text)
        .is_some_and(|captures| within_ranges(&ranges, &captures, &text))
}

const TOKENS: &[(&str, &[&str])] = &[
    ("*", &["", "a", "b1", "a.b"]),
    ("**", &["", "a/b", "1"]),
    ("**/", &["", "a/", "a/b/"]),
    ("?", &["a", "1", "/"]),
    ("[ab]", &["a", "b", "c"]),
    ("[!a]", &["a", "b"]),
    ("[a-c]", &["b", "d"]),
    ("[0-9]", &["7", "a"]),
    ("[a\\]", &["a", "]", "\\"]),
    ("{a,b}", &["a", "b", "c"]),
    ("{,a}", &["", "a"]),
    ("{a,[\\]}", &["a", "\\", "]"]),
    ("{a,b{c}", &["a", "b{c"]),
    ("{a,a1}", &["a", "a1"]),
    ("{,1}", &["", "1"]),
    ("{1..5}", &["1", "12", "5"]),
    ("{1..9}", &["1", "9", "10", "05"]),
    ("{-3..12}", &["-3", "12", "13", "0", "-0"]),
    ("{9..1}", &["5", "0"]),
    ("a", &["a"]),
    ("b", &["b"]),
    ("1", &["1"]),
    ("/", &["/"]),
    (".", &["."]),
    ("-", &["-"]),
    ("]", &["]"]),
    ("{", &["{"]),
    ("}", &["}"]),
    (",", &[","]),
    ("\\", &["\\"]),
    ("[", &["["]),
];

const PIECES: &[&str] = &["a", "b", "1", "2", "0", "10", "/", ".", "-", "]", "\\", "{"];

struct Rng(u64);

impl Rng {
    fn below(&mut self, bound: usize) -> usize {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        (self.0 % bound as u64) as usize
    }

    fn pick<'a>(&mut self, items: &[&'a str]) -> &'a str {
        items[self.below(items.len())]
    }
}

fn pair(rng: &mut Rng) -> (String, String) {
    let mut pattern = String::from(if rng.below(4) == 0 { "/" } else { "" });
    let mut instance = String::new();
    for _ in 0..=rng.below(6) {
        let (token, samples) = TOKENS[rng.below(TOKENS.len())];
        pattern.push_str(token);
        instance.push_str(rng.pick(samples));
    }

    if rng.below(2) == 0 {
        return (pattern, instance);
    }

    let path = (0..rng.below(9)).map(|_| rng.pick(PIECES)).collect();
    (pattern, path)
}

#[test]
fn matching_agrees_with_the_backtracking_matcher() {
    let patterns = [
        "{-3..4294967295}.ts",
        "{1..3}*.ts",
        "[!a].ts",
        "[\\d].ts",
        "[a-].ts",
        "?.ts",
        "??.ts",
        "[z-a]",
        "[a\\]x[0-9].ts",
        "[\\\\].ts",
        "**/x.ts",
        "/src/**/x.ts",
        "**.ts",
        "*.ts",
        "{a,b{c}.ts",
        "*",
        "**/file{1..3}.ts",
        "/foo.ts",
        "/src/{a,b}[!0-9]?.ts",
        "{-2..9}.ts",
        "{0..4294967295}.ts",
        "{a,[\\]}]{a,[\\]}{1..3}{5..6}",
        "{a,a1}{1..5}",
        "{,1}{1..5}",
    ];

    let targets = [
        "4294967295.ts",
        "-2.ts",
        "4294967296.ts",
        "+7.ts",
        "07.ts",
        "1x.ts",
        "12.ts",
        "a.ts",
        "b.ts",
        "7.ts",
        "-.ts",
        "😀.ts",
        "z",
        "].ts",
        "[.ts",
        "\\.ts",
        "x.ts",
        "src/a/b/x.ts",
        "a/b.ts",
        "b{c.ts",
        "file2.ts",
        "a/file3.ts",
        "file4.ts",
        "foo.ts",
        "src/foo.ts",
        "src/ab.ts",
        "src/a1.ts",
        "9.ts",
        "10.ts",
        "0.ts",
        "a2",
        "a5",
        "a12",
        "15",
        "",
    ];

    for pattern in patterns {
        let glob = EditorGlob::new(pattern);
        for target in targets {
            assert_eq!(
                glob.matches(target),
                reference(pattern, target),
                "{pattern} against {target}"
            );
        }
    }

    let mut rng = Rng(0x2280_5eed_c0ff_ee01);
    let mut matched = 0;
    for _ in 0..10_000 {
        let (pattern, path) = pair(&mut rng);
        let expected = reference(&pattern, &path);
        assert_eq!(
            EditorGlob::new(&pattern).matches(&path),
            expected,
            "{pattern} against {path}"
        );
        matched += usize::from(expected);
    }

    assert!(
        (1_000..9_000).contains(&matched),
        "{matched} of 10000 matched"
    );
}

#[test]
fn stars_with_no_match_stay_linear() {
    let name = format!("{}.ts", "a".repeat(60));
    assert_bounded(
        "stars",
        &[8, 32],
        5,
        ratio(LINEAR_GROWTH_CAP),
        |count| EditorGlob::new(&format!("{}b", "*a".repeat(count))),
        |glob| {
            cpu_ms(|| {
                for _ in 0..500 {
                    assert!(!glob.matches(&name));
                }
            })
        },
    );
}

const REGEX_TOKENS: &[&str] = &[
    "a", "b", ".", "\\d", "[ab]", "(", "(?:", ")", "|", "*", "+", "?", "*?", "+?", "??", "\\b",
    "\\B", "^", "$", "(?:|a)", "(b?)",
];

#[test]
fn lowered_programs_capture_what_the_backtracking_matcher_captures() {
    let mut rng = Rng(0x0228_ca97_0e5f_0001);
    let mut compared = 0;
    let mut nullable_loops = 0;
    while compared < 3_000 {
        let source: String = (0..=rng.below(8)).map(|_| rng.pick(REGEX_TOKENS)).collect();
        let units: Vec<u16> = source.encode_utf16().collect();
        let Some((alternatives, groups)) = parse(&units) else {
            continue;
        };

        let program = Program::lower(&alternatives, groups);
        nullable_loops += usize::from(program.registers > 0);
        for _ in 0..8 {
            let text: String = (0..rng.below(7))
                .map(|_| rng.pick(&["a", "b", "1", " "]))
                .collect();
            let text: Vec<u16> = text.encode_utf16().collect();
            assert_eq!(
                program.captures(&text),
                reference_captures(&units, &text),
                "{source} against {}",
                String::from_utf16_lossy(&text)
            );
        }

        compared += 1;
    }

    assert!(
        nullable_loops > 100,
        "{nullable_loops} programs had nullable loops"
    );
}
