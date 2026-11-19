use std::cell::Cell;

use super::evaluate::Value;
use crate::languages::javascript::nesting::MOST_LEVELS;

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
    pub fn link(&self, build: impl FnOnce() -> Value) -> Option<(Value, Height)> {
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
        let value = build();
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

        Some((value, height))
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
                            .map_or(Value::Unknown, |(value, _)| value)
                    });
                } else {
                    depth.link(cut_after_reach);
                }
            });

            assert_eq!(files, 33, "nested {nested}");
        }
    }
}
