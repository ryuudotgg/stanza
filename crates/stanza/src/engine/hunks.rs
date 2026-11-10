use super::doc::line_at;
use super::edits::{OffsetEdit, OffsetMap};
use super::model::{Doc, LineEdits};
use crate::files::Changed;

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

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use super::*;

    #[test]
    fn touching_reads_lines_and_deletions_inside_the_range() {
        let lines = Changed {
            lines: BTreeSet::from([4]),
            deleted_after: BTreeSet::from([7]),
        };

        let touches = touching(Some(&lines));
        assert!(touches(4, 4));
        assert!(!touches(5, 7));
        assert!(touches(5, 8));
        assert!(touching(None)(1, 1));
    }
}
