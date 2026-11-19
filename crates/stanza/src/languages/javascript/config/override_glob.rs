use super::token_glob::{
    ALTERNATIVE_LIMIT, Atom, EXPANDED_LIMIT, STATE_LIMIT, Segment, Units, encode, reached,
};
use super::width::Formatter;

#[derive(Clone, Copy)]
struct Policy {
    units: Units,
    classes: bool,
    picomatch: bool,
    folder_suffix: bool,
}

const fn policy(formatter: Formatter) -> Policy {
    match formatter {
        Formatter::Prettier => Policy {
            units: Units::Utf16,
            classes: true,
            picomatch: true,
            folder_suffix: false,
        },
        Formatter::Oxfmt => Policy {
            units: Units::Bytes,
            classes: true,
            picomatch: false,
            folder_suffix: false,
        },
        Formatter::Biome => Policy {
            units: Units::Bytes,
            classes: false,
            picomatch: false,
            folder_suffix: true,
        },
    }
}

#[derive(Clone, Copy)]
pub enum Anchor {
    Path,
    NameIfSlashless,
    Name,
}

pub struct Target {
    relative: String,
    segments: Vec<Vec<u16>>,
    length: usize,
}

impl Target {
    pub fn new(relative: &str, formatter: Formatter) -> Self {
        let units = policy(formatter).units;
        let segments: Vec<Vec<u16>> = relative
            .split('/')
            .map(|segment| encode(segment, units))
            .collect();

        let length = segments.iter().map(Vec::len).sum::<usize>() + segments.len() - 1;
        Self {
            relative: relative.to_owned(),
            segments,
            length,
        }
    }
}

enum Piece {
    Text(String),
    Group(Vec<String>),
}

pub struct OverrideGlob {
    raw: String,
    slashless: bool,
    picomatch: bool,
    alternatives: Vec<Vec<Segment>>,
}

impl OverrideGlob {
    pub fn new(pattern: &str, formatter: Formatter, target: &Target) -> Option<Self> {
        let policy = policy(formatter);
        let slashless = !pattern.contains('/');
        let body = pattern.strip_prefix("./").unwrap_or(pattern);
        let folder = policy.folder_suffix && body.len() > 1 && body.ends_with('/');
        let body = if folder {
            &body[..body.len() - 1]
        } else {
            body
        };
        if body.is_empty()
            || body.len() > 2 * EXPANDED_LIMIT + 2
            || body.starts_with('!')
            || body.contains(['\\', '(', ')', '|', '"'])
            || body.contains("]+")
            || body.contains("}+")
        {
            return None;
        }

        let mut pieces = pieces(body)?;
        if policy.picomatch {
            pieces = literal_spellings(pieces)?;
        }

        let mut alternatives: Vec<Vec<Segment>> = expanded(&pieces)?
            .iter()
            .map(|alternative| {
                alternative
                    .split('/')
                    .map(|segment| compiled_segment(segment, policy))
                    .collect()
            })
            .collect::<Option<_>>()?;

        let units: usize = alternatives
            .iter()
            .flatten()
            .map(|segment| match segment {
                Segment::Any => 2,
                Segment::Atoms(atoms) => atoms.iter().map(Atom::cost).sum::<usize>() + 1,
            })
            .sum();

        // Biome 2.6.3 reads a trailing slash as folders only, and every path matched here is a file.
        if folder {
            alternatives.clear();
        }

        (units.saturating_mul(target.length) <= STATE_LIMIT).then_some(Self {
            raw: pattern.to_owned(),
            slashless,
            picomatch: policy.picomatch,
            alternatives,
        })
    }

    pub fn applies(&self, target: &Target, excludes: &[Self]) -> bool {
        let exclude_anchor = match (self.picomatch, self.slashless) {
            (true, true) => Anchor::Name,
            (true, false) => Anchor::Path,
            (false, _) => Anchor::NameIfSlashless,
        };

        self.matches(target, Anchor::NameIfSlashless)
            && !excludes
                .iter()
                .any(|exclude| exclude.matches(target, exclude_anchor))
    }

    pub fn matches(&self, target: &Target, anchor: Anchor) -> bool {
        if self.picomatch && self.raw == target.relative {
            return true;
        }

        let name = match anchor {
            Anchor::Path => false,
            Anchor::NameIfSlashless => self.slashless,
            Anchor::Name => true,
        };

        let path = if name {
            &target.segments[target.segments.len() - 1..]
        } else {
            &target.segments[..]
        };

        self.alternatives
            .iter()
            .any(|alternative| reached(alternative, path)[path.len()])
    }

    pub fn covers(&self, target: &Target) -> bool {
        self.alternatives
            .iter()
            .any(|alternative| reached(alternative, &target.segments)[1..].contains(&true))
    }
}

fn pieces(body: &str) -> Option<Vec<Piece>> {
    let mut pieces = Vec::new();
    let mut text = String::new();
    let mut group: Option<Vec<String>> = None;
    let mut rest = body;
    while let Some(char) = rest.chars().next() {
        rest = &rest[char.len_utf8()..];
        let mut taken = String::from(char);
        match (char, group.as_mut()) {
            ('{', None) => {
                pieces.push(Piece::Text(std::mem::take(&mut text)));
                group = Some(vec![String::new()]);
                continue;
            }
            ('}', Some(arms)) => {
                if arms.len() < 2
                    || arms
                        .iter()
                        .any(|arm| arm.is_empty() || arm.contains("**") || arm.contains(".."))
                {
                    return None;
                }

                pieces.push(Piece::Group(std::mem::take(arms)));
                group = None;
                continue;
            }
            (',', Some(arms)) => {
                arms.push(String::new());
                continue;
            }
            ('[', _) => {
                let close = rest.find(']')?;
                if rest[..close].contains(['[', '/']) {
                    return None;
                }

                taken.push_str(&rest[..=close]);
                rest = &rest[close + 1..];
            }
            ('{' | '}' | ']', _) | ('/', Some(_)) => return None,
            _ => {}
        }

        match group.as_mut() {
            Some(arms) => arms
                .last_mut()
                .expect("a group has an arm")
                .push_str(&taken),
            None => text.push_str(&taken),
        }
    }

    if group.is_some() {
        return None;
    }

    pieces.push(Piece::Text(text));
    (!stars_meet(&pieces)).then_some(pieces)
}

fn stars_meet(pieces: &[Piece]) -> bool {
    let mut ends_with_star = false;
    for piece in pieces {
        let (starts, ends) = match piece {
            Piece::Text(text) if text.is_empty() => continue,
            Piece::Text(text) => (text.starts_with('*'), text.ends_with('*')),
            Piece::Group(arms) => (
                arms.iter().any(|arm| arm.starts_with('*')),
                arms.iter().any(|arm| arm.ends_with('*')),
            ),
        };

        if ends_with_star && starts {
            return true;
        }

        ends_with_star = ends;
    }

    false
}

const REGEX_CHARS: [char; 15] = [
    '-', '*', '+', '?', '.', '^', '$', '{', '}', '(', '|', ')', '[', ']', '\\',
];

fn literal_spellings(pieces: Vec<Piece>) -> Option<Vec<Piece>> {
    pieces
        .into_iter()
        .map(|piece| {
            let arms = match piece {
                Piece::Text(text) if spelled_classes(&text) == 0 => return Some(Piece::Text(text)),
                Piece::Text(text) => vec![text],
                Piece::Group(arms) => arms,
            };

            let mut spelled = Vec::new();
            for arm in &arms {
                let classes = spelled_classes(arm);
                if classes > ALTERNATIVE_LIMIT.ilog2() as usize
                    || arm.len() << classes > EXPANDED_LIMIT
                {
                    return None;
                }

                spelled.extend(spellings(arm));
                if spelled.len() > ALTERNATIVE_LIMIT {
                    return None;
                }
            }

            Some(Piece::Group(spelled))
        })
        .collect()
}

fn classes(text: &str) -> impl Iterator<Item = (usize, usize)> + '_ {
    text.match_indices('[').map(move |(open, _)| {
        (
            open,
            open + text[open..].find(']').expect("pieces closes every class"),
        )
    })
}

fn spelled_classes(text: &str) -> usize {
    classes(text)
        .filter(|&(open, close)| !text[open + 1..close].contains(REGEX_CHARS))
        .count()
}

fn spellings(text: &str) -> Vec<String> {
    let Some((open, close)) =
        classes(text).find(|&(open, close)| !text[open + 1..close].contains(REGEX_CHARS))
    else {
        return vec![text.to_owned()];
    };

    let (head, class, tail) = (&text[..open], &text[open..=close], &text[close + 1..]);
    let literal: String = class.chars().flat_map(|char| ['\\', char]).collect();
    spellings(tail)
        .iter()
        .flat_map(|rest| {
            [
                format!("{head}{class}{rest}"),
                format!("{head}{literal}{rest}"),
            ]
        })
        .collect()
}

fn expanded(pieces: &[Piece]) -> Option<Vec<String>> {
    let mut count: usize = 1;
    for piece in pieces {
        if let Piece::Group(arms) = piece {
            count = count.saturating_mul(arms.len());
            if count > ALTERNATIVE_LIMIT {
                return None;
            }
        }
    }

    let bytes = pieces.iter().fold(0usize, |bytes, piece| {
        let share = match piece {
            Piece::Text(text) => text.len().saturating_mul(count),
            Piece::Group(arms) => {
                arms.iter().map(String::len).sum::<usize>() * (count / arms.len())
            }
        };

        bytes.saturating_add(share)
    });

    if bytes > EXPANDED_LIMIT {
        return None;
    }

    let mut alternatives = vec![String::new()];
    for piece in pieces {
        alternatives = match piece {
            Piece::Text(text) => alternatives
                .into_iter()
                .map(|prefix| prefix + text)
                .collect(),
            Piece::Group(arms) => alternatives
                .iter()
                .flat_map(|prefix| arms.iter().map(move |arm| format!("{prefix}{arm}")))
                .collect(),
        };
    }

    Some(alternatives)
}

fn compiled_segment(segment: &str, policy: Policy) -> Option<Segment> {
    if segment == "**" {
        return Some(Segment::Any);
    }

    if segment.is_empty() || segment == "." || segment == ".." {
        return None;
    }

    let mut atoms = Vec::new();
    let mut chars = segment.chars().peekable();
    while let Some(char) = chars.next() {
        match char {
            '*' if chars.peek() == Some(&'*') => return None,
            '*' => atoms.push(Atom::Star),
            '\\' => {
                let literal = chars.next().expect("spellings escape one character");
                atoms.extend(
                    encode(literal.encode_utf8(&mut [0; 4]), policy.units)
                        .into_iter()
                        .map(Atom::Unit),
                );
            }
            '?' if policy.classes => atoms.push(Atom::One),
            '[' if policy.classes => {
                let content: String = chars.by_ref().take_while(|&char| char != ']').collect();
                atoms.push(Atom::Class(class(&content, policy.units)?));
            }
            '?' | '[' => return None,
            _ => atoms.extend(
                encode(char.encode_utf8(&mut [0; 4]), policy.units)
                    .into_iter()
                    .map(Atom::Unit),
            ),
        }
    }

    Some(Segment::Atoms(atoms))
}

fn class(content: &str, units: Units) -> Option<Vec<(u16, u16)>> {
    if content.is_empty() || content.starts_with(['!', '^']) {
        return None;
    }

    let single = |char: char| match encode(char.encode_utf8(&mut [0; 4]), units)[..] {
        [unit] => Some(unit),
        _ => None,
    };

    let chars: Vec<char> = content.chars().collect();
    let mut ranges = Vec::new();
    let mut at = 0;
    while at < chars.len() {
        if at + 2 < chars.len() && chars[at + 1] == '-' {
            let (first, last) = (single(chars[at])?, single(chars[at + 2])?);
            if first > last || (first..=last).contains(&u16::from(b'/')) {
                return None;
            }

            ranges.push((first, last));
            at += 3;
        } else {
            let encoded = encode(chars[at].encode_utf8(&mut [0; 4]), units);
            ranges.extend(encoded.into_iter().map(|unit| (unit, unit)));
            at += 1;
        }
    }

    ranges.sort_unstable();
    ranges.dedup();
    Some(ranges)
}

#[cfg(test)]
#[path = "override_glob_tests.rs"]
mod tests;
