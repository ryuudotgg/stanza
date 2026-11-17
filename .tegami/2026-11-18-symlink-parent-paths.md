---
packages:
  "@ryuugg/stanza": patch
---

### 🐛 A path stepping up out of a symlinked folder names the file the system opens

With `link` pointing at `real/inner`, `stanza --check link/../a.ts`, `--fix` and `--stdin` used to fold the `..` as text and check or rewrite the `a.ts` beside `link`, while `cat link/../a.ts` reads `real/a.ts`. They now take the same file the system opens, and so do `stanza explain` and the hook's check of a relative `Write` path. A `--check` or `--fix` path the system refuses to open, such as `missing/../a.ts` or `a.ts/`, now reports `no such file` for the path as typed instead of falling back to `a.ts`.
