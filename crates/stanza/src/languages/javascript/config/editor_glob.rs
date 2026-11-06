use std::cmp::Ordering;

#[derive(Debug)]
enum ClassItem {
    Range(u16, u16),
    Digit(bool),
    Word(bool),
    Space(bool),
}

#[derive(Debug)]
struct Class {
    negated: bool,
    items: Vec<ClassItem>,
}

#[derive(Debug)]
enum Node {
    Unit(u16),
    Any,
    Class(Class),
    Start,
    End,
    Boundary(bool),
    Group {
        capture: Option<usize>,
        alternatives: Vec<Vec<Self>>,
    },
    Repeat {
        node: Box<Self>,
        min: usize,
        max: Option<usize>,
        greedy: bool,
    },
}

#[derive(Debug)]
pub struct EditorGlob {
    nodes: Option<Vec<Vec<Node>>>,
    groups: usize,
    ranges: Vec<(Integer, Integer)>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Integer {
    negative: bool,
    digits: String,
}

impl Integer {
    fn parse(text: &str) -> Option<Self> {
        let unsigned = text.strip_prefix(['+', '-']).unwrap_or(text);
        if unsigned.is_empty() || !unsigned.bytes().all(|byte| byte.is_ascii_digit()) {
            return None;
        }

        let digits = unsigned.trim_start_matches('0');
        Some(Self {
            negative: text.starts_with('-') && !digits.is_empty(),
            digits: digits.to_owned(),
        })
    }
}

impl Ord for Integer {
    fn cmp(&self, other: &Self) -> Ordering {
        let magnitude = self
            .digits
            .len()
            .cmp(&other.digits.len())
            .then_with(|| self.digits.cmp(&other.digits));

        match (self.negative, other.negative) {
            (false, false) => magnitude,
            (true, true) => magnitude.reverse(),
            (true, false) => Ordering::Less,
            (false, true) => Ordering::Greater,
        }
    }
}

impl PartialOrd for Integer {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

const fn unit(byte: u8) -> u16 {
    byte as u16
}

const fn is_line_terminator(unit: u16) -> bool {
    matches!(unit, 0x0a | 0x0d | 0x2028 | 0x2029)
}

fn is_space(unit: u16) -> bool {
    char::from_u32(u32::from(unit)).is_some_and(crate::text::is_js_whitespace)
}

const fn is_digit(unit: u16) -> bool {
    matches!(unit, 0x30..=0x39)
}

const fn is_word(unit: u16) -> bool {
    matches!(unit, 0x30..=0x39 | 0x41..=0x5a | 0x61..=0x7a | 0x5f)
}

fn position(units: &[u16], target: u8, from: usize) -> Option<usize> {
    units
        .get(from..)?
        .iter()
        .position(|&found| found == unit(target))
        .map(|at| at + from)
}

fn range_bounds(content: &[u16]) -> Option<(Integer, Integer)> {
    let text = String::from_utf16(content).ok()?;
    let (first, last) = text.split_once("..")?;
    Some((Integer::parse(first)?, Integer::parse(last)?))
}

fn compile(part: &[u16], ranges: &mut Vec<(Integer, Integer)>) -> Vec<u16> {
    let mut source = Vec::new();
    let push = |source: &mut Vec<u16>, text: &str| source.extend(text.encode_utf16());
    let mut index = 0;
    while index < part.len() {
        let char = part[index];
        let at = |offset: usize| part.get(index + offset).copied();
        if char == unit(b'*') {
            if at(1) == Some(unit(b'*')) {
                if at(2) == Some(unit(b'/')) {
                    push(&mut source, "(?:.*/)?");
                    index += 2;
                } else {
                    push(&mut source, ".*");
                    index += 1;
                }
            } else {
                push(&mut source, "[^/]*");
            }
        } else if char == unit(b'?') {
            push(&mut source, "[^/]");
        } else if char == unit(b'[') {
            match position(part, b']', index + 1) {
                None => push(&mut source, "\\["),
                Some(end) => {
                    let content = &part[index + 1..end];
                    source.push(unit(b'['));
                    match content.split_first() {
                        Some((&first, rest)) if first == unit(b'!') => {
                            source.push(unit(b'^'));
                            source.extend_from_slice(rest);
                        }
                        _ => source.extend_from_slice(content),
                    }

                    source.push(unit(b']'));
                    index = end;
                }
            }
        } else if char == unit(b'{') {
            match position(part, b'}', index + 1) {
                None => push(&mut source, "\\{"),
                Some(end) => {
                    let content = &part[index + 1..end];
                    if let Some(bounds) = range_bounds(content) {
                        ranges.push(bounds);
                        push(&mut source, "(-?(?:0|[1-9]\\d*))");
                    } else {
                        push(&mut source, "(?:");
                        for (at, branch) in content.split(|&found| found == unit(b',')).enumerate()
                        {
                            if at > 0 {
                                source.push(unit(b'|'));
                            }

                            let compiled = compile(branch, ranges);
                            source.extend(compiled);
                        }

                        source.push(unit(b')'));
                    }

                    index = end;
                }
            }
        } else {
            if u8::try_from(char).is_ok_and(|byte| b"\\^$+?.()|{}".contains(&byte)) {
                source.push(unit(b'\\'));
            }

            source.push(char);
        }

        index += 1;
    }

    source
}

struct Parser<'s> {
    source: &'s [u16],
    at: usize,
    groups: usize,
    total: usize,
}

enum Atom {
    Unit(u16),
    Set(ClassItem),
}

fn capturing_groups(source: &[u16]) -> usize {
    let mut count = 0;
    let mut in_class = false;
    let mut at = 0;
    while at < source.len() {
        let char = source[at];
        if char == unit(b'\\') {
            at += 2;
            continue;
        }

        if in_class {
            in_class = char != unit(b']');
        } else if char == unit(b'[') {
            in_class = true;
        } else if char == unit(b'(') && source.get(at + 1) != Some(&unit(b'?')) {
            count += 1;
        }

        at += 1;
    }

    count
}

impl Parser<'_> {
    fn peek(&self) -> Option<u16> {
        self.source.get(self.at).copied()
    }

    fn eat(&mut self, byte: u8) -> bool {
        let found = self.peek() == Some(unit(byte));
        self.at += usize::from(found);
        found
    }

    fn hex(&mut self, length: usize) -> Option<u16> {
        let digits = self.source.get(self.at..self.at + length)?;
        let text = String::from_utf16(digits).ok()?;
        if text.len() != length || !text.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return None;
        }

        self.at += length;
        u16::from_str_radix(&text, 16).ok()
    }

    fn octal(&mut self, first: u16) -> u16 {
        let longest = if first <= unit(b'3') { 2 } else { 1 };
        let mut value = first - unit(b'0');
        for _ in 0..longest {
            match self.peek() {
                Some(digit @ 0x30..=0x37) => {
                    value = value * 8 + (digit - 0x30);
                    self.at += 1;
                }
                _ => break,
            }
        }

        value
    }

    fn escape(&mut self, in_class: bool) -> Option<Atom> {
        let escaped = self.peek()?;
        self.at += 1;
        let simple = |value: u8| Some(Atom::Unit(unit(value)));
        match u8::try_from(escaped).unwrap_or(0) {
            b'd' => Some(Atom::Set(ClassItem::Digit(true))),
            b'D' => Some(Atom::Set(ClassItem::Digit(false))),
            b'w' => Some(Atom::Set(ClassItem::Word(true))),
            b'W' => Some(Atom::Set(ClassItem::Word(false))),
            b's' => Some(Atom::Set(ClassItem::Space(true))),
            b'S' => Some(Atom::Set(ClassItem::Space(false))),
            b'b' if in_class => simple(0x08),
            b'n' => simple(b'\n'),
            b'r' => simple(b'\r'),
            b't' => simple(b'\t'),
            b'v' => simple(0x0b),
            b'f' => simple(0x0c),
            b'0'..=b'7' => Some(Atom::Unit(self.octal(escaped))),
            b'x' => Some(Atom::Unit(self.hex(2).unwrap_or(escaped))),
            b'u' => Some(Atom::Unit(self.hex(4).unwrap_or(escaped))),
            b'c' => match self.peek() {
                Some(letter)
                    if u8::try_from(letter).is_ok_and(|byte| {
                        byte.is_ascii_alphabetic()
                            || in_class && (byte.is_ascii_digit() || byte == b'_')
                    }) =>
                {
                    self.at += 1;
                    Some(Atom::Unit(letter % 32))
                }
                _ => {
                    self.at -= 1;
                    simple(b'\\')
                }
            },
            _ => Some(Atom::Unit(escaped)),
        }
    }

    fn class(&mut self) -> Option<Class> {
        let negated = self.eat(b'^');
        let mut items = Vec::new();
        let item = |atom: Atom| match atom {
            Atom::Unit(found) => ClassItem::Range(found, found),
            Atom::Set(set) => set,
        };

        loop {
            let char = self.peek()?;
            if char == unit(b']') {
                self.at += 1;
                return Some(Class { negated, items });
            }

            let low = self.class_atom()?;
            let dash = self.peek() == Some(unit(b'-'))
                && self
                    .source
                    .get(self.at + 1)
                    .is_some_and(|&next| next != unit(b']'));

            if !dash {
                items.push(item(low));
                continue;
            }

            self.at += 1;
            let high = self.class_atom()?;
            match (low, high) {
                (Atom::Unit(low), Atom::Unit(high)) if low <= high => {
                    items.push(ClassItem::Range(low, high));
                }
                (Atom::Unit(_), Atom::Unit(_)) => return None,
                (low, high) => {
                    items.push(item(low));
                    items.push(ClassItem::Range(unit(b'-'), unit(b'-')));
                    items.push(item(high));
                }
            }
        }
    }

    fn class_atom(&mut self) -> Option<Atom> {
        let char = self.peek()?;
        self.at += 1;
        if char != unit(b'\\') {
            return Some(Atom::Unit(char));
        }

        match self.peek()? {
            digit @ (0x38 | 0x39) => {
                self.at += 1;
                Some(Atom::Unit(digit))
            }
            _ => self.escape(true),
        }
    }

    fn braced_quantifier(&self) -> Option<(usize, Option<usize>, usize)> {
        let rest = self.source.get(self.at..)?;
        if rest.first() != Some(&unit(b'{')) {
            return None;
        }

        let end = rest.iter().position(|&found| found == unit(b'}'))?;
        let inner = String::from_utf16(&rest[1..end]).ok()?;
        let number = |text: &str| {
            (!text.is_empty() && text.bytes().all(|byte| byte.is_ascii_digit()))
                .then(|| text.parse::<usize>().unwrap_or(usize::MAX))
        };

        let (min, max) = match inner.split_once(',') {
            None => {
                let count = number(&inner)?;
                (count, Some(count))
            }
            Some((min, "")) => (number(min)?, None),
            Some((min, max)) => (number(min)?, Some(number(max)?)),
        };

        Some((min, max, end + 1))
    }

    fn quantifier(&mut self) -> Option<Option<(usize, Option<usize>)>> {
        let bounds = match u8::try_from(self.peek().unwrap_or(0)).unwrap_or(0) {
            b'*' => (0, None),
            b'+' => (1, None),
            b'?' => (0, Some(1)),
            b'{' => match self.braced_quantifier() {
                Some((min, max, length)) => {
                    if max.is_some_and(|max| max < min) {
                        return None;
                    }

                    self.at += length - 1;
                    (min, max)
                }
                None => return Some(None),
            },
            _ => return Some(None),
        };

        self.at += 1;
        Some(Some(bounds))
    }

    fn term(&mut self) -> Option<Node> {
        let char = self.peek()?;
        if char == unit(b'{') && self.braced_quantifier().is_some() {
            return None;
        }

        self.at += 1;
        let atom = match u8::try_from(char).unwrap_or(0) {
            b'^' => return Some(Node::Start),
            b'$' => return Some(Node::End),
            b'*' | b'+' | b'?' => return None,
            b'.' => Node::Any,
            b'[' => Node::Class(self.class()?),
            b'(' => {
                let capture = if self.eat(b'?') {
                    if !self.eat(b':') {
                        return None;
                    }

                    None
                } else {
                    self.groups += 1;
                    Some(self.groups - 1)
                };

                let alternatives = self.disjunction()?;
                if !self.eat(b')') {
                    return None;
                }

                Node::Group {
                    capture,
                    alternatives,
                }
            }
            b')' => return None,
            b'\\' => match self.peek()? {
                0x62 => {
                    self.at += 1;
                    return Some(Node::Boundary(true));
                }
                0x42 => {
                    self.at += 1;
                    return Some(Node::Boundary(false));
                }
                digit @ 0x31..=0x39 => {
                    if usize::from(digit - 0x30) <= self.total {
                        return None;
                    }

                    if digit >= 0x38 {
                        self.at += 1;
                        Node::Unit(digit)
                    } else {
                        match self.escape(false)? {
                            Atom::Unit(found) => Node::Unit(found),
                            Atom::Set(set) => Node::Class(Class {
                                negated: false,
                                items: vec![set],
                            }),
                        }
                    }
                }
                _ => match self.escape(false)? {
                    Atom::Unit(found) => Node::Unit(found),
                    Atom::Set(set) => Node::Class(Class {
                        negated: false,
                        items: vec![set],
                    }),
                },
            },
            _ => Node::Unit(char),
        };

        let Some((min, max)) = self.quantifier()? else {
            return Some(atom);
        };

        let greedy = !self.eat(b'?');
        Some(Node::Repeat {
            node: Box::new(atom),
            min,
            max,
            greedy,
        })
    }

    fn disjunction(&mut self) -> Option<Vec<Vec<Node>>> {
        let mut alternatives = vec![Vec::new()];
        while let Some(char) = self.peek() {
            if char == unit(b')') {
                break;
            }

            if char == unit(b'|') {
                self.at += 1;
                alternatives.push(Vec::new());
                continue;
            }

            let node = self.term()?;
            alternatives
                .last_mut()
                .expect("one alternative exists")
                .push(node);
        }

        Some(alternatives)
    }
}

fn parse(source: &[u16]) -> Option<(Vec<Vec<Node>>, usize)> {
    let mut parser = Parser {
        source,
        at: 0,
        groups: 0,
        total: capturing_groups(source),
    };

    let alternatives = parser.disjunction()?;
    (parser.at == source.len()).then_some((alternatives, parser.groups))
}

impl Class {
    fn matches(&self, found: u16) -> bool {
        let hit = self.items.iter().any(|item| match *item {
            ClassItem::Range(low, high) => low <= found && found <= high,
            ClassItem::Digit(wanted) => is_digit(found) == wanted,
            ClassItem::Word(wanted) => is_word(found) == wanted,
            ClassItem::Space(wanted) => is_space(found) == wanted,
        });

        hit != self.negated
    }
}

struct Bounds<'n> {
    node: &'n Node,
    min: usize,
    max: Option<usize>,
    greedy: bool,
}

type Then<'k> = &'k mut dyn FnMut(&mut Matcher, usize) -> bool;

struct Matcher<'t> {
    text: &'t [u16],
    captures: Vec<Option<(usize, usize)>>,
}

impl Matcher<'_> {
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

impl EditorGlob {
    pub fn new(pattern: &str) -> Self {
        let source = pattern.strip_prefix('/').unwrap_or(pattern);
        let units: Vec<u16> = source.encode_utf16().collect();
        let mut ranges = Vec::new();
        let mut regex = vec![unit(b'^')];
        regex.extend(compile(&units, &mut ranges));
        regex.push(unit(b'$'));
        let parsed = parse(&regex);
        Self {
            groups: parsed.as_ref().map_or(0, |(_, groups)| *groups),
            nodes: parsed.map(|(nodes, _)| nodes),
            ranges,
        }
    }

    pub fn matches(&self, target: &str) -> bool {
        let Some(alternatives) = &self.nodes else {
            return false;
        };

        let text: Vec<u16> = target.encode_utf16().collect();
        let mut matcher = Matcher {
            text: &text,
            captures: vec![None; self.groups],
        };

        let mut accept = |_: &mut Matcher, _: usize| true;
        let matched = (0..=text.len()).any(|at| {
            matcher.captures.fill(None);
            alternatives
                .iter()
                .any(|alternative| matcher.sequence(alternative, at, &mut accept))
        });

        if !matched {
            return false;
        }

        self.ranges
            .iter()
            .enumerate()
            .all(|(index, (first, last))| {
                let Some(Some((start, end))) = matcher.captures.get(index).copied() else {
                    return true;
                };

                let Some(value) = Integer::parse(&String::from_utf16_lossy(&text[start..end]))
                else {
                    return false;
                };

                let (low, high) = if first < last {
                    (first, last)
                } else {
                    (last, first)
                };
                *low <= value && value <= *high
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
