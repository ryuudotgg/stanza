use std::collections::{HashMap, HashSet};

use oxc_ast::ast::{BlockStatement, Statement};
use oxc_span::GetSpan;
use oxc_syntax::identifier::is_identifier_part;

use crate::engine::doc::{columns, comment_index, line_at, next_token};
use crate::engine::edits::OffsetEdit;
use crate::engine::model::{CommentKind, Doc};
use crate::languages::Layout;
use crate::text::{is_js_whitespace, js_trim};

#[derive(Clone, Copy)]
pub struct Controlled<'a> {
    pub block: &'a BlockStatement<'a>,
    pub header: usize,
    pub owner: usize,
}

pub struct BracePass {
    pub edits: Vec<OffsetEdit>,
    pub starts: Vec<usize>,
}

pub struct Owned<'a> {
    pub block: &'a BlockStatement<'a>,
    pub owner: usize,
}

#[derive(Debug, PartialEq, Eq)]
pub enum Hold {
    Count(usize),
    Statement,
    Continues { after: usize },
    Comment { start: usize },
    Closing { start: usize },
    Else,
    Fuse,
}

pub struct Opening {
    pub start: usize,
    pub open: usize,
    pub close: usize,
    pub inner: Option<(usize, usize)>,
}

pub enum Verdict {
    Directive,
    Inapplicable(String),
    Applies { held_by: Option<String> },
}

fn start<T: GetSpan>(node: &T) -> usize {
    node.span().start as usize
}

fn end<T: GetSpan>(node: &T) -> usize {
    node.span().end as usize
}

fn byte(doc: &Doc, offset: usize) -> Option<u8> {
    doc.text.as_bytes().get(offset).copied()
}

fn line_end(doc: &Doc, line: usize) -> usize {
    doc.line_starts[line - 1] + doc.lines[line - 1].len()
}

fn removable(statement: &Statement) -> bool {
    matches!(
        statement,
        Statement::ExpressionStatement(_)
            | Statement::ReturnStatement(_)
            | Statement::ThrowStatement(_)
            | Statement::ContinueStatement(_)
            | Statement::BreakStatement(_)
            | Statement::IfStatement(_)
            | Statement::ForStatement(_)
            | Statement::ForInStatement(_)
            | Statement::ForOfStatement(_)
            | Statement::WhileStatement(_)
            | Statement::DoWhileStatement(_)
    )
}

fn ends_with_open_if(node: &Statement, removed: &HashSet<usize>) -> bool {
    let mut current = node;
    loop {
        current = match current {
            Statement::IfStatement(it) => match &it.alternate {
                None => return true,
                Some(alternate) => alternate,
            },
            Statement::ForStatement(it) => &it.body,
            Statement::ForInStatement(it) => &it.body,
            Statement::ForOfStatement(it) => &it.body,
            Statement::WhileStatement(it) => &it.body,
            Statement::DoWhileStatement(it) => &it.body,
            Statement::LabeledStatement(it) => &it.body,
            Statement::WithStatement(it) => &it.body,
            Statement::BlockStatement(it) if removed.contains(&start(&**it)) => &it.body[0],
            _ => return false,
        };
    }
}

fn opening_edit(doc: &Doc, offset: usize) -> OffsetEdit {
    let line = line_at(doc, offset);
    let start = doc.line_starts[line - 1];
    let end = line_end(doc, line);
    if js_trim(doc.lines[line - 1]) == "{" {
        return OffsetEdit {
            start,
            end: end + 1,
        };
    }

    if js_trim(&doc.text[offset + 1..end]).is_empty() {
        let before = &doc.text[start..offset];
        let horizontal = before.len()
            - before
                .trim_end_matches(|char| is_js_whitespace(char) && char != '\r' && char != '\n')
                .len();

        return OffsetEdit {
            start: offset - horizontal,
            end: offset + 1,
        };
    }

    let spaced = byte(doc, offset + 1) == Some(b' ');
    OffsetEdit {
        start: offset,
        end: offset + if spaced { 2 } else { 1 },
    }
}

fn join_edit(doc: &Doc, controlled: &Controlled) -> Option<OffsetEdit> {
    let block = controlled.block;
    let inner = start(&block.body[0]);
    let line = line_at(doc, start(block));
    let end = line_end(doc, line);
    if line_at(doc, inner) == line || !js_trim(&doc.text[start(block) + 1..end]).is_empty() {
        return None;
    }

    let cut = doc.text[..start(block)]
        .trim_end_matches(is_js_whitespace)
        .len();
    let comment = comment_index(doc, cut)
        .checked_sub(1)
        .and_then(|index| doc.comments.get(index));

    if comment.is_some_and(|comment| comment.end == cut && comment.kind == CommentKind::Line) {
        return None;
    }

    if line_at(doc, controlled.header) != line_at(doc, cut) {
        return None;
    }

    if byte(doc, cut) == Some(b' ') {
        return Some(OffsetEdit {
            start: cut + 1,
            end: inner,
        });
    }

    (byte(doc, inner - 1) == Some(b' ')).then_some(OffsetEdit {
        start: cut,
        end: inner - 1,
    })
}

fn add_part<'t>(parts: &mut Vec<&'t str>, broken: &mut bool, part: &'t str) -> bool {
    let newline = part.find('\n');
    let refused = if *broken {
        !js_trim(part).is_empty()
    } else {
        newline.is_some_and(|newline| !js_trim(&part[newline..]).is_empty())
    };

    if refused {
        return false;
    }

    *broken |= newline.is_some();
    parts.push(part);
    true
}

fn slice(text: &str, from: usize, to: usize) -> &str {
    let to = to.min(text.len());
    if from >= to { "" } else { &text[from..to] }
}

fn joined_line(
    doc: &Doc,
    edits: &[OffsetEdit],
    ordered: &[usize],
    from: usize,
    to: usize,
) -> Option<String> {
    let mut low = 0;
    let mut high = ordered.len();
    while low < high {
        let middle = (low + high) / 2;
        if edits[ordered[middle]].end <= from {
            low = middle + 1;
        } else {
            high = middle;
        }
    }

    let mut parts = Vec::new();
    let mut broken = false;

    let mut cursor = from;
    for &index in &ordered[low..] {
        let edit = edits[index];
        if edit.start >= to {
            break;
        }

        if !add_part(&mut parts, &mut broken, slice(doc.text, cursor, edit.start)) {
            return None;
        }

        cursor = to.min(cursor.max(edit.end));
    }

    if !add_part(&mut parts, &mut broken, slice(doc.text, cursor, to)) {
        return None;
    }

    let line = parts.concat();
    Some(line.trim_end_matches(is_js_whitespace).to_owned())
}

fn closing_edit(doc: &Doc, offset: usize) -> OffsetEdit {
    let line = line_at(doc, offset);
    let start = doc.line_starts[line - 1];
    let end = line_end(doc, line);
    if js_trim(doc.lines[line - 1]) == "}" {
        let final_line = end == doc.text.len();
        return OffsetEdit {
            start,
            end: if final_line { end } else { end + 1 },
        };
    }

    let word =
        byte(doc, offset + 2).is_some_and(|next| next.is_ascii_alphanumeric() || next == b'_');
    if byte(doc, offset + 1) == Some(b' ') && word {
        return OffsetEdit {
            start: offset,
            end: offset + 2,
        };
    }

    let spaced = offset > 0 && byte(doc, offset - 1) == Some(b' ');
    OffsetEdit {
        start: offset - usize::from(spaced),
        end: offset + 1,
    }
}

fn continuation(doc: &Doc, inner: &Statement, block: &BlockStatement) -> Option<usize> {
    if doc.text[start(inner)..end(inner)].ends_with(';') {
        return None;
    }

    let after = next_token(doc, end(block));
    let separated = doc.text[end(block)..after].contains(['\r', '\n']);
    let starts_expression = matches!(
        byte(doc, after),
        Some(b'(' | b'[' | b'`' | b'+' | b'-' | b'/' | b'.' | b'<' | b';')
    );

    let safe = after >= doc.text.len() || (separated && !starts_expression);
    (!safe).then_some(after)
}

fn identifier_part(char: char) -> bool {
    char == '\\' || is_identifier_part(char)
}

fn fuses_identifiers(doc: &Doc, edit: OffsetEdit) -> bool {
    let before = doc.text[..edit.start].chars().next_back();
    let after = doc
        .text
        .get(edit.end..)
        .and_then(|rest| rest.chars().next());
    before.is_some_and(identifier_part) && after.is_some_and(identifier_part)
}

fn closing_comment(doc: &Doc, block: &BlockStatement) -> Option<usize> {
    let comment = doc.comments.get(comment_index(doc, end(block)))?;
    let line = line_at(doc, end(block) - 1);
    let closing = js_trim(&doc.text[end(block)..comment.start]).is_empty()
        && line_at(doc, comment.start) == line
        && js_trim(&doc.text[doc.line_starts[line - 1]..end(block) - 1]).is_empty();

    closing.then_some(comment.start)
}

fn else_follows(doc: &Doc, block: &BlockStatement) -> bool {
    let next = next_token(doc, end(block));
    let rest = &doc.text.as_bytes()[next..];
    rest.starts_with(b"else")
        && !rest
            .get(4)
            .is_some_and(|byte| byte.is_ascii_alphanumeric() || *byte == b'_')
}

pub fn brace_hold(
    doc: &Doc,
    block: &BlockStatement,
    removed: &HashSet<usize>,
    chained: bool,
) -> Option<Hold> {
    let [inner] = block.body.as_slice() else {
        return Some(Hold::Count(block.body.len()));
    };

    if !removable(inner) {
        return Some(Hold::Statement);
    }

    if let Some(after) = (!chained)
        .then(|| continuation(doc, inner, block))
        .flatten()
    {
        return Some(Hold::Continues { after });
    }

    let comments = comment_index(doc, start(block) + 1)..comment_index(doc, end(block));
    if let Some(comment) = doc.comments[comments]
        .iter()
        .find(|comment| comment.start < start(inner) || comment.end > end(inner))
    {
        return Some(Hold::Comment {
            start: comment.start,
        });
    }

    if let Some(start) = closing_comment(doc, block) {
        return Some(Hold::Closing { start });
    }

    if else_follows(doc, block) && ends_with_open_if(inner, removed) {
        return Some(Hold::Else);
    }

    let opening = opening_edit(doc, start(block));
    let closing = closing_edit(doc, end(block) - 1);
    if fuses_identifiers(doc, opening) || fuses_identifiers(doc, closing) {
        return Some(Hold::Fuse);
    }

    None
}

fn trailing_body<'s, 'a>(statement: &'s Statement<'a>) -> Option<&'s Statement<'a>> {
    match statement {
        Statement::IfStatement(it) if it.alternate.is_none() => Some(&it.consequent),
        Statement::ForStatement(it) => Some(&it.body),
        Statement::ForInStatement(it) => Some(&it.body),
        Statement::ForOfStatement(it) => Some(&it.body),
        Statement::WhileStatement(it) => Some(&it.body),
        _ => None,
    }
}

struct Chains<'d, 'b> {
    doc: &'d Doc<'d>,
    by_closing: HashMap<usize, &'b BlockStatement<'b>>,
    known: HashMap<usize, bool>,
    code_starts: HashMap<usize, usize>,
}

impl Chains<'_, '_> {
    fn closers_end_at_fixed_brace(&mut self, from: usize) -> bool {
        let text = self.doc.text;
        let mut visited = Vec::new();

        let mut at = from;
        let answer = loop {
            at = text.len() - text[at..].trim_start_matches(is_js_whitespace).len();
            if let Some(&answer) = self.known.get(&at) {
                break answer;
            }

            match text.as_bytes().get(at) {
                None => break true,
                Some(b'}') if !self.by_closing.contains_key(&at) => break true,
                Some(b'}') => {
                    visited.push(at);
                    at += 1;
                }
                Some(_) => break false,
            }
        };

        for offset in visited {
            self.known.insert(offset, answer);
        }

        answer
    }

    fn code_start(&mut self, offset: usize) -> usize {
        let line = self.doc.line_starts[line_at(self.doc, offset) - 1];
        let text = self.doc.text;
        *self.code_starts.entry(line).or_insert_with(|| {
            text[line..]
                .char_indices()
                .find(|&(_, char)| {
                    char == '\n' || !(is_js_whitespace(char) || char == '{' || char == '}')
                })
                .filter(|&(_, char)| char != '\n')
                .map_or(usize::MAX, |(index, _)| line + index)
        })
    }
}

struct Removal<'d> {
    doc: &'d Doc<'d>,
    edits: Vec<OffsetEdit>,
    starts: Vec<usize>,
    removed: HashSet<usize>,
    openings: HashMap<usize, usize>,
}

impl Removal<'_> {
    fn remove(&mut self, block: &BlockStatement) {
        let opening = opening_edit(self.doc, start(block));
        self.removed.insert(start(block));
        self.openings.insert(start(block), self.edits.len());
        self.edits.push(opening);
        self.edits.push(closing_edit(self.doc, end(block) - 1));
        self.starts.push(start(block));
    }
}

fn closes_chain(
    doc: &Doc,
    block: &BlockStatement,
    removed: &HashSet<usize>,
    outer_goes: bool,
    chains: &mut Chains,
) -> bool {
    let inner = match block.body.as_slice() {
        [only] => trailing_body(only),
        _ => None,
    };

    let Some(Statement::BlockStatement(inner)) = inner else {
        return false;
    };

    if !removed.contains(&start(&**inner)) {
        return false;
    }

    let inner_end = end(&**inner);
    slice(doc.text, inner_end, end(block) - 1) == " "
        && chains.code_start(inner_end - 1) < inner_end - 1
        && doc.text.as_bytes().get(end(block)..end(block) + 2) == Some(b" }")
        && !outer_goes
        && chains.closers_end_at_fixed_brace(end(block))
        && brace_hold(doc, block, removed, true).is_none()
}

fn collapse(blocks: &[&Controlled], removal: &mut Removal) {
    let standard = removal.removed.clone();
    let mut chains = Chains {
        doc: removal.doc,
        by_closing: blocks
            .iter()
            .map(|controlled| (end(controlled.block) - 1, controlled.block))
            .collect(),
        known: HashMap::new(),
        code_starts: HashMap::new(),
    };

    for controlled in blocks {
        let block = controlled.block;
        let outer_goes = chains
            .by_closing
            .get(&(end(block) + 1))
            .is_some_and(|outer| standard.contains(&start(&**outer)));

        if !standard.contains(&start(block))
            && closes_chain(
                removal.doc,
                block,
                &removal.removed,
                outer_goes,
                &mut chains,
            )
        {
            removal.remove(block);
        }
    }
}

fn join_bodies(doc: &Doc, blocks: &[&Controlled], removal: &mut Removal, layout: Layout) {
    let mut ordered: Vec<usize> = (0..removal.edits.len()).collect();
    ordered.sort_by_key(|&index| removal.edits[index].start);

    let mut removed: Vec<&&Controlled> = blocks
        .iter()
        .filter(|controlled| removal.removed.contains(&start(controlled.block)))
        .collect();

    removed.sort_by_key(|controlled| std::cmp::Reverse(start(controlled.block)));
    for controlled in removed {
        let Some(join) = join_edit(doc, controlled) else {
            continue;
        };

        let opening = removal.openings[&start(controlled.block)];
        let plain = removal.edits[opening];
        removal.edits[opening] = join;

        let from = doc.line_starts[line_at(doc, join.start.saturating_sub(1)) - 1];
        let last = line_at(doc, end(&controlled.block.body[0]) - 1);
        let to = line_end(doc, last);
        let fits = joined_line(doc, &removal.edits, &ordered, from, to)
            .is_some_and(|line| columns(&line, layout.tab) <= layout.columns);

        if !fits {
            removal.edits[opening] = plain;
        }
    }
}

pub fn brace_edits(
    doc: &Doc,
    blocks: &[Controlled],
    collapse_chains: bool,
    layout: Layout,
) -> BracePass {
    let mut removal = Removal {
        doc,
        edits: Vec::new(),
        starts: Vec::new(),
        removed: HashSet::new(),
        openings: HashMap::new(),
    };

    let blocks: Vec<&Controlled> = blocks.iter().collect();
    for controlled in &blocks {
        if brace_hold(doc, controlled.block, &removal.removed, false).is_none() {
            removal.remove(controlled.block);
        }
    }

    if collapse_chains {
        collapse(&blocks, &mut removal);
    }

    join_bodies(doc, &blocks, &mut removal, layout);
    BracePass {
        edits: removal.edits,
        starts: removal.starts,
    }
}

fn estree_type(statement: &Statement) -> &'static str {
    match statement {
        Statement::ClassDeclaration(_) => "ClassDeclaration",
        Statement::FunctionDeclaration(function) if function.body.is_none() => "TSDeclareFunction",
        Statement::FunctionDeclaration(_) => "FunctionDeclaration",
        Statement::VariableDeclaration(_) => "VariableDeclaration",
        Statement::TSTypeAliasDeclaration(_) => "TSTypeAliasDeclaration",
        Statement::TSInterfaceDeclaration(_) => "TSInterfaceDeclaration",
        Statement::TSEnumDeclaration(_) => "TSEnumDeclaration",
        Statement::TSImportEqualsDeclaration(_) => "TSImportEqualsDeclaration",
        _ => "Statement",
    }
}

fn spaced_lowercase(name: &str) -> String {
    let mut spaced = String::with_capacity(name.len() + 4);
    let mut lower_before = false;
    for char in name.chars() {
        if lower_before && char.is_ascii_uppercase() {
            spaced.push(' ');
        }

        lower_before = char.is_ascii_lowercase();
        spaced.push(char.to_ascii_lowercase());
    }

    spaced
}

fn statement_name(doc: &Doc, inner: &Statement) -> String {
    match inner {
        Statement::BlockStatement(_) => return "a nested block".to_owned(),
        Statement::EmptyStatement(_) => return "an empty statement".to_owned(),
        Statement::LabeledStatement(_) => return "a labeled statement".to_owned(),
        _ => {}
    }

    let text = &doc.text[start(inner)..end(inner)];
    let word = text
        .find(|char: char| !(char.is_ascii_alphanumeric() || char == '_' || char == '$'))
        .map_or(text, |at| &text[..at]);

    if word.is_empty() {
        format!("a {}", spaced_lowercase(estree_type(inner)))
    } else {
        format!("a `{word}` statement")
    }
}

pub fn hold_reason(
    doc: &Doc,
    block: &BlockStatement,
    hold: &Hold,
    at: &dyn Fn(usize) -> usize,
) -> String {
    match *hold {
        Hold::Count(0) => "the body is empty".to_owned(),
        Hold::Count(count) => format!("the body holds {count} statements"),
        Hold::Statement => format!(
            "the body is {}, and only an expression, `return`, `throw`, `break`, `continue`, `if` or loop stands without braces",
            statement_name(doc, &block.body[0])
        ),
        Hold::Continues { after } => {
            let inner = &block.body[0];
            let line = line_at(doc, after);
            let place = if line == line_at(doc, end(block) - 1) {
                "the same line".to_owned()
            } else {
                format!("line {}", at(after))
            };

            let first = doc.text[start(inner)..end(inner)]
                .split('\n')
                .next()
                .unwrap_or_default();

            format!(
                "`{}` has no semicolon, so without braces it could continue onto `{}` on {place}",
                js_trim(first),
                js_trim(&doc.text[after..line_end(doc, line)])
            )
        }
        Hold::Comment { start } => format!(
            "a comment on line {} sits inside the braces but outside the statement",
            at(start)
        ),
        Hold::Closing { start } => format!(
            "a comment after the closing brace on line {} would move onto the next line without it",
            at(start)
        ),
        Hold::Else => format!(
            "without braces, the `else` on line {} would attach to the `if` inside them",
            at(next_token(doc, end(block)))
        ),
        Hold::Fuse => "removing the braces would run two words together".to_owned(),
    }
}

pub fn openings(doc: &Doc, owned: &[Owned], line: usize) -> Vec<Opening> {
    let opening: Vec<&Owned> = owned
        .iter()
        .filter(|owned| line_at(doc, start(owned.block)) == line)
        .collect();

    let selected = if opening.is_empty() {
        owned
            .iter()
            .filter(|owned| line_at(doc, owned.owner) == line)
            .collect()
    } else {
        opening
    };

    selected
        .into_iter()
        .map(|owned| {
            let block = owned.block;
            let inner = match block.body.as_slice() {
                [inner] => Some((line_at(doc, start(inner)), line_at(doc, end(inner) - 1))),
                _ => None,
            };

            Opening {
                start: start(block),
                open: line_at(doc, start(block)),
                close: line_at(doc, end(block) - 1),
                inner,
            }
        })
        .collect()
}

pub fn verdict(
    doc: &Doc,
    owned: &[Owned],
    blocks: &[Controlled],
    offset: usize,
    at: &dyn Fn(usize) -> usize,
) -> Verdict {
    let Some(survivor) = owned.iter().find(|owned| start(owned.block) == offset) else {
        return Verdict::Directive;
    };

    if !blocks
        .iter()
        .any(|controlled| std::ptr::eq(controlled.block, survivor.block))
    {
        return Verdict::Directive;
    }

    let block = survivor.block;
    match brace_hold(doc, block, &HashSet::new(), false) {
        Some(hold @ Hold::Count(_)) => Verdict::Inapplicable(hold_reason(doc, block, &hold, at)),
        Some(hold) => Verdict::Applies {
            held_by: Some(hold_reason(doc, block, &hold, at)),
        },
        None => Verdict::Applies { held_by: None },
    }
}
