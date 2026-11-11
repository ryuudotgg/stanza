---
packages:
  "@ryuugg/stanza": patch
---

### 🐛 Stanza names files whose names are not UTF-8

On Linux, a TypeScript or JavaScript file whose name held bytes that are not valid UTF-8 used to be skipped without a word by `--check .`, `--changed` and the Stop hook, named wrongly by `--staged`, and reported missing when named as a path, while a file named with the replacement character could be read in its place. Every source now prints `stanza: name is not valid UTF-8: "bad\377.ts"`, quoting the name as git does, leaves that file untouched and exits 2, or 1 for the hook, and the rest of the run still goes ahead.
