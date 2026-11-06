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

pub struct OffsetMap {
    ordered: Vec<OffsetEdit>,
    shifted_starts: Vec<isize>,
    removed_before: Vec<usize>,
}

impl OffsetMap {
    pub fn new(edits: &[OffsetEdit]) -> Self {
        let ordered = by_start(edits);
        let mut shifted_starts = Vec::with_capacity(ordered.len());
        let mut removed_before = vec![0];

        let mut removed = 0;
        for edit in &ordered {
            shifted_starts.push(edit.start as isize - removed as isize);
            removed += edit.end - edit.start;
            removed_before.push(removed);
        }

        OffsetMap {
            ordered,
            shifted_starts,
            removed_before,
        }
    }

    pub fn forward(&self, offset: usize) -> usize {
        let index = self.ordered.partition_point(|edit| edit.start < offset);
        if index == 0 {
            return offset;
        }

        let edit = self.ordered[index - 1];
        let removed = self.removed_before[index - 1] as isize;
        let cut = offset.min(edit.end) as isize - edit.start as isize;
        (offset as isize - removed - cut) as usize
    }

    pub fn back(&self, offset: usize) -> usize {
        let index = lower_bound(&self.shifted_starts, &(offset as isize + 1));
        offset + self.removed_before[index]
    }

    pub fn removes(&self, offset: usize) -> bool {
        let index = self.ordered.partition_point(|edit| edit.start <= offset);
        index > 0 && offset < self.ordered[index - 1].end
    }
}

pub fn back_through(maps: &[OffsetMap], offset: usize) -> usize {
    maps.iter()
        .rev()
        .fold(offset, |offset, map| map.back(offset))
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
    fn an_offset_map_follows_text_across_removed_spans() {
        let map = OffsetMap::new(&[
            OffsetEdit { start: 6, end: 8 },
            OffsetEdit { start: 2, end: 4 },
        ]);

        let forward: Vec<usize> = (0..10).map(|offset| map.forward(offset)).collect();
        let removed: Vec<usize> = (0..10).filter(|&offset| map.removes(offset)).collect();
        let back: Vec<usize> = (0..6).map(|offset| map.back(offset)).collect();

        assert_eq!(forward, [0, 1, 2, 2, 2, 3, 4, 4, 4, 5]);
        assert_eq!(removed, [2, 3, 6, 7]);
        assert_eq!(back, [0, 1, 4, 5, 8, 9]);
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
