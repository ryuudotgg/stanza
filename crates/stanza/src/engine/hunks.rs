use std::collections::{BTreeSet, HashMap};

use super::doc::line_at;
use super::edits::{OffsetEdit, OffsetMap};
use super::model::{Doc, LineEdits};
use crate::text::{is_js_whitespace, js_trim, utf16_len};

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Changed {
    pub lines: BTreeSet<usize>,
    pub deleted_after: BTreeSet<usize>,
}

pub fn touching(changed: Option<&Changed>) -> impl Fn(usize, usize) -> bool + '_ {
    move |first, last| {
        changed.is_none_or(|changed| {
            changed
                .lines
                .range(first..)
                .next()
                .is_some_and(|&line| line <= last)
                || changed
                    .deleted_after
                    .range(first..)
                    .next()
                    .is_some_and(|&line| line < last)
        })
    }
}

fn is_word(char: char) -> bool {
    char.is_ascii_alphanumeric() || char == '_'
}

fn is_line_terminator(char: char) -> bool {
    matches!(char, '\n' | '\r' | '\u{2028}' | '\u{2029}')
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
    at == 0 || !is_word(chars[at - 1])
}

fn bounded_after(chars: &[char], at: usize) -> bool {
    chars.get(at).is_none_or(|&char| !is_word(char))
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
        let closes = (next == Some('*'))
            .then(|| {
                (index + 2..chars.len().saturating_sub(1)).find(|&at| starts_at(&chars, at, "*/"))
            })
            .flatten();

        if let (Some(closes), '/') = (closes, char) {
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

fn closes_line(chars: &[char], from: usize) -> bool {
    let after_space = skip_space(chars, from);
    if after_space == chars.len()
        || starts_at(chars, after_space, "//")
        || starts_at(chars, after_space, "/*")
    {
        return true;
    }

    let last = chars.iter().rposition(|&char| !is_js_whitespace(char));
    last.is_some_and(|last| {
        last >= from
            && chars[last] == '}'
            && !chars[from..last]
                .iter()
                .any(|&char| is_line_terminator(char))
    })
}

fn open_brace_end(chars: &[char], at: usize) -> Option<(usize, usize)> {
    let head = opening_head(chars, at)?;
    let brace = skip_space(chars, head);
    if chars.get(brace) != Some(&'{') || !closes_line(chars, brace + 1) {
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

    let mut opened = Vec::with_capacity(chars.len());
    let mut at = 0;
    while at < chars.len() {
        match open_brace_end(&chars, at) {
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
                closed.push(opened[at]);
                at += 1;
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

fn join_key(indent: &str, code: &str, text: &str) -> String {
    format!("{indent}{code}\0{text}")
}

const EDIT_LIMIT: i64 = 2000;

fn shortest_edit(before: &[usize], after: &[usize]) -> Option<Vec<(usize, usize)>> {
    let (width, height) = (before.len() as i64, after.len() as i64);
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

    let mut x = width as f64;
    let mut y = height as f64;
    for edits in (0..trace.len()).rev() {
        let diagonal = x - y;
        let rounds = edits as f64;

        let mut start_x = 0.0;
        let mut start_y = 0.0;
        if edits > 0 {
            let previous = &trace[edits - 1];
            let read = |index: f64| {
                let slot = index + rounds - 1.0;
                if slot >= 0.0 && slot.fract() == 0.0 && (slot as usize) < previous.len() {
                    f64::from(previous[slot as usize])
                } else {
                    f64::NAN
                }
            };

            let from = if diagonal == -rounds
                || (diagonal != rounds && read(diagonal - 1.0) < read(diagonal + 1.0))
            {
                diagonal + 1.0
            } else {
                diagonal - 1.0
            };

            start_x = read(from);
            start_y = start_x - from;
        }

        let snake_x = if edits > 0 && start_x - start_y == diagonal - 1.0 {
            start_x + 1.0
        } else {
            start_x
        };

        let snake_y = if edits > 0 && start_x - start_y == diagonal + 1.0 {
            start_y + 1.0
        } else {
            start_y
        };

        while x > snake_x && y > snake_y {
            x -= 1.0;
            y -= 1.0;
            pairs.push((x as usize, y as usize));
        }

        x = start_x;
        y = start_y;
    }

    pairs.reverse();
    pairs
}

fn matches(before: &[usize], after: &[usize]) -> Option<Vec<(usize, usize)>> {
    let mut head = 0;
    while head < before.len() && head < after.len() && before[head] == after[head] {
        head += 1;
    }

    let mut tail = 0;
    while tail < before.len() - head
        && tail < after.len() - head
        && before[before.len() - 1 - tail] == after[after.len() - 1 - tail]
    {
        tail += 1;
    }

    let middle = shortest_edit(
        &before[head..before.len() - tail],
        &after[head..after.len() - tail],
    )?;

    let mut pairs: Vec<(usize, usize)> = (0..head).map(|index| (index, index)).collect();
    pairs.extend(middle.into_iter().map(|(x, y)| (x + head, y + head)));
    pairs.extend((0..tail).map(|index| (before.len() - tail + index, after.len() - tail + index)));
    Some(pairs)
}

#[derive(Clone)]
struct After {
    text: String,
    line: usize,
}

#[derive(Default)]
struct Lines {
    before: Vec<String>,
    after: Vec<After>,
    at: usize,
}

fn hunk_header(row: &str) -> Option<(Option<i64>, i64, Option<i64>)> {
    fn digits(text: &str) -> (&str, &str) {
        let end = text
            .find(|char: char| !char.is_ascii_digit())
            .unwrap_or(text.len());
        (&text[..end], &text[end..])
    }

    fn count(text: &str) -> Option<(Option<i64>, &str)> {
        let Some(rest) = text.strip_prefix(',') else {
            return Some((None, text));
        };

        let (number, rest) = digits(rest);
        (!number.is_empty()).then(|| (Some(number.parse().unwrap_or(i64::MAX)), rest))
    }

    let rest = row.strip_prefix("@@ -")?;
    let (start, rest) = digits(rest);
    if start.is_empty() {
        return None;
    }

    let (before, rest) = count(rest)?;
    let rest = rest.strip_prefix(" +")?;
    let (first, rest) = digits(rest);
    if first.is_empty() {
        return None;
    }

    let (after, rest) = count(rest)?;
    rest.starts_with(" @@")
        .then(|| (before, first.parse().unwrap_or(i64::MAX), after))
}

fn sides(section: &[&str]) -> (Lines, Vec<Lines>) {
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
            file.before.push(text.to_owned());
            file.after.push(After {
                text: text.to_owned(),
                line: line as usize,
            });

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
            let after = After {
                text: text.to_owned(),
                line: line as usize,
            };

            file.after.push(after.clone());
            block.after.push(after);
        } else {
            before_left -= 1;
            if brace_only(text) {
                continue;
            }

            file.before.push(text.to_owned());
            block.before.push(text.to_owned());
        }
    }

    file.at = file.after.first().map_or(1, |after| after.line) - 1;
    (file, blocks)
}

struct Group {
    first: usize,
    last: usize,
    text: String,
}

fn rejoin(before: &[String], from: usize, to: usize, after: &[After]) -> Vec<Group> {
    let mut joins: HashMap<String, &str> = HashMap::new();

    let mut longest = 0;
    for After { text, .. } in after {
        let piece = piece(text);
        joins.insert(join_key(&piece.indent, &piece.code, &piece.text), text);
        longest = longest.max(utf16_len(&piece.code));
    }

    let mut groups = Vec::new();
    let mut index = from;
    while index < to {
        let first = piece(&before[index]);

        let mut last = index;
        let mut code = first.code.clone();
        let mut text = first.text.clone();
        let mut found = None;
        if first.opens && !joins.contains_key(&join_key(&first.indent, &code, &text)) {
            let mut next = index + 1;
            while next < to && utf16_len(&code) < longest {
                let part = piece(&before[next]);
                if part.code.is_empty() && part.text.is_empty() {
                    next += 1;
                    continue;
                }

                if !part.code.is_empty() {
                    code = format!("{code} {}", part.code);
                }

                text.push_str(&part.text);
                found = joins.get(&join_key(&first.indent, &code, &text)).copied();
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
            text: found.map_or_else(|| before[index].clone(), str::to_owned),
        });

        index = last + 1;
    }

    groups
}

struct Pair {
    first: usize,
    last: usize,
    text: String,
    y: usize,
}

struct Ids(HashMap<String, usize>);

impl Ids {
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

fn classify(lines: &Lines, changed: &mut Changed) -> bool {
    let Lines { before, after, at } = lines;
    let mut at = *at;
    let mut ids = Ids(HashMap::new());
    let before_ids: Vec<usize> = before.iter().map(|text| ids.id(text)).collect();
    let after_ids: Vec<usize> = after.iter().map(|line| ids.id(&line.text)).collect();
    let Some(pairs) = matches(&before_ids, &after_ids) else {
        return false;
    };

    let mut paired = Vec::new();

    let mut x = 0;
    let mut y = 0;
    for (pair_x, pair_y) in pairs.into_iter().chain([(before.len(), after.len())]) {
        let groups = if x < pair_x && y < pair_y {
            rejoin(before, x, pair_x, &after[y..pair_y])
        } else {
            Vec::new()
        };

        if groups.len() < pair_x - x {
            let group_ids: Vec<usize> = groups.iter().map(|group| ids.id(&group.text)).collect();
            let slice_ids: Vec<usize> = after[y..pair_y]
                .iter()
                .map(|line| ids.id(&line.text))
                .collect();

            for (group_x, group_y) in matches(&group_ids, &slice_ids).unwrap_or_default() {
                let group = &groups[group_x];
                paired.push(Pair {
                    first: group.first,
                    last: group.last,
                    text: group.text.clone(),
                    y: y + group_y,
                });
            }
        }

        if pair_x < before.len() {
            paired.push(Pair {
                first: pair_x,
                last: pair_x,
                text: before[pair_x].clone(),
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
        text: String::new(),
        y: after.len(),
    });

    for pair in &paired {
        if x < pair.first {
            changed.deleted_after.insert(at);
        }

        while y < pair.y {
            at = after[y].line;
            changed.lines.insert(at);
            y += 1;
        }

        if pair.first == before.len() {
            break;
        }

        let now = &after[pair.y];
        if pair.text != now.text && braces(&now.text) >= braces(&pair.text) {
            changed.lines.insert(now.line);
        }

        at = now.line;
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
            if classify(block, &mut changed) {
                continue;
            }

            if !block.before.is_empty() {
                changed.deleted_after.insert(block.at);
            }

            changed
                .lines
                .extend(block.after.iter().map(|after| after.line));
        }
    }

    let lines = &changed.lines;
    changed
        .deleted_after
        .retain(|line| !lines.contains(line) && !lines.contains(&(line + 1)));

    changed
}

fn line_before(next: &Doc, offset: usize) -> usize {
    let line = line_at(next, offset.min(next.text.len()));
    if next.line_starts[line - 1] == offset {
        line - 1
    } else {
        line
    }
}

pub struct Before {
    pub line_starts: Vec<usize>,
    pub length: usize,
}

impl Before {
    pub fn of(doc: &Doc) -> Self {
        Before {
            line_starts: doc.line_starts.clone(),
            length: doc.text.len(),
        }
    }

    fn line_end(&self, line: usize) -> usize {
        self.line_starts.get(line).copied().unwrap_or(self.length)
    }
}

pub fn after_offsets(
    before: &Before,
    changed: Option<Changed>,
    edits: &[OffsetEdit],
    next: &Doc,
) -> Option<Changed> {
    let changed = changed?;
    let map = OffsetMap::new(edits);
    let mut moved = Changed::default();
    for &line in &changed.lines {
        let Some(&from) = line
            .checked_sub(1)
            .and_then(|index| before.line_starts.get(index))
        else {
            continue;
        };

        let start = map.forward(from);
        let end = map.forward(before.line_end(line));
        if start == end {
            moved.deleted_after.insert(line_before(next, start));
            continue;
        }

        let last = line_at(next, end.min(next.text.len()).saturating_sub(1));
        moved.lines.extend(line_at(next, start)..=last);
    }

    for &line in &changed.deleted_after {
        let offset = map.forward(before.line_end(line));
        moved.deleted_after.insert(line_before(next, offset));
    }

    Some(moved)
}

pub fn after_lines(doc: &Doc, changed: Option<Changed>, edits: &LineEdits) -> Option<Changed> {
    let changed = changed?;
    let mut moved = Changed::default();
    if changed.deleted_after.contains(&0) {
        moved.deleted_after.insert(0);
    }

    let mut output = 0;
    for line in 1..=doc.lines.len() {
        if !edits.delete_lines.contains(&line) {
            output += 1;
            if changed.lines.contains(&line) {
                moved.lines.insert(output);
            }
        } else if changed.lines.contains(&line) {
            moved.deleted_after.insert(output);
        }

        if changed.deleted_after.contains(&line) {
            moved.deleted_after.insert(output);
        }

        if edits.insert_after.contains(&line) {
            output += 1;
        }
    }

    Some(moved)
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

    #[test]
    fn the_edit_limit_falls_back_to_each_block() {
        let before: Vec<usize> = (0..2_100).collect();
        let after: Vec<usize> = (5_000..7_100).collect();
        assert!(shortest_edit(&before, &after).is_none());
        assert!(shortest_edit(&before[..900], &after[..900]).is_some());
    }

    #[test]
    fn typechange_sections_merge_under_one_header() {
        let text = "diff --git a/a.ts b/a.ts\nold\ndiff --git a/a.ts b/a.ts\nnew\ndiff --git a/b.ts b/b.ts\n";
        let sections = diff_sections(text);
        assert_eq!(sections.len(), 2);
        assert_eq!(sections[0].len(), 4);
    }

    #[test]
    fn touching_reads_lines_and_deletions_inside_the_range() {
        let lines = changed(&[4], &[7]);
        let touches = touching(Some(&lines));
        assert!(touches(4, 4));
        assert!(!touches(5, 7));
        assert!(touches(5, 8));
        assert!(touching(None)(1, 1));
    }
}
