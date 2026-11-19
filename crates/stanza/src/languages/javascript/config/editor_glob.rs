use super::token_glob::{
    ALTERNATIVE_LIMIT, Atom, EXPANDED_LIMIT, STATE_LIMIT, Segment, Units, encode, reached, wildcard,
};

const MAX_SAFE_INTEGER: i64 = (1 << 53) - 1;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Rules {
    EditorConfig,
    Oxfmt,
}

pub struct SectionPath {
    segments: Vec<Vec<u16>>,
    length: usize,
    bytes: Vec<u16>,
}

impl SectionPath {
    pub fn new(relative: &str) -> Self {
        let segments: Vec<Vec<u16>> = relative
            .split('/')
            .map(|segment| encode(segment, Units::Utf16))
            .collect();

        let length = segments.iter().map(Vec::len).sum::<usize>() + segments.len() - 1;
        Self {
            segments,
            length,
            bytes: encode(relative, Units::Bytes),
        }
    }
}

pub struct SectionGlob {
    editorconfig: Vec<Vec<Segment>>,
    oxfmt: Option<Vec<Vec<Atom>>>,
    segment_cost: usize,
    byte_cost: usize,
}

impl SectionGlob {
    pub fn new(name: &str, rules: Rules) -> Option<Self> {
        if name.is_empty() || name.len() > EXPANDED_LIMIT {
            return None;
        }

        let nodes = parsed(name)?;
        let mut prefixed = Vec::new();
        if !name.contains('/') {
            prefixed.extend([Node::Token(Token::Globstar), Node::Token(Token::Slash)]);
        }

        prefixed.extend(nodes.iter().cloned());
        let editorconfig: Vec<Vec<Segment>> = expanded(&prefixed, Rules::EditorConfig, false)?
            .iter()
            .map(|alternative| segments(alternative, name.starts_with('/')))
            .collect::<Option<_>>()?;

        let oxfmt = match rules {
            Rules::EditorConfig => None,
            Rules::Oxfmt => Some(flat(&expanded(&nodes, Rules::Oxfmt, false)?)?),
        };

        let segment_cost = editorconfig
            .iter()
            .flatten()
            .map(|segment| match segment {
                Segment::Any => 2,
                Segment::Atoms(atoms) => atoms.iter().map(Atom::cost).sum::<usize>() + 1,
            })
            .sum();

        let byte_cost = oxfmt.iter().flatten().flatten().map(Atom::cost).sum();
        Some(Self {
            editorconfig,
            oxfmt,
            segment_cost,
            byte_cost,
        })
    }

    pub fn matches(&self, path: &SectionPath) -> Option<bool> {
        let states = self
            .segment_cost
            .saturating_mul(path.length.max(1))
            .saturating_add(self.byte_cost.saturating_mul(path.bytes.len().max(1)));

        if states > STATE_LIMIT {
            return None;
        }

        let standard = self
            .editorconfig
            .iter()
            .any(|alternative| reached(alternative, &path.segments)[path.segments.len()]);

        match &self.oxfmt {
            None => Some(standard),
            Some(alternatives) => {
                let oxfmt = alternatives
                    .iter()
                    .any(|atoms| wildcard(atoms, &path.bytes));

                (oxfmt == standard).then_some(standard)
            }
        }
    }
}

#[derive(Clone)]
struct Class {
    negated: bool,
    members: Vec<(char, bool)>,
}

#[derive(Clone)]
enum Token {
    Char(char),
    Escaped(char),
    Star,
    Globstar,
    One,
    Class(Class),
    Slash,
}

impl Token {
    fn weight(&self) -> usize {
        match self {
            Self::Class(class) => class.members.len(),
            _ => 1,
        }
    }
}

#[derive(Clone)]
enum Node {
    Token(Token),
    Group(Vec<Vec<Self>>),
    Braced(Vec<Self>),
    Sequence { first: i64, last: i64, raw: String },
}

struct Frame {
    arms: Vec<Vec<Node>>,
    open: usize,
}

impl Frame {
    fn new(open: usize) -> Self {
        Self {
            arms: vec![Vec::new()],
            open,
        }
    }

    fn push(&mut self, node: Node) {
        self.arms.last_mut().expect("a frame has an arm").push(node);
    }
}

fn parsed(name: &str) -> Option<Vec<Node>> {
    if name.contains('{') && name.contains(['\u{2028}', '\u{2029}']) {
        return None;
    }

    let expands = !name.contains('/')
        || name.contains("**")
        || name
            .find('{')
            .is_some_and(|open| name[open..].contains('}'));

    let chars: Vec<char> = name.chars().collect();
    let mut stack = vec![Frame::new(0)];
    let mut previous = None;
    let mut at = 0;
    while let Some(&char) = chars.get(at) {
        let next = chars.get(at + 1).copied();
        let mut width = 1;
        let node = match char {
            '\\' => {
                let escaped = next.filter(|&escaped| escaped != '/')?;
                if escaped == '*' && chars.get(at + 2) == Some(&'*') || escaped == '\\' && !expands
                {
                    return None;
                }

                width = 2;
                Some(Token::Escaped(escaped))
            }
            '*' if next == Some('*') => {
                if chars.get(at + 2) == Some(&'*') || previous == Some('$') {
                    return None;
                }

                width = 2;
                Some(Token::Globstar)
            }
            '*' => Some(Token::Star),
            '?' => Some(Token::One),
            '(' | ']' => return None,
            '[' => {
                let (class, used) = class(&chars[at + 1..], expands)?;
                width = used + 1;
                Some(Token::Class(class))
            }
            '{' if previous == Some('$') => return None,
            '{' if stack.len() > ALTERNATIVE_LIMIT => return None,
            '{' => {
                stack.push(Frame::new(at + 1));
                None
            }
            ',' if stack.len() > 1 => {
                stack
                    .last_mut()
                    .expect("an open group")
                    .arms
                    .push(Vec::new());
                None
            }
            '}' if stack.len() == 1 => return None,
            '}' => {
                let closed = stack.pop().expect("an open group");
                let raw: String = chars[closed.open..at].iter().collect();
                let node = group(closed.arms, raw)?;
                stack.last_mut().expect("the root frame stays").push(node);
                None
            }
            '/' => Some(Token::Slash),
            _ => Some(Token::Char(char)),
        };

        if let Some(token) = node {
            stack
                .last_mut()
                .expect("the root frame stays")
                .push(Node::Token(token));
        }

        previous = Some(chars[at + width - 1]);
        at += width;
    }

    let root = stack.pop().filter(|_| stack.is_empty())?;
    root.arms
        .into_iter()
        .next()
        .filter(|nodes| expands_after_braces(nodes))
}

// brace-expansion leaves the rest of a scope unexpanded after a one element group unless a later `,...}` follows it.
fn expands_after_braces(nodes: &[Node]) -> bool {
    let braced = nodes
        .iter()
        .position(|node| matches!(node, Node::Braced(_)));
    braced.is_none_or(|at| {
        !nodes[at..]
            .iter()
            .any(|node| matches!(node, Node::Sequence { .. }))
    })
}

fn class(chars: &[char], expands: bool) -> Option<(Class, usize)> {
    let negated = matches!(chars.first(), Some('!' | '^'));
    let mut at = usize::from(negated);
    let mut members = Vec::new();
    loop {
        match *chars.get(at)? {
            ']' if !members.is_empty() => return Some((Class { negated, members }, at + 1)),
            '\\' => {
                let escaped = chars
                    .get(at + 1)
                    .copied()
                    .filter(|&escaped| escaped != '/' && (expands || escaped != '\\'))?;
                members.push((escaped, true));
                at += 2;
            }
            '[' | '/' | '{' | '}' | ',' | '*' => return None,
            member => {
                members.push((member, false));
                at += 1;
            }
        }
    }
}

fn group(mut arms: Vec<Vec<Node>>, raw: String) -> Option<Node> {
    if arms.len() > 1 {
        return arms
            .iter()
            .all(|arm| expands_after_braces(arm))
            .then_some(Node::Group(arms));
    }

    let parts: Vec<&str> = raw.split("..").collect();
    let numeric = |part: &&str| {
        let digits = part.strip_prefix('-').unwrap_or(part);
        !digits.is_empty() && digits.bytes().all(|byte| byte.is_ascii_digit())
    };

    let letter =
        |part: &&str| part.len() == 1 && part.bytes().all(|byte| byte.is_ascii_alphabetic());
    if let [first, last] = parts[..]
        && numeric(&first)
        && numeric(&last)
    {
        let padded = |part: &str| {
            part.strip_prefix('-').unwrap_or(part).len() > 1
                && part.trim_start_matches('-').starts_with('0')
        };
        let (first, last) = (bound(first)?, bound(last)?);
        let padded = padded(parts[0]) || padded(parts[1]);
        let count = first.abs_diff(last).saturating_add(1);
        if padded || count > ALTERNATIVE_LIMIT as u64 {
            return None;
        }

        return Some(Node::Sequence { first, last, raw });
    }

    let stepped = parts.len() == 3
        && numeric(&parts[2])
        && (parts[..2].iter().all(numeric) || parts[..2].iter().all(letter));
    if stepped || parts.len() == 2 && parts.iter().all(letter) {
        return None;
    }

    let arm = arms.pop().expect("a frame has an arm");
    let simple = arm
        .iter()
        .all(|node| matches!(node, Node::Token(token) if !matches!(token, Token::Globstar)));

    simple.then_some(Node::Braced(arm))
}

fn bound(text: &str) -> Option<i64> {
    text.parse::<i64>()
        .ok()
        .filter(|value| (-MAX_SAFE_INTEGER..=MAX_SAFE_INTEGER).contains(value))
}

fn expanded(nodes: &[Node], rules: Rules, nested: bool) -> Option<Vec<Vec<Token>>> {
    let mut alternatives = vec![Vec::new()];
    let mut weight: usize = 0;
    for node in nodes {
        let options: Vec<Vec<Token>> = match (node, rules) {
            (Node::Token(Token::Globstar), Rules::EditorConfig) => vec![
                vec![Token::Star],
                vec![
                    Token::Globstar,
                    Token::Slash,
                    Token::Globstar,
                    Token::Slash,
                    Token::Globstar,
                ],
            ],
            (Node::Token(Token::Globstar), Rules::Oxfmt) if nested => return None,
            (Node::Token(token), _) => vec![vec![token.clone()]],
            (Node::Group(arms), _) => {
                let mut options = Vec::new();
                for arm in arms {
                    if rules == Rules::Oxfmt && arm.is_empty() {
                        continue;
                    }

                    options.extend(expanded(arm, rules, true)?);
                }

                if options.is_empty() {
                    options.push(Vec::new());
                }

                options
            }
            (Node::Braced(arm), Rules::EditorConfig) => expanded(arm, rules, true)?
                .into_iter()
                .map(|inner| {
                    let mut spelled = vec![Token::Char('{')];
                    spelled.extend(inner);
                    spelled.push(Token::Char('}'));
                    spelled
                })
                .collect(),
            (Node::Braced(arm), Rules::Oxfmt) => expanded(arm, rules, true)?,
            (Node::Sequence { first, last, .. }, Rules::EditorConfig) => {
                let step = if first <= last { 1 } else { -1 };
                let mut value = *first;
                let mut numbers = Vec::new();
                loop {
                    numbers.push(value.to_string().chars().map(Token::Char).collect());
                    if value == *last {
                        break numbers;
                    }

                    value += step;
                }
            }
            (Node::Sequence { raw, .. }, Rules::Oxfmt) => {
                vec![raw.chars().map(Token::Char).collect()]
            }
        };

        let size = alternatives.len().saturating_mul(options.len());
        let added: usize = options.iter().flatten().map(Token::weight).sum();
        weight = weight
            .saturating_mul(options.len())
            .saturating_add(added.saturating_mul(alternatives.len()));

        if size > ALTERNATIVE_LIMIT || weight > EXPANDED_LIMIT {
            return None;
        }

        if let [option] = &options[..] {
            for alternative in &mut alternatives {
                alternative.extend(option.iter().cloned());
            }

            continue;
        }

        alternatives = alternatives
            .iter()
            .flat_map(|prefix| {
                options.iter().map(move |option| {
                    let mut joined = prefix.clone();
                    joined.extend(option.iter().cloned());
                    joined
                })
            })
            .collect();
    }

    Some(alternatives)
}

fn segments(tokens: &[Token], rooted: bool) -> Option<Vec<Segment>> {
    let mut pieces: Vec<&[Token]> = tokens
        .split(|token| matches!(token, Token::Slash))
        .collect();

    if rooted {
        pieces.remove(0);
    }

    let last = pieces.len() - 1;
    let mut lowered = Vec::new();
    for (index, piece) in pieces.into_iter().enumerate() {
        let stars = piece.iter().try_fold(0, |stars, token| match token {
            Token::Star => Some(stars + 1),
            Token::Globstar => Some(stars + 2),
            _ => None,
        });

        if stars == Some(2) {
            lowered.push(Segment::Any);
            if index == last {
                lowered.push(Segment::Atoms(vec![Atom::Star]));
            }

            continue;
        }

        if fast_tested(piece) {
            return None;
        }

        let dots = piece.iter().all(|token| matches!(token, Token::Char('.')));
        if piece.is_empty() || dots && piece.len() <= 2 {
            return None;
        }

        lowered.push(Segment::Atoms(atoms(piece, Units::Utf16)?));
    }

    Some(lowered)
}

// Minimatch tests a segment of leading stars or question marks then plain text with `endsWith` on the raw text, backslashes included.
fn fast_tested(piece: &[Token]) -> bool {
    let lead = piece
        .iter()
        .take_while(|token| matches!(token, Token::Star | Token::Globstar))
        .count();
    let lead = if lead > 0 {
        lead
    } else {
        piece
            .iter()
            .take_while(|token| matches!(token, Token::One))
            .count()
    };

    let plain = |char: &char| !"+@!?*[(".contains(*char);
    let tail = &piece[lead..];
    lead > 0
        && tail.iter().any(|token| matches!(token, Token::Escaped(_)))
        && tail.iter().all(|token| match token {
            Token::Char(char) | Token::Escaped(char) => plain(char),
            _ => false,
        })
}

fn flat(alternatives: &[Vec<Token>]) -> Option<Vec<Vec<Atom>>> {
    let mut flattened = Vec::new();
    for tokens in alternatives {
        let starred = |token: &Token| matches!(token, Token::Star | Token::Globstar);
        if tokens
            .windows(2)
            .any(|pair| starred(&pair[0]) && starred(&pair[1]))
        {
            return None;
        }

        let mut forks: Vec<Vec<Token>> = vec![Vec::new()];
        let mut at = 0;
        while let Some(token) = tokens.get(at) {
            let starts_component = at == 0 || matches!(tokens[at - 1], Token::Slash);
            let ends_component = matches!(tokens.get(at + 1), Some(Token::Slash));
            if matches!(token, Token::Globstar) && starts_component && ends_component {
                forks = forks
                    .into_iter()
                    .flat_map(|fork| {
                        let mut crossed = fork.clone();
                        crossed.extend([Token::Star, Token::Slash]);
                        [fork, crossed]
                    })
                    .collect();

                at += 2;
                continue;
            }

            for fork in &mut forks {
                fork.push(token.clone());
            }

            at += 1;
        }

        flattened.extend(forks);
        if flattened.len() > ALTERNATIVE_LIMIT {
            return None;
        }
    }

    flattened
        .iter()
        .map(|tokens| atoms(tokens, Units::Bytes))
        .collect()
}

fn atoms(tokens: &[Token], units: Units) -> Option<Vec<Atom>> {
    let mut atoms = Vec::new();
    for token in tokens {
        match token {
            Token::Char(char) | Token::Escaped(char) => atoms.extend(
                encode(char.encode_utf8(&mut [0; 4]), units)
                    .into_iter()
                    .map(Atom::Unit),
            ),
            Token::Slash => atoms.push(Atom::Unit(u16::from(b'/'))),
            Token::Star | Token::Globstar => atoms.push(Atom::Star),
            Token::One => atoms.push(Atom::One),
            Token::Class(class) => atoms.push(Atom::Class(ranges(class, units)?)),
        }
    }

    Some(atoms)
}

fn ranges(class: &Class, units: Units) -> Option<Vec<(u16, u16)>> {
    let mut members = Vec::new();
    for &(char, escaped) in &class.members {
        match units {
            Units::Utf16 => {
                members.extend(
                    char.encode_utf16(&mut [0; 2])
                        .iter()
                        .map(|&unit| (unit, escaped)),
                );
            }
            Units::Bytes if char.is_ascii() => members.push((u16::from(char as u8), escaped)),
            Units::Bytes => return None,
        }
    }

    if units == Units::Bytes && members.iter().any(|&(_, escaped)| escaped) {
        return None;
    }

    let dash = |at: usize| members.get(at) == Some(&(u16::from(b'-'), false));
    let mut ranges = Vec::new();
    let mut at = 0;
    while let Some(&(unit, _)) = members.get(at) {
        if dash(at + 1) && at + 2 == members.len() {
            ranges.extend([(unit, unit), (u16::from(b'-'), u16::from(b'-'))]);
            at += 2;
        } else if dash(at + 1) {
            let last = members[at + 2].0;
            if unit <= last {
                ranges.push((unit, last));
            }

            if units == Units::Bytes && dash(at + 3) {
                return None;
            }

            at += 3;
        } else {
            ranges.push((unit, unit));
            at += 1;
        }
    }

    ranges.sort_unstable();
    if !class.negated || ranges.is_empty() {
        return Some(ranges);
    }

    let top = match units {
        Units::Utf16 => u16::MAX,
        Units::Bytes => 0xff,
    };

    let mut complement = Vec::new();
    let mut from = 0u32;
    for (first, last) in ranges {
        if u32::from(first) > from {
            complement.push((from as u16, first - 1));
        }

        from = from.max(u32::from(last) + 1);
    }

    if from <= u32::from(top) {
        complement.push((from as u16, top));
    }

    Some(complement)
}

#[cfg(test)]
#[path = "editor_glob_tests.rs"]
mod tests;
