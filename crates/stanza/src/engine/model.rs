use std::collections::HashSet;
use std::ops::ControlFlow;

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
    Declaration,
    Assignment,
    If,
    Loop,
    Switch,
    Case,
    Try,
    Function,
    Return,
    Throw,
    Break,
    Continue,
    Expression,
    Import,
    Other,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Binding {
    Names(Vec<String>),
    Path(Vec<String>),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Declaration {
    pub keyword: &'static str,
    pub let_like: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Guard {
    pub jump: Option<&'static str>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Joined {
    pub columns: usize,
    pub arms: usize,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Facts {
    pub kind: Kind,
    pub word: Option<&'static str>,
    pub binds: Option<Binding>,
    pub declaration: Option<Declaration>,
    pub guard: Option<Guard>,
    pub compact: bool,
    pub joined: Option<Joined>,
    pub case_body: bool,
}

pub trait Reads {
    fn references(&self, doc: &Doc, names: &[&str], each: &mut dyn FnMut(&str) -> ControlFlow<()>);
    fn reads_path(&self, doc: &Doc, path: &[String]) -> bool;
    fn operation(&self, doc: &Doc) -> Option<String>;
    fn finally_repeats(&self, doc: &Doc, operation: &str) -> bool;
}

#[derive(Clone, Debug)]
pub struct Item<N> {
    pub node: N,
    pub start: usize,
    pub end: usize,
    pub facts: Facts,
}

#[derive(Clone, Debug)]
pub struct Stmt<N> {
    pub node: N,
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

#[derive(Clone, Debug)]
pub struct StatementList<N> {
    pub kind: ListKind,
    pub start: usize,
    pub end: usize,
    pub open_line: Option<usize>,
    pub close_line: Option<usize>,
    pub stmts: Vec<Stmt<N>>,
}

#[derive(Default)]
pub struct LineEdits {
    pub delete_lines: HashSet<usize>,
    pub insert_after: HashSet<usize>,
}

impl LineEdits {
    pub fn is_empty(&self) -> bool {
        self.delete_lines.is_empty() && self.insert_after.is_empty()
    }
}
