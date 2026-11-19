---
packages:
  "@ryuugg/stanza": patch
---

### 🐛 ESLint patterns Stanza reads unlike ESLint keep braces

In ESLint `files`, `ignores`, `excludedFiles` and override patterns, a brace group without a comma such as `{src}`, a range such as `{1..3}`, a backslash, an empty path segment, and a `.` or `..` segment now leave Stanza unsure which files the pattern covers, so braces stay. `--fix` keeps braces where such a pattern turns `curly` on, and such a pattern can no longer turn `curly` off. Before, `{src}/**` reached `src` although ESLint reads it as a directory literally named `{src}`, and `src/{1..3}/**` reached no file. oxlint still reads `{src}` as `src`.
