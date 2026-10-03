---
packages:
  "@ryuugg/stanza": patch
---

### Whole repo runs look up each directory once

`--check` and `--fix` resolve a directory's real path, look for config files in it and read the `package.json` files above it once per run, rather than once per file. Large repos spend less time on file system calls. Output is unchanged.
