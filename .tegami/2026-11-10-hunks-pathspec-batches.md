---
packages:
  "@ryuugg/stanza": patch
---

### 🐛 `--hunks` works on any number of changed files

`--hunks` used to pass every changed file to one `git diff`, so past roughly 16,000 files on macOS or 33,000 on Linux the command line grew too long and the run stopped with `E2BIG: argument list too long`, the pre-commit hook and the Stop hook included. Stanza now asks git about the files in batches and joins the answers, so a codemod over tens of thousands of files gets the same findings and fixes as a small edit. A run over a large edit also needs less memory, since stanza keeps one batch of diff text at a time instead of two copies of the whole diff.
