---
packages:
  "@ryuugg/stanza": patch
---

### 🐛 Hunk runs keep braces short-body cannot join

`--fix --changed --hunks` used to remove braces outside the changed lines whenever a changed gap with a blank line sat in a list of 3 or fewer statements, even at the top level of a file or beside a statement that spans several lines, where `short-body` never joins the list. It now reaches past the changed lines only in a block of 2 or 3 statements that are all compact once their braces come off, and `--check --changed --hunks` reports the same.
