---
packages:
  "@ryuugg/stanza": patch
---

### 🐛 Unreadable paths are reported in one voice

A file `--check` or `--fix` cannot read or write gets a `parse` or `write` finding reading `cannot read: <code>` or `cannot write: <code>`, such as `cannot read: EACCES`, instead of raw error text with an absolute path. A directory or named path stanza cannot reach prints `stanza: cannot read <path>: <code>` and exits 2, including a directory inside a git repository, which was silently skipped with exit 0 before. A symlink loop no longer reads as `no such file`.
