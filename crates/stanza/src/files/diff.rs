use std::collections::{BTreeSet, HashMap, HashSet};
use std::ops::{Range, RangeInclusive};

use super::codes::Codes;
use crate::text::{is_js_line_terminator, is_js_whitespace, is_js_word, js_trim, utf16_len};

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Changed {
    pub lines: BTreeSet<usize>,
    pub deleted_after: BTreeSet<usize>,
}

fn braces(line: &str) -> usize {
    line.chars()
        .filter(|&char| char == '{' || char == '}')
        .count()
}

fn brace_only(line: &str) -> bool {
    line.chars()
        .all(|char| is_js_whitespace(char) || char == '{' || char == '}')
        && braces(line) > 0
}

fn starts_at(chars: &[char], at: usize, word: &str) -> bool {
    word.chars()
        .enumerate()
        .all(|(index, char)| chars.get(at + index) == Some(&char))
}

fn bounded_before(chars: &[char], at: usize) -> bool {
    at == 0 || !is_js_word(chars[at - 1])
}

fn bounded_after(chars: &[char], at: usize) -> bool {
    chars.get(at).is_none_or(|&char| !is_js_word(char))
}

fn skip_space(chars: &[char], mut at: usize) -> usize {
    while chars.get(at).is_some_and(|&char| is_js_whitespace(char)) {
        at += 1;
    }

    at
}

fn literals(line: &str) -> (String, String) {
    let chars: Vec<char> = line.chars().collect();
    let mut code = String::new();
    let mut text = String::new();

    let mut index = 0;
    while index < chars.len() {
        let char = chars[index];
        let next = chars.get(index + 1).copied();
        let closes = (char == '/' && next == Some('*'))
            .then(|| {
                (index + 2..chars.len().saturating_sub(1)).find(|&at| starts_at(&chars, at, "*/"))
            })
            .flatten();

        if let Some(closes) = closes {
            text.extend(&chars[index..closes + 2]);
            text.push('\0');
            code.push(' ');
            index = closes + 2;
            continue;
        }

        if char == '/' && matches!(next, Some('/' | '*')) {
            code.push(char);
            code.extend(next);
            text.extend(&chars[index + 2..]);
            return (code, text);
        }

        code.push(char);
        if !matches!(char, '"' | '\'' | '`') {
            index += 1;
            continue;
        }

        let mut end = index + 1;
        while end < chars.len() && chars[end] != char {
            end += if chars[end] == '\\' { 2 } else { 1 };
        }

        text.extend(&chars[index + 1..end.min(chars.len())]);
        text.push('\0');
        code.extend(chars.get(end));
        index = end + 1;
    }

    (code, text)
}

fn opening_head(chars: &[char], at: usize) -> Option<usize> {
    if chars[at] == ')' {
        return Some(at + 1);
    }

    if !bounded_before(chars, at) {
        return None;
    }

    ["else", "do"]
        .into_iter()
        .find(|word| starts_at(chars, at, word))
        .map(|word| at + word.len())
}

fn closing_reach(chars: &[char]) -> Option<RangeInclusive<usize>> {
    let last = chars.iter().rposition(|&char| !is_js_whitespace(char))?;
    if chars[last] != '}' {
        return None;
    }

    let after_break = chars[..last]
        .iter()
        .rposition(|&char| is_js_line_terminator(char))
        .map_or(0, |at| at + 1);

    Some(after_break..=last)
}

fn closes_line(chars: &[char], from: usize, reach: Option<&RangeInclusive<usize>>) -> bool {
    let after_space = skip_space(chars, from);
    after_space == chars.len()
        || starts_at(chars, after_space, "//")
        || starts_at(chars, after_space, "/*")
        || reach.is_some_and(|reach| reach.contains(&from))
}

fn open_brace_end(
    chars: &[char],
    at: usize,
    reach: Option<&RangeInclusive<usize>>,
) -> Option<(usize, usize)> {
    let head = opening_head(chars, at)?;
    let brace = skip_space(chars, head);
    if chars.get(brace) != Some(&'{') || !closes_line(chars, brace + 1, reach) {
        return None;
    }

    Some((head, skip_space(chars, brace + 1)))
}

fn close_brace_end(chars: &[char], at: usize) -> Option<usize> {
    let brace = skip_space(chars, at);
    if chars.get(brace) != Some(&'}') {
        return None;
    }

    let after = skip_space(chars, brace + 1);
    let follows = after == chars.len()
        || (starts_at(chars, after, "else") && bounded_after(chars, after + 4))
        || (starts_at(chars, after, "while") && bounded_after(chars, after + 5))
        || starts_at(chars, after, "//")
        || starts_at(chars, after, "/*")
        || chars[after] == '}';

    follows.then_some(after)
}

fn unbraced_code(line: &str) -> String {
    let chars: Vec<char> = line.chars().collect();
    let indent = skip_space(&chars, 0);
    let chars: Vec<char> = if chars.get(indent) == Some(&'}') {
        let rest = skip_space(&chars, indent + 1);
        chars[..indent]
            .iter()
            .chain(&chars[rest..])
            .copied()
            .collect()
    } else {
        chars
    };

    let reach = closing_reach(&chars);
    let mut opened = Vec::with_capacity(chars.len());
    let mut at = 0;
    while at < chars.len() {
        match open_brace_end(&chars, at, reach.as_ref()) {
            Some((head, end)) => {
                opened.extend(&chars[at..head]);
                opened.push(' ');
                at = end;
            }
            None => {
                opened.push(chars[at]);
                at += 1;
            }
        }
    }

    let mut closed = String::with_capacity(opened.len());
    let mut at = 0;
    while at < opened.len() {
        match close_brace_end(&opened, at) {
            Some(end) => {
                closed.push(' ');
                at = end;
            }
            None => {
                let next = skip_space(&opened, at).max(at + 1);
                closed.extend(&opened[at..next]);
                at = next;
            }
        }
    }

    closed.trim_end_matches(is_js_whitespace).to_owned()
}

fn without_carriage_return(line: &str) -> &str {
    line.strip_suffix('\r').unwrap_or(line)
}

fn unbraced(line: &str) -> String {
    let (code, text) = literals(without_carriage_return(line));
    format!("{}\0{text}", unbraced_code(&code))
}

struct Piece {
    indent: String,
    code: String,
    text: String,
    opens: bool,
}

fn collapse_space(text: &str) -> String {
    let mut collapsed = String::with_capacity(text.len());
    let mut spaced = false;
    for char in text.chars() {
        if is_js_whitespace(char) {
            spaced = true;
            continue;
        }

        if spaced {
            collapsed.push(' ');
            spaced = false;
        }

        collapsed.push(char);
    }

    collapsed
}

fn ends_opening(line: &str) -> bool {
    let chars: Vec<char> = line.chars().collect();
    let ends_with_word = |word: &str| {
        let count = word.chars().count();
        chars.len() >= count
            && starts_at(&chars, chars.len() - count, word)
            && bounded_before(&chars, chars.len() - count)
    };

    chars.last() == Some(&')') || ends_with_word("else") || ends_with_word("do")
}

fn piece(line: &str) -> Piece {
    let (code, text) = literals(without_carriage_return(line));
    let unbraced_line = unbraced_code(&code);
    let indent = &code[..code.len() - code.trim_start_matches(is_js_whitespace).len()];
    Piece {
        indent: indent.to_owned(),
        code: collapse_space(js_trim(&unbraced_line)),
        text,
        opens: ends_opening(&unbraced_line),
    }
}

const EDIT_LIMIT: i64 = 2000;

#[cfg(test)]
thread_local! {
    static SEARCHES: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    static SKIPS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

fn shortest_edit(before: &[usize], after: &[usize]) -> Option<Vec<(usize, usize)>> {
    let (width, height) = (before.len() as i64, after.len() as i64);
    if (width - height).abs() > EDIT_LIMIT {
        return None;
    }

    let seen: HashSet<usize> = before.iter().copied().collect();
    if !after.iter().any(|id| seen.contains(id)) {
        return (width + height <= EDIT_LIMIT).then(Vec::new);
    }

    #[cfg(test)]
    SEARCHES.set(SEARCHES.get() + 1);

    let offset = EDIT_LIMIT + 1;
    let mut reach = vec![0i32; (2 * offset + 1) as usize];
    let at = |diagonal: i64| (offset + diagonal) as usize;
    let mut trace: Vec<Vec<i32>> = Vec::new();
    for edits in 0..=EDIT_LIMIT {
        for diagonal in (-edits..=edits).step_by(2) {
            let down = diagonal == -edits
                || (diagonal != edits && reach[at(diagonal - 1)] < reach[at(diagonal + 1)]);

            let mut x = i64::from(if down {
                reach[at(diagonal + 1)]
            } else {
                reach[at(diagonal - 1)] + 1
            });

            let mut y = x - diagonal;
            while x < width && y < height && before[x as usize] == after[y as usize] {
                x += 1;
                y += 1;
            }

            reach[at(diagonal)] = x as i32;
            if x < width || y < height {
                continue;
            }

            trace.push(reach[at(-edits)..=at(edits)].to_vec());
            return Some(backtrack(&trace, width, height));
        }

        trace.push(reach[at(-edits)..=at(edits)].to_vec());
    }

    None
}

fn backtrack(trace: &[Vec<i32>], width: i64, height: i64) -> Vec<(usize, usize)> {
    let mut pairs = Vec::new();

    let mut x = width;
    let mut y = height;
    for edits in (0..trace.len()).rev() {
        let mut start = (0, 0);
        let mut snake = (0, 0);
        if edits > 0 {
            let rounds = edits as i64;
            let diagonal = x - y;
            let previous = &trace[edits - 1];
            let read = |diagonal: i64| i64::from(previous[(diagonal + rounds - 1) as usize]);
            let down = diagonal == -rounds
                || (diagonal != rounds && read(diagonal - 1) < read(diagonal + 1));

            let from = if down { diagonal + 1 } else { diagonal - 1 };
            start = (read(from), read(from) - from);
            snake = if down {
                (start.0, start.1 + 1)
            } else {
                (start.0 + 1, start.1)
            };
        }

        while x > snake.0 && y > snake.1 {
            x -= 1;
            y -= 1;
            pairs.push((x as usize, y as usize));
        }

        (x, y) = start;
    }

    pairs.reverse();
    pairs
}

fn matches(before: &[&str], after: &[&str], ids: &mut Ids) -> Option<Vec<(usize, usize)>> {
    let mut head = 0;
    while head < before.len() && head < after.len() && ids.same(before[head], after[head]) {
        head += 1;
    }

    let mut tail = 0;
    while tail < before.len() - head
        && tail < after.len() - head
        && ids.same(
            before[before.len() - 1 - tail],
            after[after.len() - 1 - tail],
        )
    {
        tail += 1;
    }

    let middle_ids = |side: &[&str], ids: &mut Ids| -> Vec<usize> {
        side[head..side.len() - tail]
            .iter()
            .map(|text| ids.id(text))
            .collect()
    };

    let middle = shortest_edit(&middle_ids(before, ids), &middle_ids(after, ids))?;

    let mut pairs: Vec<(usize, usize)> = (0..head).map(|index| (index, index)).collect();
    pairs.extend(middle.into_iter().map(|(x, y)| (x + head, y + head)));
    pairs.extend((0..tail).map(|index| (before.len() - tail + index, after.len() - tail + index)));
    Some(pairs)
}

#[derive(Default)]
struct Lines<'a> {
    before: Vec<&'a str>,
    after: Vec<&'a str>,
    after_lines: Vec<usize>,
    at: usize,
}

impl<'a> Lines<'a> {
    fn push_after(&mut self, text: &'a str, line: usize) {
        self.after.push(text);
        self.after_lines.push(line);
    }
}

fn hunk_header(row: &str) -> Option<(Option<i64>, i64, Option<i64>)> {
    fn digits(text: &str) -> (&str, &str) {
        let end = text
            .find(|char: char| !char.is_ascii_digit())
            .unwrap_or(text.len());
        (&text[..end], &text[end..])
    }

    fn number(text: &str) -> Option<i64> {
        text.parse::<u32>().ok().map(i64::from)
    }

    fn count(text: &str) -> Option<(Option<i64>, &str)> {
        let Some(rest) = text.strip_prefix(',') else {
            return Some((None, text));
        };

        let (count, rest) = digits(rest);
        Some((Some(number(count)?), rest))
    }

    let rest = row.strip_prefix("@@ -")?;
    let (start, rest) = digits(rest);
    number(start)?;

    let (before, rest) = count(rest)?;
    let rest = rest.strip_prefix(" +")?;
    let (first, rest) = digits(rest);
    let first = number(first)?;

    let (after, rest) = count(rest)?;
    let starts_after_a_line = first > 0 || after == Some(0);
    (rest.starts_with(" @@") && starts_after_a_line).then_some((before, first, after))
}

fn sides<'a>(section: &[&'a str]) -> (Lines<'a>, Vec<Lines<'a>>) {
    let mut file = Lines::default();
    let mut blocks: Vec<Lines> = Vec::new();

    let mut line: i64 = 0;
    let mut before_left: i64 = 0;
    let mut after_left: i64 = 0;
    let mut in_block = false;
    for row in section {
        if let Some((before, first, after)) = hunk_header(row) {
            before_left = before.unwrap_or(1);
            after_left = after.unwrap_or(1);
            line = if after_left == 0 { first } else { first - 1 };
            in_block = false;
            continue;
        }

        if before_left == 0 && after_left == 0 {
            continue;
        }

        let text = row.get(1..).unwrap_or_default();
        if row.starts_with(' ') {
            before_left -= 1;
            after_left -= 1;
            line += 1;
            file.before.push(text);
            file.push_after(text, line as usize);
            in_block = false;
            continue;
        }

        if !row.starts_with('-') && !row.starts_with('+') {
            continue;
        }

        if !in_block {
            blocks.push(Lines {
                at: line as usize,
                ..Lines::default()
            });

            in_block = true;
        }

        let block = blocks.last_mut().expect("a block was just opened");
        if row.starts_with('+') {
            after_left -= 1;
            line += 1;
            file.push_after(text, line as usize);
            block.push_after(text, line as usize);
        } else {
            before_left -= 1;
            if brace_only(text) {
                continue;
            }

            file.before.push(text);
            block.before.push(text);
        }
    }

    file.at = file.after_lines.first().map_or(1, |&line| line) - 1;
    (file, blocks)
}

struct Group<'a> {
    first: usize,
    last: usize,
    text: &'a str,
}

struct Join<'a> {
    indent: String,
    key: String,
    line: &'a str,
}

struct Joins<'a> {
    sorted: Vec<Join<'a>>,
    longest: usize,
}

impl<'a> Joins<'a> {
    fn new(after: &[&'a str]) -> Self {
        let mut longest = 0;
        let mut sorted: Vec<Join> = Vec::with_capacity(after.len());
        for &line in after {
            let Piece {
                indent, code, text, ..
            } = piece(line);
            longest = longest.max(utf16_len(&code));
            sorted.push(Join {
                indent,
                key: format!("{code}\0{text}"),
                line,
            });
        }

        sorted.sort_by(|a, b| (&a.indent, &a.key).cmp(&(&b.indent, &b.key)));
        Self { sorted, longest }
    }

    fn starting(&self, indent: &str, code: &str) -> Range<usize> {
        let start = self
            .sorted
            .partition_point(|join| join.indent.as_str() < indent);
        let end = self
            .sorted
            .partition_point(|join| join.indent.as_str() <= indent);
        self.narrow(start..end, 0, code)
    }

    fn narrow(&self, range: Range<usize>, at: usize, extension: &str) -> Range<usize> {
        let within = &self.sorted[range.clone()];
        let start = within.partition_point(|join| &join.key[at..] < extension);
        let end = within.partition_point(|join| {
            let rest = &join.key[at..];
            rest < extension || rest.starts_with(extension)
        });
        range.start + start..range.start + end
    }

    fn find(&self, range: Range<usize>, at: usize, text: &str) -> Option<&'a str> {
        let tail = || [0].iter().chain(text.as_bytes());
        let within = &self.sorted[range];
        let through = within.partition_point(|join| join.key.as_bytes()[at..].iter().le(tail()));
        let join = within[..through].last()?;
        join.key.as_bytes()[at..]
            .iter()
            .eq(tail())
            .then_some(join.line)
    }

    fn starts(&self, pieces: &[Piece]) -> Vec<bool> {
        let everywhere = vec![true; pieces.len()];
        let mut joined = String::new();
        let mut texts = Texts(vec![(0, 0)]);
        let mut offsets = Vec::with_capacity(pieces.len());
        for (index, piece) in pieces.iter().enumerate() {
            texts.push(&piece.text);
            if piece.code.is_empty() {
                continue;
            }

            if !joined.is_empty() {
                joined.push(' ');
            }

            offsets.push((joined.len(), index));
            joined.push_str(&piece.code);
        }

        if joined.contains('\0') {
            return everywhere;
        }

        let mut reached = vec![0i64; self.sorted.len() + 1];
        for piece in pieces.iter().filter(|piece| piece.opens) {
            let range = self.starting(&piece.indent, &piece.code);
            reached[range.start] += 1;
            reached[range.end] -= 1;
        }

        let mut budget = 8 * joined.len();
        let mut codes = Codes::new();
        let mut indents: HashMap<&str, usize> = HashMap::new();
        let mut keys = HashSet::new();
        let (removed_text, _) = texts.0[texts.0.len() - 1];
        let mut walks = 0;
        for (join, &change) in self.sorted.iter().zip(&reached) {
            walks += change;
            let (code, text) = join.key.split_once('\0').unwrap_or((&join.key, ""));
            if walks == 0 || code.len() > joined.len() || text.len() > removed_text {
                continue;
            }

            let Some(node) = codes.insert(code, &mut budget) else {
                return everywhere;
            };

            let next = indents.len();
            let indent = *indents.entry(join.indent.as_str()).or_insert(next);
            keys.insert((node, indent, text.len(), Texts::hash(text)));
        }

        codes.link();

        let mut followers = vec![pieces.len(); pieces.len()];
        for at in (0..pieces.len().saturating_sub(1)).rev() {
            let next = &pieces[at + 1];
            followers[at] = match next.code.is_empty() && next.text.is_empty() {
                true => followers[at + 1],
                false => at + 1,
            };
        }

        let mut starts = vec![false; pieces.len()];
        let mut state = 0;
        let mut ending = offsets
            .iter()
            .map(|&(offset, index)| (offset + pieces[index].code.len(), index))
            .peekable();
        for (offset, &byte) in joined.as_bytes().iter().enumerate() {
            state = codes.step(state, byte);
            let Some((_, last)) = ending.next_if(|&(end, _)| end == offset + 1) else {
                continue;
            };

            for (node, length) in codes.matches(state) {
                let Some(left) = budget.checked_sub(1) else {
                    return everywhere;
                };

                budget = left;
                let Ok(found) =
                    offsets.binary_search_by_key(&(offset + 1 - length), |&(offset, _)| offset)
                else {
                    continue;
                };

                let first = offsets[found].1;
                let Some(&indent) = indents.get(pieces[first].indent.as_str()) else {
                    continue;
                };

                let joins = |through: usize| {
                    let (length, hash) = texts.slice(first, through);
                    keys.contains(&(node, indent, length, hash))
                };

                let follower = followers[last];
                starts[first] |= pieces[first].opens
                    && (joins(last) || (follower < pieces.len() && joins(follower)));
            }
        }

        starts
    }
}

struct Texts(Vec<(usize, u64)>);

impl Texts {
    const MODULUS: u64 = (1 << 61) - 1;
    const BASE: u64 = 1_000_003;

    fn times(left: u64, right: u64) -> u64 {
        (u128::from(left) * u128::from(right) % u128::from(Self::MODULUS)) as u64
    }

    fn extend(hash: u64, text: &str) -> u64 {
        text.bytes().fold(hash, |hash, byte| {
            (Self::times(hash, Self::BASE) + u64::from(byte) + 1) % Self::MODULUS
        })
    }

    fn hash(text: &str) -> u64 {
        Self::extend(0, text)
    }

    fn push(&mut self, text: &str) {
        let (length, hash) = *self.0.last().expect("seeded with an empty text");
        self.0.push((length + text.len(), Self::extend(hash, text)));
    }

    fn slice(&self, first: usize, last: usize) -> (usize, u64) {
        let ((start, before), (end, through)) = (self.0[first], self.0[last + 1]);
        let mut power = 1;
        let (mut base, mut exponent) = (Self::BASE, end - start);
        while exponent > 0 {
            if exponent & 1 == 1 {
                power = Self::times(power, base);
            }

            base = Self::times(base, base);
            exponent >>= 1;
        }

        let shifted = Self::times(before, power);
        (
            end - start,
            (through + Self::MODULUS - shifted) % Self::MODULUS,
        )
    }
}

fn rejoin<'a>(before: &[&'a str], from: usize, to: usize, after: &[&'a str]) -> Vec<Group<'a>> {
    let joins = Joins::new(after);
    let pieces: Vec<Piece> = before[from..to].iter().map(|line| piece(line)).collect();
    let mut starts = None;
    let mut steps = 0;

    let mut groups = Vec::new();
    let mut index = from;
    while index < to {
        let first = &pieces[index - from];

        let mut last = index;
        let walked_the_block = steps > pieces.len();
        let skipped = walked_the_block
            && first.opens
            && !starts.get_or_insert_with(|| joins.starts(&pieces))[index - from];

        #[cfg(test)]
        SKIPS.set(SKIPS.get() + usize::from(skipped));

        let mut range = if first.opens && !skipped {
            joins.starting(&first.indent, &first.code)
        } else {
            0..0
        };

        let mut at = first.code.len();
        let mut length = utf16_len(&first.code);
        let mut text = first.text.clone();
        let mut found = None;
        if first.opens && joins.find(range.clone(), at, &text).is_none() {
            let mut next = index + 1;
            while next < to && !range.is_empty() && length < joins.longest {
                steps += 1;
                let part = &pieces[next - from];
                if part.code.is_empty() && part.text.is_empty() {
                    next += 1;
                    continue;
                }

                if !part.code.is_empty() {
                    range = joins.narrow(range, at, &format!(" {}", part.code));
                    at += 1 + part.code.len();
                    length += 1 + utf16_len(&part.code);
                }

                text.push_str(&part.text);
                found = joins.find(range.clone(), at, &text);
                if found.is_some() {
                    last = next;
                    break;
                }

                if !part.opens {
                    break;
                }

                next += 1;
            }
        }

        groups.push(Group {
            first: index,
            last,
            text: found.unwrap_or(before[index]),
        });

        index = last + 1;
    }

    groups
}

struct Pair<'a> {
    first: usize,
    last: usize,
    text: &'a str,
    y: usize,
}

struct Ids(HashMap<String, usize>);

impl Ids {
    fn same(&mut self, before: &str, after: &str) -> bool {
        before == after || self.id(before) == self.id(after)
    }

    fn id(&mut self, text: &str) -> usize {
        let key = if brace_only(text) {
            text.to_owned()
        } else {
            unbraced(text)
        };

        let next = self.0.len();
        *self.0.entry(key).or_insert(next)
    }
}

fn blank(line: &str) -> bool {
    line.chars().all(is_js_whitespace)
}

fn anchors_blank(before: &[&str], (x, _): (usize, usize)) -> bool {
    before.get(x).is_some_and(|line| blank(line))
}

fn slid_blanks(before: &[&str], pairs: &[(usize, usize)], index: usize) -> Option<Vec<usize>> {
    if !anchors_blank(before, pairs[index]) {
        return None;
    }

    let mut end = index;
    while pairs[end + 1].1 == pairs[end].1 + 1 && anchors_blank(before, pairs[end + 1]) {
        end += 1;
    }

    let (next_x, next_y) = pairs[end + 1];
    if next_y != pairs[end].1 + 1 {
        return None;
    }

    let blanks: Vec<usize> = (pairs[index].0..next_x)
        .filter(|&at| blank(before[at]))
        .collect();

    let slid = blanks[blanks.len() - (end + 1 - index)..].to_vec();
    (slid[0] > pairs[index].0).then_some(slid)
}

fn classify(lines: &Lines, changed: &mut Changed) -> bool {
    let Lines {
        before,
        after,
        after_lines,
        at,
    } = lines;

    let mut at = *at;
    let mut ids = Ids(HashMap::new());
    let Some(pairs) = matches(before, after, &mut ids) else {
        return false;
    };

    let mut pairs = pairs;
    pairs.push((before.len(), after.len()));

    let mut paired = Vec::new();

    let mut x = 0;
    let mut y = 0;
    for index in 0..pairs.len() {
        let (pair_x, pair_y) = pairs[index];
        let mut groups = Vec::new();
        if x < pair_x && y < pair_y {
            groups = rejoin(before, x, pair_x, &after[y..pair_y]);
            if let Some(slid) = slid_blanks(before, &pairs, index) {
                let through = rejoin(before, x, slid[0], &after[y..pair_y]);
                if through
                    .iter()
                    .any(|group| group.first < pair_x && group.last > pair_x)
                {
                    groups = through;
                    for (offset, slid_x) in slid.into_iter().enumerate() {
                        pairs[index + offset].0 = slid_x;
                    }
                }
            }
        }

        let pair_x = pairs[index].0;

        if groups.len() < pair_x - x {
            let texts: Vec<&str> = groups.iter().map(|group| group.text).collect();
            for (group_x, group_y) in
                matches(&texts, &after[y..pair_y], &mut ids).unwrap_or_default()
            {
                let group = &groups[group_x];
                paired.push(Pair {
                    first: group.first,
                    last: group.last,
                    text: group.text,
                    y: y + group_y,
                });
            }
        }

        if pair_x < before.len() {
            paired.push(Pair {
                first: pair_x,
                last: pair_x,
                text: before[pair_x],
                y: pair_y,
            });
        }

        x = pair_x + 1;
        y = pair_y + 1;
    }

    x = 0;
    y = 0;
    paired.push(Pair {
        first: before.len(),
        last: before.len(),
        text: "",
        y: after.len(),
    });

    for pair in &paired {
        if x < pair.first {
            changed.deleted_after.insert(at);
        }

        while y < pair.y {
            at = after_lines[y];
            changed.lines.insert(at);
            y += 1;
        }

        if pair.first == before.len() {
            break;
        }

        let (now, line) = (after[pair.y], after_lines[pair.y]);
        if pair.text != now && braces(now) >= braces(pair.text) {
            changed.lines.insert(line);
        }

        at = line;
        x = pair.last + 1;
        y = pair.y + 1;
    }

    true
}

pub fn diff_changes(section: &[&str]) -> Changed {
    let (file, blocks) = sides(section);
    let mut changed = Changed::default();
    if !classify(&file, &mut changed) {
        for block in &blocks {
            let whole =
                block.before.len() == file.before.len() && block.after.len() == file.after.len();
            if !whole && classify(block, &mut changed) {
                continue;
            }

            if !block.before.is_empty() {
                changed.deleted_after.insert(block.at);
            }

            changed.lines.extend(&block.after_lines);
        }
    }

    let lines = &changed.lines;
    changed
        .deleted_after
        .retain(|line| !lines.contains(line) && !lines.contains(&(line + 1)));

    changed
}

pub fn diff_sections(text: &str) -> Vec<Vec<&str>> {
    let mut sections: Vec<Vec<&str>> = Vec::new();
    for line in text.split('\n') {
        let repeats = sections
            .last()
            .is_some_and(|section| section.first() == Some(&line));

        if line.starts_with("diff --git ") && !repeats {
            sections.push(Vec::new());
        }

        if let Some(section) = sections.last_mut() {
            section.push(line);
        }
    }

    sections
}

#[cfg(test)]
mod tests {
    use super::*;

    fn changed(lines: &[usize], deleted_after: &[usize]) -> Changed {
        Changed {
            lines: lines.iter().copied().collect(),
            deleted_after: deleted_after.iter().copied().collect(),
        }
    }

    fn one_line(removed: &str, added: &str) -> Changed {
        let removed = format!("-{removed}");
        let added = format!("+{added}");
        diff_changes(&[
            "diff --git a/a.ts b/a.ts",
            "@@ -1,2 +1,2 @@",
            &removed,
            &added,
            " y();",
        ])
    }

    #[test]
    fn hunk_headers_git_never_writes_are_skipped() {
        let none = changed(&[], &[]);
        for header in [
            "@@ -1,2 +0,3 @@",
            "@@ -1 +0 @@",
            "@@ -1,2 +4294967296,2 @@",
            "@@ -1,4294967296 +1,2 @@",
            "@@ -1,2 +9223372036854775807,2 @@",
        ] {
            let section = [
                "diff --git a/a.ts b/a.ts",
                header,
                " x();",
                "+y();",
                "+z();",
            ];
            assert_eq!(diff_changes(&section), none, "{header}");
        }

        let deleted = [
            "diff --git a/a.ts b/a.ts",
            "@@ -1,2 +0,0 @@",
            "-x();",
            "-y();",
        ];
        assert_eq!(diff_changes(&deleted), changed(&[], &[0]));
    }

    #[test]
    fn hunks_drop_only_braces_stanza_removes_not_braces_added_or_inside_strings_and_comments() {
        let none = changed(&[], &[]);
        assert_eq!(one_line("  if (a) {", "  if (a)"), none);
        assert_eq!(one_line("  } else {", "  else"), none);
        assert_eq!(one_line("  if (a) { x(); }", "  if (a) x();"), none);
        assert_eq!(one_line("  } else { // why", "  else // why"), none);
        assert_eq!(one_line("  if (s === \"}\") {", "  if (s === \"}\")"), none);

        let first = changed(&[1], &[]);
        assert_eq!(one_line("  if (a) x();", "  if (a) { x(); }"), first);
        assert_eq!(
            one_line("  const s = \") {\";", "  const s = \") \";"),
            first
        );
        assert_eq!(one_line("  const { a } = o;", "  const a = o;"), first);
        assert_eq!(one_line("  foo(); // }", "  foo(); //"), first);
        assert_eq!(
            one_line("  const s = \"} else\";", "  const s = \" else\";"),
            first
        );
        assert_eq!(one_line("  }", "  } "), first);
    }

    fn joined(removed: &[String], added: &str) -> Changed {
        let header = format!("@@ -1,{} +1,2 @@", removed.len() + 1);
        let added = format!("+{added}");
        let mut section = vec!["diff --git a/a.ts b/a.ts".to_owned(), header];
        section.extend(removed.iter().map(|line| format!("-{line}")));
        section.extend([added, " y();".to_owned()]);
        let rows: Vec<&str> = section.iter().map(String::as_str).collect();
        diff_changes(&rows)
    }

    fn owned(lines: &[&str]) -> Vec<String> {
        lines.iter().map(|&line| line.to_owned()).collect()
    }

    #[test]
    fn hunks_match_a_body_stanza_joined_onto_its_header_and_nothing_else() {
        let none = changed(&[], &[]);
        let body = |lines: &[&str]| joined(&owned(lines), "  if (a) x();");
        assert_eq!(body(&["  if (a) {", "    x();", "  }"]), none);
        assert_eq!(body(&["  if (a)", "  {", "    x();", "  }"]), none);
        assert_eq!(
            joined(
                &owned(&["  if (a) /* note */ {", "    x();", "  }"]),
                "  if (a) /* note */ x();"
            ),
            none
        );
        assert_eq!(body(&["  if (a) {", "", "    x();", "  }"]), none);

        let names: Vec<char> = "abcdefghijkl".chars().collect();
        let mut nested: Vec<String> = names
            .iter()
            .map(|name| format!("  if ({name}) {{"))
            .collect();
        nested.push("    x();".to_owned());
        nested.extend(names.iter().map(|_| "  }".to_owned()));
        let headers: String = names.iter().map(|name| format!("if ({name}) ")).collect();
        assert_eq!(joined(&nested, &format!("  {headers}x();")), none);

        assert_eq!(
            joined(
                &owned(&["  if (a) {", "    return typeof x;", "  }"]),
                "  if (a) return typeofx;"
            ),
            changed(&[1], &[])
        );
    }

    fn rewritten(before: &str, after: &str) -> Changed {
        let lines = |text: &str| -> Vec<String> {
            text.split_inclusive('\n')
                .map(|line| line.strip_suffix('\n').unwrap_or(line).to_owned())
                .collect()
        };

        let (before, after) = (lines(before), lines(after));
        let mut section = vec![
            "diff --git a/a.ts b/a.ts".to_owned(),
            format!("@@ -1,{} +1,{} @@", before.len(), after.len()),
        ];
        section.extend(before.iter().map(|line| format!("-{line}")));
        section.extend(after.iter().map(|line| format!("+{line}")));
        let rows: Vec<&str> = section.iter().map(String::as_str).collect();
        diff_changes(&rows)
    }

    fn rejoined<'a>(
        before: &[&'a str],
        from: usize,
        to: usize,
        after: &[&'a str],
    ) -> Vec<(usize, usize, &'a str)> {
        rejoin(before, from, to, after)
            .into_iter()
            .map(|group| (group.first, group.last, group.text))
            .collect()
    }

    fn assert_rejoins_like_a_walk(before: &[&str], from: usize, to: usize, after: &[&str]) {
        let groups = rejoined(before, from, to, after);
        assert_eq!(
            groups,
            walked(before, from, to, after),
            "{before:?} {from}..{to} {after:?}"
        );

        let pieces: Vec<Piece> = before[from..to].iter().map(|line| piece(line)).collect();
        let starts = Joins::new(after).starts(&pieces);
        for start in from..to {
            let (_, last, _) = walked(before, start, to, after)[0];
            assert!(
                last == start || starts[start - from],
                "{before:?} {start}..{to} {after:?}"
            );
        }
    }

    fn walked<'a>(
        before: &[&'a str],
        from: usize,
        to: usize,
        after: &[&'a str],
    ) -> Vec<(usize, usize, &'a str)> {
        let mut lines: HashMap<(String, String), &str> = HashMap::new();
        let mut longest = 0;
        for &line in after {
            let Piece {
                indent, code, text, ..
            } = piece(line);
            longest = longest.max(utf16_len(&code));
            lines.insert((indent, format!("{code}\0{text}")), line);
        }

        let mut groups = Vec::new();
        let mut index = from;
        while index < to {
            let first = piece(before[index]);
            let key = |code: &str, text: &str| (first.indent.clone(), format!("{code}\0{text}"));
            let prefixed = |code: &str| {
                lines
                    .keys()
                    .any(|(indent, key)| *indent == first.indent && key.starts_with(code))
            };

            let mut code = first.code.clone();
            let mut text = first.text.clone();
            let mut group = (index, index, before[index]);
            if first.opens && prefixed(&code) && !lines.contains_key(&key(&code, &text)) {
                for (next, removed) in before.iter().enumerate().take(to).skip(index + 1) {
                    if utf16_len(&code) >= longest {
                        break;
                    }

                    let part = piece(removed);
                    if part.code.is_empty() && part.text.is_empty() {
                        continue;
                    }

                    if !part.code.is_empty() {
                        code = format!("{code} {}", part.code);
                    }

                    text.push_str(&part.text);
                    if let Some(&line) = lines.get(&key(&code, &text)) {
                        group = (index, next, line);
                        break;
                    }

                    if !part.opens || !prefixed(&code) {
                        break;
                    }
                }
            }

            groups.push(group);
            index = group.1 + 1;
        }

        groups
    }

    #[test]
    fn rejoin_pairs_what_a_walk_from_each_header_pairs() {
        let headers = [
            "if (a)",
            "  if (a)",
            "if (a) if (a)",
            "else",
            "do",
            "if (\"s\")",
            "while (é)",
            "if (a) /* c */",
            "while (é) if (a)",
        ];
        let lines = [
            "",
            "  ",
            "f();",
            "g();",
            "/* c */",
            "if (a) // c",
            "if (a) f();",
            "if (a) if (a) f();",
            "if (a) if (a) g();",
            "else f();",
            "if (a) /* c */ f();",
            "if (\"s\") f();",
        ];

        let comment_after_a_header: (&[&str], &[&str]) = (
            &["x();", "if (a)", "/* c */"],
            &["y();", "if (a) /* c */", "longer();"],
        );
        let nul_in_added_code: (&[&str], &[&str]) = (&["if (a)", "b\"s\0\""], &["if (a) b\"\"\0s"]);
        let nul_in_removed_code: (&[&str], &[&str]) = (&["if (a)", "b\0c;"], &["if (a) b\0c;"]);
        for (before, after) in [
            comment_after_a_header,
            nul_in_added_code,
            nul_in_removed_code,
        ] {
            let groups = rejoined(before, 0, before.len(), after);
            assert!(groups.iter().any(|(first, last, _)| first < last));
            assert_rejoins_like_a_walk(before, 0, before.len(), after);
        }

        let mut before = vec!["if (a)"; 12];
        before.extend(["f();", "if (b)", "g();"]);
        let after = [
            format!("{}h();", "if (a) ".repeat(12)),
            "if (a) /* x */".to_owned(),
            "if (b) g();".to_owned(),
        ];
        let after: Vec<&str> = after.iter().map(String::as_str).collect();
        SKIPS.set(0);
        let groups = rejoined(&before, 0, before.len(), &after);
        assert!(SKIPS.get() > 0, "the filter skipped no walk");
        assert!(groups.contains(&(13, 14, "if (b) g();")));
        assert_rejoins_like_a_walk(&before, 0, before.len(), &after);

        let mut seed: u64 = 0x9e37_79b9_7f4a_7c15;
        let mut next = |bound: usize| {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            (seed % bound as u64) as usize
        };

        let mut joins = 0;
        for _ in 0..20_000 {
            let before: Vec<&str> = (0..next(9) + 1)
                .map(|_| match next(3) {
                    0 => lines[next(lines.len())],
                    _ => headers[next(headers.len())],
                })
                .collect();

            let from = next(before.len()) * next(2);
            let to = from + 1 + next(before.len() - from);
            let joined: Vec<String> = (0..next(4) + 1)
                .map(|_| {
                    if next(4) == 0 {
                        return lines[next(lines.len())].to_owned();
                    }

                    let first = from + next(to - from);
                    let last = first + next(to - first);
                    let parts: Vec<&str> = before[first..=last]
                        .iter()
                        .map(|line| line.trim())
                        .collect();
                    let indent =
                        &before[first][..before[first].len() - before[first].trim_start().len()];
                    format!("{indent}{}", parts.join(" "))
                })
                .collect();

            let after: Vec<&str> = joined.iter().map(String::as_str).collect();
            assert_rejoins_like_a_walk(&before, from, to, &after);
            let groups = rejoined(&before, from, to, &after);

            joins += groups
                .iter()
                .filter(|(first, last, _)| first < last)
                .count();
        }

        assert!(joins > 2_000, "{joins} joins");
    }

    #[test]
    fn a_join_counts_the_blank_lines_it_consumed_and_only_those_as_unchanged() {
        let none = changed(&[], &[]);
        for ending in ["\n", "\r\n"] {
            let before = "  } else {\n\n    call(c);\n  }\n".replace('\n', ending);
            let after = "  } else call(c);\n".replace('\n', ending);
            assert_eq!(rewritten(&before, &after), none, "{ending:?}");
        }

        let blank_inside_and_after_the_body = [
            "diff --git a/a.ts b/a.ts",
            "@@ -1,12 +1,10 @@",
            " function f(a, c) {",
            "   zero();",
            "   if (a) {",
            "-    one();",
            "+    one(1);",
            "     two();",
            "-  } else {",
            "-",
            "-    call(c);",
            "-  }",
            "+  } else call(c);",
            " ",
            " }",
            "+",
            " function g() {}",
        ];
        assert_eq!(
            diff_changes(&blank_inside_and_after_the_body),
            changed(&[4, 9], &[])
        );

        let two_blanks_inside_and_after_the_body = [
            "diff --git a/a.ts b/a.ts",
            "@@ -1,14 +1,11 @@",
            " function f(a, c) {",
            "   zero();",
            "   if (a) {",
            "-    one();",
            "+    one(1);",
            "     two();",
            "-  } else {",
            "-",
            "-",
            "-    call(c);",
            "-  }",
            "+  } else call(c);",
            " ",
            " ",
            " }",
            "+",
            " function g() {}",
        ];
        assert_eq!(
            diff_changes(&two_blanks_inside_and_after_the_body),
            changed(&[4, 10], &[])
        );

        let more_blanks_after_the_body_than_inside = [
            "diff --git a/a.ts b/a.ts",
            "@@ -1,15 +1,12 @@",
            " function f(a, c) {",
            "   zero();",
            "   if (a) {",
            "-    one();",
            "+    one(1);",
            "     two();",
            "-  } else {",
            "-",
            "-",
            "-    call(c);",
            "-  }",
            "+  } else call(c);",
            " ",
            " ",
            " ",
            " }",
            "+",
            " function g() {}",
        ];
        assert_eq!(
            diff_changes(&more_blanks_after_the_body_than_inside),
            changed(&[4, 11], &[])
        );

        assert_eq!(
            rewritten("a();\n\nb();\n", "a();\nb();\n"),
            changed(&[], &[1])
        );
        assert_eq!(
            rewritten("const s = \"}\";\n", "const s = \"\";\n"),
            changed(&[1], &[])
        );
        assert_eq!(
            rewritten("const s = \"\";\n", "const s = \"}\";\n"),
            changed(&[1], &[])
        );
    }

    fn stays_linear(what: &str, line: impl Fn(usize) -> (String, String)) {
        use crate::engine::tests::{LINEAR_GROWTH_CAP, assert_bounded, cpu_ms, ratio};

        assert_bounded(
            what,
            &[1_000, 4_000],
            7,
            ratio(LINEAR_GROWTH_CAP),
            line,
            |(removed, added)| {
                cpu_ms(|| {
                    one_line(removed, added);
                })
            },
        );
    }

    #[test]
    fn classifying_a_changed_line_stays_linear_in_its_length() {
        let edited = |unit: &str| {
            let unit = unit.to_owned();
            move |count: usize| {
                let removed = unit.repeat(count);
                (removed.clone(), removed.replacen('a', "c", 1))
            }
        };

        stays_linear("a*b;", edited("a*b;"));
        stays_linear("if(a){f();}", edited("if(a){f();}"));
        stays_linear("a spaced brace", |count| {
            (format!("a{}}}b", " ".repeat(count * 4)), "c".to_owned())
        });
    }

    #[test]
    fn added_text_longer_than_every_removed_text_spends_no_budget() {
        let before = vec!["if (a)"; 200];
        let mut after = vec![format!("{}g();", "if (a) ".repeat(200))];
        after.extend((1..=100).map(|count| format!("{}/* x */", "if (a) ".repeat(count))));
        let after: Vec<&str> = after.iter().map(String::as_str).collect();
        let pieces: Vec<Piece> = before.iter().map(|line| piece(line)).collect();
        assert!(!Joins::new(&after).starts(&pieces).contains(&true));
    }

    #[test]
    fn rejoining_edited_headers_stays_linear_in_their_count() {
        use crate::engine::tests::{LINEAR_GROWTH_CAP, assert_bounded, cpu_ms, ratio};

        let none = changed(&[], &[]);
        let headers = |added: &'static str, body: &'static str| {
            move |count: usize| {
                (
                    format!("{}f();\n", "if (a)\n".repeat(count)),
                    format!("{}{body}\n", added.repeat(count)),
                )
            }
        };

        let (before, after) = headers("if (b) ", "f();")(4);
        assert_eq!(rewritten(&before, &after), changed(&[1], &[]));
        assert_eq!(
            rewritten(&before, &after.replace('b', "a")),
            none,
            "a body joined onto its headers stays unchanged"
        );

        let (before, after) = headers("if (a) ", "g();")(4);
        assert_eq!(rewritten(&before, &after), changed(&[1], &[]));

        for (what, added, body) in [
            ("edited headers", "if (b) ", "f();"),
            ("headers the added line starts with", "if (a) ", "g();"),
            (
                "headers beside a header with another comment",
                "if (a) ",
                "g();\nif (a) /* x */",
            ),
        ] {
            assert_bounded(
                what,
                &[400, 1_600],
                5,
                ratio(LINEAR_GROWTH_CAP),
                headers(added, body),
                |(before, after)| {
                    cpu_ms(|| {
                        rewritten(before, after);
                    })
                },
            );
        }
    }

    #[test]
    fn the_edit_limit_falls_back_to_each_block() {
        let before: Vec<usize> = (0..2_100).collect();
        let after: Vec<usize> = (5_000..7_100).collect();
        assert!(shortest_edit(&before, &after).is_none());
        assert!(shortest_edit(&before[..900], &after[..900]).is_some());
    }

    fn searches(before: &str, after: &str) -> (Changed, usize) {
        SEARCHES.set(0);
        let changed = rewritten(before, after);
        (changed, SEARCHES.get())
    }

    #[test]
    fn a_failed_search_over_the_whole_file_is_not_repeated() {
        let replaced = Changed {
            lines: (1..=12_000).collect(),
            deleted_after: BTreeSet::new(),
        };
        let interleaved = |call: &str| format!("x();\n{call}();\n").repeat(6_000);
        assert_eq!(
            searches(&interleaved("old"), &interleaved("newer")),
            (replaced.clone(), 1)
        );

        let old = "old();\n".repeat(12_000);
        assert_eq!(searches(&old, &"newer();\n".repeat(12_000)), (replaced, 0));
        assert_eq!(searches(&old, "newer();\n"), (changed(&[1], &[]), 0));
    }

    #[test]
    fn edits_beyond_the_limit_are_known_without_a_search() {
        let before: Vec<usize> = (0..2_001).collect();
        assert!(shortest_edit(&before, &[]).is_none());
        assert_eq!(shortest_edit(&before[..2_000], &[]), Some(Vec::new()));

        let after: Vec<usize> = (5_000..6_001).collect();
        assert!(shortest_edit(&before[..1_000], &after).is_none());
        assert_eq!(
            shortest_edit(&before[..1_000], &after[..1_000]),
            Some(Vec::new())
        );
    }

    #[test]
    fn repeated_join_keys_pair_the_last_added_line() {
        let before = "  if (a) {\n    x();\n  }\n";
        assert_eq!(
            rewritten(before, "  if (a) x();\n  if (a)  x();\n"),
            changed(&[1], &[])
        );
    }

    #[test]
    fn typechange_sections_merge_under_one_header() {
        let text = "diff --git a/a.ts b/a.ts\nold\ndiff --git a/a.ts b/a.ts\nnew\ndiff --git a/b.ts b/b.ts\n";
        let sections = diff_sections(text);
        assert_eq!(sections.len(), 2);
        assert_eq!(sections[0].len(), 4);
    }
}
