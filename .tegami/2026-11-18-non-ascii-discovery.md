---
packages:
  "@ryuugg/stanza": patch
---

### 🎨 Repositories under non-ASCII folders skip two git processes

A repository whose path holds a name such as `café` or `日本`, or whose `.git` file or `commondir` points through one, is now found from the files on disk like any other. A `--check` of one file there starts one git process instead of three, no longer asking `git rev-parse` for the root or `git check-attr` for attributes. Which files are selected and what is printed are unchanged.
