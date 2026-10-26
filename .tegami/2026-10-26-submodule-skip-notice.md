---
packages:
  "@ryuugg/stanza": patch
---

### 🐛 Skipped submodules are named on stderr

A path in a superproject still leaves out the files inside its git submodules, and `--check` and `--fix` now say so with `stanza: skipped git submodule <path>, pass its path to format its files` instead of nothing. `--changed` and `--staged` print the same line for a submodule they skip. A selection holding only a submodule no longer claims git ignores its files, and the exit status is unchanged.
