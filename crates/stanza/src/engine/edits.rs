use super::model::{Doc, LineEdits};

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
