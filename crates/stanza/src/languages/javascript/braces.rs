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
    pub openings: Vec<usize>,
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
    Closer,
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
            Statement::LabeledStatement(it) => &it.body,
            Statement::WithStatement(it) => &it.body,
            Statement::BlockStatement(it) if removed.contains(&start(&**it)) => &it.body[0],
            _ => return false,
        };
    }
}

fn blank(doc: &Doc, line: usize) -> bool {
    js_trim(doc.lines[line - 1]).is_empty()
}

fn last_blank_below(doc: &Doc, line: usize) -> usize {
    let mut last = line;
    while last < doc.lines.len() && blank(doc, last + 1) {
        last += 1;
    }

    last
}

fn first_blank_above(doc: &Doc, line: usize) -> usize {
    let mut first = line;
    while first > 1 && blank(doc, first - 1) {
        first -= 1;
    }

    first
}

fn opening_edit(doc: &Doc, offset: usize) -> OffsetEdit {
    let line = line_at(doc, offset);
    let start = doc.line_starts[line - 1];
    let end = line_end(doc, line);
    if js_trim(doc.lines[line - 1]) == "{" {
        return OffsetEdit::cut(start, doc.line_starts[last_blank_below(doc, line)]);
    }

    if js_trim(&doc.text[offset + 1..end]).is_empty() {
        let before = &doc.text[start..offset];
        let horizontal = before.len()
            - before
                .trim_end_matches(|char| is_js_whitespace(char) && char != '\r' && char != '\n')
                .len();

        let last = last_blank_below(doc, line);
        let end = if last == line {
            offset + 1
        } else {
            line_end(doc, last) - usize::from(doc.lines[last - 1].ends_with('\r'))
        };

        return OffsetEdit::cut(offset - horizontal, end);
    }

    let spaced = byte(doc, offset + 1) == Some(b' ');
    OffsetEdit::cut(offset, offset + if spaced { 2 } else { 1 })
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

    Some(OffsetEdit::replace(cut, inner, " "))
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

        if edit.start >= cursor {
            parts.push(edit.text);
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
    let end = line_end(doc, line);
    if js_trim(doc.lines[line - 1]) == "}" {
        let final_line = end == doc.text.len();
        let start = doc.line_starts[first_blank_above(doc, line) - 1];
        return OffsetEdit::cut(start, if final_line { end } else { end + 1 });
    }

    let word =
        byte(doc, offset + 2).is_some_and(|next| next.is_ascii_alphanumeric() || next == b'_');
    if byte(doc, offset + 1) == Some(b' ') && word {
        return OffsetEdit::cut(offset, offset + 2);
    }

    let spaced = offset > 0 && byte(doc, offset - 1) == Some(b' ');
    OffsetEdit::cut(offset - usize::from(spaced), offset + 1)
}

fn leads_line(doc: &Doc, offset: usize) -> bool {
    let start = doc.line_starts[line_at(doc, offset) - 1];
    js_trim(&doc.text[start..offset]).is_empty()
}

fn blanks_above_closing(doc: &Doc, offset: usize) -> Option<OffsetEdit> {
    let line = line_at(doc, offset);
    let start = doc.line_starts[line - 1];
    if !leads_line(doc, offset) || js_trim(doc.lines[line - 1]) == "}" {
        return None;
    }

    let first = first_blank_above(doc, line);
    (first < line).then(|| OffsetEdit::cut(doc.line_starts[first - 1], start))
}

fn continuation(doc: &Doc, inner: &Statement, block: &BlockStatement) -> Option<Hold> {
    if doc.text[start(inner)..end(inner)].ends_with(';') {
        return None;
    }

    let after = next_token(doc, end(block));
    let separated = doc.text[end(block)..after].contains(['\r', '\n']);
    if !separated && byte(doc, after) == Some(b'}') {
        // Cutting a leading closer with the closers after it leaves a stray line and orphans a trailing comment.
        return leads_line(doc, end(block) - 1).then_some(Hold::Closer);
    }

    let starts_expression = matches!(
        byte(doc, after),
        Some(b'(' | b'[' | b'`' | b'+' | b'-' | b'/' | b'.' | b'<' | b';')
    );

    let safe = after >= doc.text.len() || (separated && !starts_expression);
    (!safe).then_some(Hold::Continues { after })
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
    let closing = js_trim(&doc.text[end(block)..comment.start]).is_empty()
        && line_at(doc, comment.start) == line_at(doc, end(block) - 1)
        && leads_line(doc, end(block) - 1);

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

pub fn brace_hold(doc: &Doc, block: &BlockStatement, removed: &HashSet<usize>) -> Option<Hold> {
    let [inner] = block.body.as_slice() else {
        return Some(Hold::Count(block.body.len()));
    };

    if !removable(inner) {
        return Some(Hold::Statement);
    }

    if let Some(hold) = continuation(doc, inner, block) {
        return Some(hold);
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
        self.edits
            .extend(blanks_above_closing(self.doc, end(block) - 1));
        self.starts.push(start(block));
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

pub fn brace_edits(doc: &Doc, blocks: &[Controlled], layout: Layout) -> BracePass {
    let mut removal = Removal {
        doc,
        edits: Vec::new(),
        starts: Vec::new(),
        removed: HashSet::new(),
        openings: HashMap::new(),
    };

    let blocks: Vec<&Controlled> = blocks.iter().collect();
    for controlled in &blocks {
        if brace_hold(doc, controlled.block, &removal.removed).is_none() {
            removal.remove(controlled.block);
        }
    }

    join_bodies(doc, &blocks, &mut removal, layout);
    let openings = removal
        .starts
        .iter()
        .map(|start| removal.openings[start])
        .collect();

    BracePass {
        edits: removal.edits,
        starts: removal.starts,
        openings,
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
        Hold::Closer => format!(
            "the closing brace on line {} shares its line with the `}}` after it, which would be left behind without it",
            at(end(block) - 1)
        ),
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
    match brace_hold(doc, block, &HashSet::new()) {
        Some(hold @ Hold::Count(_)) => Verdict::Inapplicable(hold_reason(doc, block, &hold, at)),
        Some(hold) => Verdict::Applies {
            held_by: Some(hold_reason(doc, block, &hold, at)),
        },
        None => Verdict::Applies { held_by: None },
    }
}

#[cfg(test)]
mod tests {
    use oxc_allocator::Allocator;
    use oxc_ast::ast::Statement;

    use crate::engine::process_file;
    use crate::engine::types::Mode;
    use crate::languages::FALLBACK_LAYOUT;
    use crate::languages::javascript::parse::parse;

    fn unbraced(path: &str, text: &str) -> String {
        let result = process_file(path, text, Mode::Fix, false, FALLBACK_LAYOUT, None);
        assert!(!result.parse_error, "{text:?} parses");
        result.fixed.map_or_else(
            || text.to_owned(),
            |fixed| {
                assert_eq!(fixed.error, None, "{text:?} fix parses");
                fixed.text
            },
        )
    }

    #[test]
    fn removed_braces_take_their_edge_blanks() {
        let cases = [
            (
                "if (a) {\n  three();\n\n} else {\n  four();\n  five();\n}\n",
                "if (a) three();\nelse {\n  four();\n  five();\n}\n",
            ),
            (
                "do {\n  six();\n\n} while (c);\n",
                "do six();\nwhile (c);\n",
            ),
            (
                "function f() {\n  if (a) {\n    three();\n\n  }\n  next();\n  next();\n  next();\n}\n",
                "function f() {\n  if (a) three();\n  next();\n  next();\n  next();\n}\n",
            ),
            (
                "if (c) {\n\n  someVeryLongFunctionName(argumentNumberOne, argumentNumberTwo, argumentNumberThree);\n}\n",
                "if (c)\n  someVeryLongFunctionName(argumentNumberOne, argumentNumberTwo, argumentNumberThree);\n",
            ),
            (
                "before();\n\nif (a) {\n  b();\n\n}\n",
                "before();\n\nif (a) b();\n",
            ),
            ("if (a)\n{\n\n  b();\n\n}\n", "if (a) b();\n"),
            (
                "if (a) {\n\n  if (b) {\n\n    c();\n\n  }\n\n}\nnext();\n",
                "if (a) if (b) c();\nnext();\n",
            ),
            ("if (a) {\n  \n  b();\n  \n}\n", "if (a) b();\n"),
            (
                "function f() {\n  if (a) {\n    b();\n\n  } else {\n    c();\n    d();\n  }\n}\n",
                "function f() {\n  if (a) b();\n  else {\n    c();\n    d();\n  }\n}\n",
            ),
        ];

        for (source, expected) in cases {
            assert_eq!(unbraced("a.ts", source), expected, "{source:?}");

            let crlf = source.replace('\n', "\r\n");
            let expected = expected.replace('\n', "\r\n");
            assert_eq!(unbraced("a.ts", &crlf), expected, "{crlf:?}");
        }
    }

    #[test]
    fn tab_indented_bodies_join() {
        let wide = format!("if (a) {{\n\t{}();\n}}\n", "x".repeat(81));
        let cases = [
            (
                "if (a)\n{\n\tb();\n}\nif (a){\n\tc();\n}\n".to_owned(),
                "if (a) b();\nif (a) c();\n".to_owned(),
            ),
            (wide.clone(), wide.replace(" {", "").replace("\n}", "")),
            (
                "if (a) // why\n{\n\tb();\n}\n".to_owned(),
                "if (a) // why\n\tb();\n".to_owned(),
            ),
            (
                "if (a)\t{\n\tb();\n}\nelse\n{\n\tc();\n}\ndo\n{\n\td();\n} while (e);\n"
                    .to_owned(),
                "if (a) b();\nelse c();\ndo d();\nwhile (e);\n".to_owned(),
            ),
        ];

        for (source, expected) in cases {
            assert_eq!(unbraced("a.ts", &source), expected, "{source:?}");

            let crlf = source.replace('\n', "\r\n");
            let crlf_expected = expected.replace('\n', "\r\n");
            assert_eq!(unbraced("a.ts", &crlf), crlf_expected, "{crlf:?}");

            let unended = &source[..source.len() - 1];
            let unended_expected = &expected[..expected.len() - 1];
            assert_eq!(unbraced("a.ts", unended), unended_expected, "{unended:?}");
        }
    }

    #[test]
    fn do_while_closes_its_trailing_if_before_an_else() {
        let closed = "if (a) {\n  do if (b) c();\n  while (d);\n} else e();\n";
        let fixed = unbraced("a.ts", closed);
        assert_eq!(fixed, "if (a)\n  do if (b) c();\n  while (d);\nelse e();\n");

        let allocator = Allocator::default();
        let parsed = parse(&allocator, "a.ts", &fixed);
        let Some(Statement::IfStatement(outer)) = parsed.program.body.first() else {
            panic!("{fixed:?} starts with an if");
        };

        assert!(
            outer.alternate.is_some(),
            "{fixed:?} keeps the else outside"
        );

        let open = "if (a) {\n  while (d) if (b) c();\n} else e();\n";
        assert_eq!(unbraced("a.ts", open), open);
    }

    #[test]
    fn a_body_before_a_same_line_closer_unbraces() {
        let cases = [
            (
                "function f() { if (a) { b() } }\n",
                "function f() { if (a) b() }\n",
            ),
            (
                "if (a) { if (b) { f() } } (c)();\n",
                "if (a) { if (b) f() } (c)();\n",
            ),
            ("if (a) { b() } (c)();\n", "if (a) { b() } (c)();\n"),
            (
                "if (a) { if (b) { c() } } else d()\n",
                "if (a) { if (b) c() } else d()\n",
            ),
            (
                "do { if (a) { f() } } while (c)\n",
                "do { if (a) f() } while (c)\n",
            ),
            (
                "switch (x) { case 1: if (a) { b() } }\n",
                "switch (x) { case 1: if (a) b() }\n",
            ),
        ];

        for (source, expected) in cases {
            assert_eq!(unbraced("a.ts", source), expected, "{source:?}");
        }
    }

    #[test]
    fn a_closer_sharing_its_line_leaves_no_stray_text() {
        let cases = [
            (
                "function f() {\n  while (a) {\n    if (b) {\n      g()\n    } }\n  next()\n}\n",
                "function f() {\n  while (a)\n    if (b) g()\n  next()\n}\n",
            ),
            (
                "function f() {\n  if (a) { if (b) { if (c) {\n    g()\n  } } }\n}\n",
                "function f() {\n  if (a) if (b) if (c) g()\n}\n",
            ),
            (
                "function f() {\n  if (a) {\n    g()\n  }}\n",
                "function f() {\n  if (a) {\n    g()\n  }}\n",
            ),
            (
                "function f() {\n  if (a) { if (b) { if (c) {\n    g()\n  }}} // c\n}\n",
                "function f() {\n  if (a) if (b) if (c) {\n    g()\n  } // c\n}\n",
            ),
            (
                "if (a) { if (b) {\n  c()\n} } (d)();\n",
                "if (a) { if (b) {\n  c()\n} } (d)();\n",
            ),
        ];

        for (source, expected) in cases {
            assert_eq!(unbraced("a.ts", source), expected, "{source:?}");
        }
    }

    #[test]
    fn braces_a_comment_keeps_leave_their_edge_blanks_to_the_spacing_pass() {
        let source = "if (a) {\n\n  b(); // why\n\n} // kept\nnext();\n";
        assert_eq!(
            unbraced("a.ts", source),
            "if (a) {\n  b(); // why\n} // kept\n\nnext();\n"
        );
    }
}
