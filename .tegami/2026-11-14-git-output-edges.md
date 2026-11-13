---
packages:
  "@ryuugg/stanza": patch
---

### 🐛 Git failures name the git command

When a git call fails, the error line now names the subcommand, such as `git diff failed in <dir>`, where a call that passed `-c` settings used to name the first setting instead. A git killed by a signal now reads `killed by signal <n>` instead of `exit null`.

### 🐛 File names that start with a byte order mark are selected

A file whose name begins with U+FEFF is now selected, checked against its `.gitattributes` and reported under its own name. Before, git's listing lost that character from the first name, so a directory run or `--changed` could leave the file out.
