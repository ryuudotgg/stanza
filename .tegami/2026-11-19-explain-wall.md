---
packages:
  "@ryuugg/stanza": minor
---

### ✨ Explain shows the wall a line runs through

`stanza explain` on any line of a wall now prints a wall section: the run's lines, its first and last statements, how many statements run together and that 6 or more make a wall, and that `--fix` leaves the split to you. It counts the run as `--check` does, after `--fix` removes braces and the blank lines its rules drop. A line with no gap, braced body or wall still exits 1.
