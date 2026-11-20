---
packages:
  "@ryuugg/stanza": patch
---

### 🐛 Relative names that are not UTF-8 need the working directory

When the working directory is gone, `stanza --check` and `--fix` given a relative path whose name is not valid UTF-8 now print `cannot read the working directory`, as they do for any other relative path. Before, Stanza resolved that name under `/` and reported a name that was never typed, such as `"/b\377.ts"`.
