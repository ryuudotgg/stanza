---
packages:
  "@ryuugg/stanza": patch
---

### 🐛 A failed attribute query is named even when every name is invalid

When `git check-attr` fails while Stanza judges a file name that is not valid UTF-8, `--check` and `--fix` now print `git check-attr failed:` with git's message and exit 2, for a folder, `--changed`, `--staged` and a named file alike. Before, Stanza hid that failure and reported `name is not valid UTF-8` instead, so `--staged` named a different cause depending on whether a valid file was staged beside the name.
