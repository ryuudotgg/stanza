#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CommentKind {
    Line,
    Block,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Comment<'a> {
    pub kind: CommentKind,
    pub value: &'a str,
    pub start: usize,
    pub end: usize,
}

pub struct Doc<'a> {
    pub text: &'a str,
    pub lines: Vec<&'a str>,
    pub line_starts: Vec<usize>,
    pub comments: Vec<Comment<'a>>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Case,
    Other,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Facts {
    pub kind: Kind,
    pub case_body: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Item {
    pub start: usize,
    pub end: usize,
    pub facts: Facts,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Stmt {
    pub start: usize,
    pub end: usize,
    pub facts: Facts,
    pub frozen: bool,
    pub start_line: usize,
    pub end_line: usize,
    pub multiline: bool,
    pub detached: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ListKind {
    Block,
    Switch,
    Module,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StatementList {
    pub kind: ListKind,
    pub start: usize,
    pub end: usize,
    pub open_line: Option<usize>,
    pub close_line: Option<usize>,
    pub stmts: Vec<Stmt>,
}
