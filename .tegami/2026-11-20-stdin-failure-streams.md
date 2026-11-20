---
packages:
  "@ryuugg/stanza": patch
---

### 🚨 Stdin failures stay inside the JSON array

`stanza --fix --json --stdin <path>` now reports a failure after reading stdin, such as a path that is not TypeScript or JavaScript, a git error or a deleted working directory, as an `error` entry in the findings array on stderr, and prints no `stanza:` line after it. Input stdin cannot supply, such as a directory, gets the same entry. `--fix --stdin` also prints the input unchanged on stdout when the working directory is gone, where before stdout was empty. A tool that read the trailing line from stderr should read the `error` entry instead.
