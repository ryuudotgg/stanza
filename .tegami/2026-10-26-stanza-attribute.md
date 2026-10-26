---
packages:
  "@ryuugg/stanza": minor
---

### A git attribute keeps stanza off a file

Write `path/to/file.ts -stanza` or `path/to/file.ts stanza=false` in `.gitattributes` to keep stanza off a file without editing it. No selection source takes the file: paths, `--changed`, `--staged`, `--stdin` and the hook all skip it. `--check` on it alone says the stanza attribute opted it out, and `stanza explain` names the attribute as the reason both modes skip it.
