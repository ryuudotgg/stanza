use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};
use std::rc::{Rc, Weak};

use super::evaluate::Value;
use crate::languages::javascript::nesting::MOST_LEVELS;

// A lazy can be forced after the answer holding it was cached, so its files reach that answer only through parents.
#[derive(Default)]
pub struct Touched {
    files: RefCell<Rc<HashSet<Rc<str>>>>,
    hit: Cell<bool>,
    parents: RefCell<Vec<Weak<Self>>>,
}

impl std::fmt::Debug for Touched {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("Touched")
    }
}

impl Touched {
    pub fn files(&self) -> Vec<Rc<str>> {
        self.files.borrow().iter().cloned().collect()
    }

    pub fn agree(&self, left: &[Rc<str>], right: &[Rc<str>]) -> bool {
        let files = self.files.borrow();
        if files.is_empty() {
            return true;
        }

        let mut left: Vec<&Rc<str>> = left.iter().filter(|file| files.contains(*file)).collect();
        let mut right: Vec<&Rc<str>> = right.iter().filter(|file| files.contains(*file)).collect();
        left.sort_unstable();
        right.sort_unstable();
        left == right
    }

    pub fn hit(&self) -> bool {
        self.hit.get()
    }

    fn mark_hit(self: &Rc<Self>) {
        let mut pending = vec![self.clone()];
        while let Some(cell) = pending.pop() {
            if !cell.hit.replace(true) {
                pending.extend(cell.parents.borrow().iter().filter_map(Weak::upgrade));
            }
        }
    }

    fn add(self: &Rc<Self>, file: &Rc<str>) {
        let mut pending = vec![self.clone()];
        while let Some(cell) = pending.pop() {
            if cell.files.borrow().contains(file) {
                continue;
            }

            Rc::make_mut(&mut cell.files.borrow_mut()).insert(file.clone());
            pending.extend(cell.parents.borrow().iter().filter_map(Weak::upgrade));
        }
    }

    pub fn include(self: &Rc<Self>, child: &Rc<Self>) {
        let known = child
            .parents
            .borrow()
            .last()
            .is_some_and(|last| std::ptr::eq(last.as_ptr(), Rc::as_ptr(self)));
        if known || Rc::ptr_eq(self, child) {
            return;
        }

        child.parents.borrow_mut().push(Rc::downgrade(self));
        if child.hit.get() {
            self.mark_hit();
        }

        let files = child.files.borrow().clone();
        let fresh = self.files.borrow().is_empty() && self.parents.borrow().is_empty();
        if fresh {
            *self.files.borrow_mut() = files;
            return;
        }

        if Rc::ptr_eq(&self.files.borrow(), &files) {
            return;
        }

        for file in files.iter() {
            self.add(file);
        }
    }
}

#[derive(Debug)]
pub struct Answer {
    pub chain: Rc<Vec<Rc<str>>>,
    pub value: Value,
    pub height: Height,
    pub touched: Rc<Touched>,
}

// A config reaches a chain through the binding it exports, so the cap holds that binding, 1,000 links and the binding the links start from.
const LINK_CAP: usize = 1_002;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Height {
    links: usize,
    frames: usize,
    files: usize,
}

impl Height {
    const PAST_CAP: Self = Self {
        links: LINK_CAP + 1,
        frames: MOST_LEVELS + 1,
        files: 0,
    };

    const fn past_cap(self) -> bool {
        self.links > LINK_CAP || self.frames > MOST_LEVELS
    }
}

#[derive(Default)]
pub struct Depth {
    open: Cell<usize>,
    frames: Cell<usize>,
    base: Cell<usize>,
    reach: Cell<Height>,
    cut: Cell<bool>,
    touched: RefCell<Rc<Touched>>,
    epoch: Cell<usize>,
    roots: RefCell<HashMap<String, usize>>,
}

struct Epoch<'a> {
    depth: &'a Depth,
    outer: usize,
}

impl Drop for Epoch<'_> {
    fn drop(&mut self) {
        self.depth.epoch.set(self.outer);
    }
}

struct Traced<'a> {
    depth: &'a Depth,
    outer: Rc<Touched>,
}

impl Drop for Traced<'_> {
    fn drop(&mut self) {
        let inner = self.depth.touched.replace(self.outer.clone());
        self.outer.include(&inner);
    }
}

struct Opened<'a> {
    depth: &'a Depth,
    reach: Height,
}

impl Drop for Opened<'_> {
    fn drop(&mut self) {
        let open = self.depth.open.get() - 1;
        self.depth.open.set(open);
        self.depth.reach.set(self.reach);
        if open == 0 {
            self.depth.cut.set(false);
        }
    }
}

pub struct Frame<'a>(&'a Depth);

impl Drop for Frame<'_> {
    fn drop(&mut self) {
        self.0.frames.set(self.0.frames.get() - 1);
    }
}

impl Depth {
    pub fn touch(&self, file: &Rc<str>) {
        self.touched.borrow().clone().add(file);
    }

    pub fn hit(&self) {
        self.current().mark_hit();
    }

    pub fn current(&self) -> Rc<Touched> {
        self.touched.borrow().clone()
    }

    pub fn traced<R>(&self, run: impl FnOnce() -> R) -> (R, Rc<Touched>) {
        let inner = Rc::new(Touched::default());
        (self.traced_into(&inner, run), inner)
    }

    pub fn traced_into<R>(&self, cell: &Rc<Touched>, run: impl FnOnce() -> R) -> R {
        let _traced = Traced {
            depth: self,
            outer: self.touched.replace(cell.clone()),
        };

        run()
    }

    pub fn begin_root(&self, file: &str) {
        let mut roots = self.roots.borrow_mut();
        let next = roots.len() + 1;
        self.epoch
            .set(*roots.entry(file.to_owned()).or_insert(next));
    }

    pub fn epoch(&self) -> usize {
        self.epoch.get()
    }

    // A lazy forced long after its root was read, as a width mirror is, still answers for that root.
    pub fn within<R>(&self, epoch: usize, run: impl FnOnce() -> R) -> R {
        let _epoch = Epoch {
            depth: self,
            outer: self.epoch.replace(epoch),
        };

        run()
    }

    pub fn scope(&self, scoped: bool) -> usize {
        if scoped { self.epoch.get() } else { 0 }
    }

    pub fn remember(
        &self,
        answers: &mut Vec<Answer>,
        chain: &Rc<Vec<Rc<str>>>,
        value: &Value,
        height: Height,
        touched: Rc<Touched>,
    ) {
        answers.push(Answer {
            chain: chain.clone(),
            value: value.clone(),
            height,
            touched,
        });
    }

    pub fn find<'a>(
        &self,
        answers: Option<&'a Vec<Answer>>,
        chain: &[Rc<str>],
    ) -> Option<&'a Answer> {
        answers?
            .iter()
            .find(|answer| answer.touched.agree(&answer.chain, chain))
    }

    pub fn served(&self, answers: Option<&Vec<Answer>>, chain: &[Rc<str>]) -> Option<Value> {
        let answer = self.find(answers, chain)?;
        self.note(answer.height);
        self.current().include(&answer.touched);
        Some(answer.value.clone())
    }

    pub fn note(&self, height: Height) {
        let reach = self.reach.get();
        self.reach.set(Height {
            links: reach.links.max(height.links),
            frames: reach.frames.max(self.frames.get() + height.frames),
            files: reach.files.max(height.files),
        });
    }

    pub fn enter_file(&self, files: usize) {
        self.note(Height {
            files,
            ..Height::default()
        });
    }

    pub fn files_reached<R>(&self, read: impl FnOnce() -> R) -> (R, usize) {
        let outer = self.reach.get().files;
        self.reach.set(Height {
            files: 0,
            ..self.reach.get()
        });

        let result = read();
        let files = self.reach.get().files;
        self.reach.set(Height {
            files: outer.max(files),
            ..self.reach.get()
        });

        (result, files)
    }

    pub fn cut(&self) -> bool {
        self.cut.get()
    }

    pub fn frame(&self) -> Option<Frame<'_>> {
        let frames = self.frames.get() + 1;
        if self.open.get() > 0 && (self.cut.get() || frames - self.base.get() > MOST_LEVELS) {
            self.cut.set(true);
            return None;
        }

        self.frames.set(frames);
        self.note(Height::default());
        Some(Frame(self))
    }

    // A cut answer is not cached: read from nearer the top, the same binding may fit, so caching it would make the answer depend on which reader came first.
    pub fn link(&self, build: impl FnOnce() -> Value) -> Option<(Value, Height, Rc<Touched>)> {
        if self.cut.get() {
            return None;
        }

        if self.open.get() == LINK_CAP {
            self.cut.set(true);
            return None;
        }

        let start = self.frames.get();
        if self.open.get() == 0 {
            self.base.set(start);
        }

        let opened = Opened {
            depth: self,
            reach: self.reach.replace(Height {
                links: 0,
                frames: start,
                files: 0,
            }),
        };

        self.open.set(self.open.get() + 1);
        let (value, touched) = self.traced(build);
        let reach = self.reach.get();
        let height = if self.cut.get() {
            Height::PAST_CAP
        } else {
            Height {
                links: reach.links + 1,
                frames: reach.frames - start,
                files: reach.files,
            }
        };

        drop(opened);
        self.enter_file(reach.files);
        if self.cut.get() {
            return None;
        }

        self.note(height);
        let value = if height.past_cap() {
            Value::Unknown
        } else {
            value
        };

        Some((value, height, touched))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stable_config_imports_keep_file_reach_through_a_cut() {
        for nested in [false, true] {
            let depth = Depth::default();
            let cut_after_reach = || {
                depth.enter_file(33);
                depth.cut.set(true);
                Value::Unknown
            };

            let ((), files) = depth.files_reached(|| {
                if nested {
                    depth.link(|| {
                        depth
                            .link(cut_after_reach)
                            .map_or(Value::Unknown, |(value, ..)| value)
                    });
                } else {
                    depth.link(cut_after_reach);
                }
            });

            assert_eq!(files, 33, "nested {nested}");
        }
    }
}
