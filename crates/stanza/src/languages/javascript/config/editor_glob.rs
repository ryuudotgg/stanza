use std::cmp::Ordering;

#[derive(Clone, Debug)]
enum ClassItem {
    Range(u16, u16),
    Digit(bool),
    Word(bool),
    Space(bool),
}

#[derive(Clone, Debug)]
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
    program: Option<Program>,
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

    fn quantifier(&mut self) -> Option<(usize, Option<usize>)> {
        let bounds = match u8::try_from(self.peek()?).unwrap_or(0) {
            b'*' => (0, None),
            b'+' => (1, None),
            b'?' => (0, Some(1)),
            _ => return None,
        };

        self.at += 1;
        Some(bounds)
    }

    fn term(&mut self) -> Option<Node> {
        let char = self.peek()?;
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

        let Some((min, max)) = self.quantifier() else {
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

fn nullable(node: &Node) -> bool {
    match node {
        Node::Unit(_) | Node::Any | Node::Class(_) => false,
        Node::Start | Node::End | Node::Boundary(_) => true,
        Node::Group { alternatives, .. } => alternatives
            .iter()
            .any(|alternative| alternative.iter().all(nullable)),
        Node::Repeat { node, min, .. } => *min == 0 || nullable(node),
    }
}

fn word_at(text: &[u16], at: usize) -> bool {
    text.get(at).copied().is_some_and(is_word)
}

#[derive(Debug)]
enum Step {
    Unit(u16),
    Any,
    Class(Class),
    Start,
    End,
    Boundary(bool),
    Split(usize, usize),
    Jump(usize),
    Save(usize),
    Mark(usize),
    Progress(usize),
    Match,
}

#[derive(Default)]
struct Lowering {
    steps: Vec<Step>,
    registers: usize,
}

impl Lowering {
    fn push(&mut self, step: Step) -> usize {
        self.steps.push(step);
        self.steps.len() - 1
    }

    fn alternatives(&mut self, alternatives: &[Vec<Node>]) {
        let mut exits = Vec::new();
        for (index, alternative) in alternatives.iter().enumerate() {
            let split = (index + 1 < alternatives.len()).then(|| self.push(Step::Split(0, 0)));
            for node in alternative {
                self.node(node);
            }

            if let Some(split) = split {
                exits.push(self.push(Step::Jump(0)));
                self.steps[split] = Step::Split(split + 1, self.steps.len());
            }
        }

        let end = self.steps.len();
        for exit in exits {
            self.steps[exit] = Step::Jump(end);
        }
    }

    fn node(&mut self, node: &Node) {
        let step = match node {
            Node::Unit(expected) => Step::Unit(*expected),
            Node::Any => Step::Any,
            Node::Class(class) => Step::Class(class.clone()),
            Node::Start => Step::Start,
            Node::End => Step::End,
            Node::Boundary(wanted) => Step::Boundary(*wanted),
            Node::Group {
                capture,
                alternatives,
            } => {
                if let Some(index) = capture {
                    self.push(Step::Save(index * 2));
                }

                self.alternatives(alternatives);
                if let Some(index) = capture {
                    self.push(Step::Save(index * 2 + 1));
                }

                return;
            }
            Node::Repeat {
                node,
                min,
                max,
                greedy,
            } => return self.repeat(node, *min, *max, *greedy),
        };

        self.push(step);
    }

    fn repeat(&mut self, node: &Node, min: usize, max: Option<usize>, greedy: bool) {
        for _ in 0..min {
            self.node(node);
        }

        let register = nullable(node).then(|| {
            self.registers += 1;
            self.registers - 1
        });

        let mut heads = Vec::new();
        for _ in 0..max.map_or(1, |max| max - min) {
            heads.push(self.push(Step::Split(0, 0)));
            if let Some(register) = register {
                self.push(Step::Mark(register));
            }

            self.node(node);
            if let Some(register) = register {
                self.push(Step::Progress(register));
            }

            if max.is_none() {
                self.push(Step::Jump(heads[0]));
            }
        }

        let exit = self.steps.len();
        for head in heads {
            self.steps[head] = if greedy {
                Step::Split(head + 1, exit)
            } else {
                Step::Split(exit, head + 1)
            };
        }
    }
}

enum Undo {
    Branch(usize, usize),
    Slot(usize, Option<usize>),
    Register(usize, usize),
}

#[derive(Debug)]
struct Program {
    steps: Vec<Step>,
    groups: usize,
    registers: usize,
}

impl Program {
    fn lower(alternatives: &[Vec<Node>], groups: usize) -> Self {
        let mut lowering = Lowering::default();
        lowering.alternatives(alternatives);
        lowering.push(Step::Match);
        Self {
            steps: lowering.steps,
            groups,
            registers: lowering.registers,
        }
    }

    fn captures(&self, text: &[u16]) -> Option<Vec<Option<(usize, usize)>>> {
        let width = text.len() + 1;
        // Progress reads where its loop iteration began, which a (step, position) key misses.
        let memoize = self.registers == 0;
        let mut seen = vec![
            0_u64;
            if memoize {
                (self.steps.len() * width).div_ceil(64)
            } else {
                0
            }
        ];
        let mut slots = vec![None; self.groups * 2];
        let mut registers = vec![0; self.registers];
        let mut stack = Vec::new();
        let last_start = if matches!(self.steps.first(), Some(Step::Start)) {
            0
        } else {
            text.len()
        };

        for start in 0..=last_start {
            slots.fill(None);
            stack.push(Undo::Branch(0, start));
            while let Some(undo) = stack.pop() {
                let (mut pc, mut at) = match undo {
                    Undo::Branch(pc, at) => (pc, at),
                    Undo::Slot(slot, value) => {
                        slots[slot] = value;
                        continue;
                    }
                    Undo::Register(register, value) => {
                        registers[register] = value;
                        continue;
                    }
                };

                loop {
                    if memoize {
                        let state = pc * width + at;
                        let bit = 1 << (state % 64);
                        if seen[state / 64] & bit != 0 {
                            break;
                        }

                        seen[state / 64] |= bit;
                    }

                    let found = text.get(at).copied();
                    match &self.steps[pc] {
                        Step::Unit(expected) if found == Some(*expected) => {
                            pc += 1;
                            at += 1;
                        }
                        Step::Any if found.is_some_and(|found| !is_line_terminator(found)) => {
                            pc += 1;
                            at += 1;
                        }
                        Step::Class(class) if found.is_some_and(|found| class.matches(found)) => {
                            pc += 1;
                            at += 1;
                        }
                        Step::Start if at == 0 => pc += 1,
                        Step::End if at == text.len() => pc += 1,
                        Step::Boundary(wanted)
                            if ((at > 0 && word_at(text, at - 1)) != word_at(text, at))
                                == *wanted =>
                        {
                            pc += 1;
                        }
                        Step::Split(first, second) => {
                            stack.push(Undo::Branch(*second, at));
                            pc = *first;
                        }
                        Step::Jump(to) => pc = *to,
                        Step::Save(slot) => {
                            stack.push(Undo::Slot(*slot, slots[*slot]));
                            slots[*slot] = Some(at);
                            pc += 1;
                        }
                        Step::Mark(register) => {
                            stack.push(Undo::Register(*register, registers[*register]));
                            registers[*register] = at;
                            pc += 1;
                        }
                        Step::Progress(register) if at != registers[*register] => pc += 1,
                        Step::Match => {
                            return Some(
                                slots.chunks(2).map(|pair| pair[0].zip(pair[1])).collect(),
                            );
                        }
                        _ => break,
                    }
                }
            }
        }

        None
    }
}

fn regex(pattern: &str) -> (Vec<u16>, Vec<(Integer, Integer)>) {
    let source = pattern.strip_prefix('/').unwrap_or(pattern);
    let units: Vec<u16> = source.encode_utf16().collect();
    let mut ranges = Vec::new();
    let mut regex = vec![unit(b'^')];
    regex.extend(compile(&units, &mut ranges));
    regex.push(unit(b'$'));
    (regex, ranges)
}

fn within_ranges(
    ranges: &[(Integer, Integer)],
    captures: &[Option<(usize, usize)>],
    text: &[u16],
) -> bool {
    ranges.iter().enumerate().all(|(index, (first, last))| {
        let Some(Some((start, end))) = captures.get(index).copied() else {
            return true;
        };

        let Some(value) = Integer::parse(&String::from_utf16_lossy(&text[start..end])) else {
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

impl EditorGlob {
    pub fn new(pattern: &str) -> Self {
        let (regex, ranges) = regex(pattern);
        Self {
            program: parse(&regex).map(|(nodes, groups)| Program::lower(&nodes, groups)),
            ranges,
        }
    }

    pub fn matches(&self, target: &str) -> bool {
        let Some(program) = &self.program else {
            return false;
        };

        let text: Vec<u16> = target.encode_utf16().collect();
        program
            .captures(&text)
            .is_some_and(|captures| within_ranges(&self.ranges, &captures, &text))
    }
}

#[cfg(test)]
#[path = "editor_glob_tests.rs"]
mod tests;
