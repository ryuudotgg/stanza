use super::model::{Doc, LineEdits};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct OffsetEdit {
    pub start: usize,
    pub end: usize,
}

pub fn lower_bound<T: Ord>(values: &[T], target: &T) -> usize {
    let mut low = 0;
    let mut high = values.len();
    while low < high {
        let middle = (low + high) / 2;
        if values[middle] < *target {
            low = middle + 1;
        } else {
            high = middle;
        }
    }

    low
}

fn by_start(edits: &[OffsetEdit]) -> Vec<OffsetEdit> {
    let mut ordered = edits.to_vec();
    ordered.sort_by_key(|edit| edit.start);
    ordered
}

pub struct OffsetBack {
    shifted_starts: Vec<isize>,
    removed_before: Vec<usize>,
}

impl OffsetBack {
    pub fn new(edits: &[OffsetEdit]) -> Self {
        let mut shifted_starts = Vec::with_capacity(edits.len());
        let mut removed_before = vec![0];

        let mut removed = 0;
        for edit in by_start(edits) {
            shifted_starts.push(edit.start as isize - removed as isize);
            removed += edit.end - edit.start;
            removed_before.push(removed);
        }

        OffsetBack {
            shifted_starts,
            removed_before,
        }
    }

    pub fn back(&self, offset: usize) -> usize {
        let index = lower_bound(&self.shifted_starts, &(offset as isize + 1));
        offset + self.removed_before[index]
    }
}

pub fn apply_offsets(text: &str, edits: &[OffsetEdit]) -> String {
    let mut result = String::with_capacity(text.len());

    let mut cursor = 0;
    for edit in by_start(edits) {
        if edit.start >= cursor {
            result.push_str(&text[cursor..edit.start.min(text.len())]);
        }

        cursor = cursor.max(edit.end);
    }

    result.push_str(&text[cursor.min(text.len())..]);
    if !text.ends_with('\n') && result.ends_with('\n') {
        result.pop();
        if result.ends_with('\r') {
            result.pop();
        }
    }

    result
}

pub fn apply_lines(doc: &Doc, edits: &LineEdits) -> String {
    let mut lines = Vec::with_capacity(doc.lines.len() + edits.insert_after.len());
    for (index, &line) in doc.lines.iter().enumerate() {
        if !edits.delete_lines.contains(&(index + 1)) {
            lines.push(line);
        }

        if edits.insert_after.contains(&(index + 1)) {
            lines.push(if line.ends_with('\r') { "\r" } else { "" });
        }
    }

    lines.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::doc::document;

    fn applied(text: &str, delete: &[usize], insert: &[usize]) -> String {
        let doc = document(text, Vec::new());
        let edits = LineEdits {
            delete_lines: delete.iter().copied().collect(),
            insert_after: insert.iter().copied().collect(),
        };

        apply_lines(&doc, &edits)
    }

    #[test]
    fn deletes_and_inserts_whole_lines() {
        assert_eq!(applied("a\n\nb\nc\n", &[2], &[3]), "a\nb\n\nc\n");
    }

    #[test]
    fn an_inserted_line_copies_the_carriage_return_above() {
        assert_eq!(
            applied("a\r\nb\nc\r\n", &[], &[1, 2]),
            "a\r\n\r\nb\n\nc\r\n"
        );
    }
}
