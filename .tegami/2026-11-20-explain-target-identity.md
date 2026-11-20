---
packages:
  "@ryuugg/stanza": patch
---

### 🐛 Explain skips a file when `--check` does

`stanza explain` now decides whether `--fix` and `--check` skip a file by running the same check `--check` runs on that path, so a symlink is judged by the file it points to. Before, explain judged the name you typed: a link to a generated file printed no skip note, a link opted out with the `stanza` attribute printed the opt out note although `--check` formats its target, and a path through a regular file such as `a.ts/x.ts` failed with a git error instead of `no such file`.
