---
packages:
  "@ryuugg/stanza": patch
---

### 🐛 Explain notes only the braces that bear on a gap

`stanza explain` on a gap now notes a brace removal only when the braces sit inside one of the gap's two statements, or inside a block of 2 or 3 statements where `short-body` judges every statement. Before, a gap at module level named braces inside any function in the file, so a gap decided by `use-join` beside an unrelated function claimed it was decided after `--fix` removed that function's braces.
