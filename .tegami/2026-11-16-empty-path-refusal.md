---
packages:
  "@ryuugg/stanza": patch
---

### 🐛 An empty path argument stops the run instead of selecting the folder

An empty path given to `--check` or `--fix`, alone, beside other paths or after `--`, used to select every file in the working directory, so `stanza --fix a.ts ""` rewrote the whole folder. Stanza now prints `stanza: "" is not a path, pass . for the current folder`, exits 2 and reads or writes no file, as `git ls-files -- ""` refuses the same pathspec.
