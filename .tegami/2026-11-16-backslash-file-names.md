---
packages:
  "@ryuugg/stanza": patch
---

### 🐛 A backslash in a file name is part of the name

A file named `dist\a.ts` is now checked like any other file, where it used to be skipped as if it sat in a `dist` folder. Only `/` separates folders, so `src\dist/a.ts` is no longer skipped either.
