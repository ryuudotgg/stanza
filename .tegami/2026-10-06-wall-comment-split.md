---
packages:
  "@ryuugg/stanza": patch
---

### Walls with a comment inside a joined group are reported again

`wall` again reports a run of joined statements, such as six guards, when a comment sits directly above one of them, because a blank line between that comment and its statement survives `--fix` and splits the run. Runs that no kept blank line can break stay unreported, and `--fix` output is unchanged.
