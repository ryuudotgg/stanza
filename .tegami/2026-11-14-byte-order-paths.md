---
packages:
  "@ryuugg/stanza": minor
---

### 🚨 Files are listed in byte order, as git lists them

Findings, `--json` output, error lines, the `--staged` repair lines, the hook's lists of files and the notices that name config files now order paths by their bytes, the order `git ls-files` and `LC_ALL=C sort` use, so `Button.ts` and `_x.ts` come before `api.ts`. A script that reads `--json` findings in order sees the new order. Errors from several path arguments sort by path, so `stanza --check z a` reports a missing `a` before an unreadable `z`, and two files whose names differ only by an invisible character no longer have their findings mixed together.

### 🎨 The binary sheds its collation data

The `stanza` binary no longer carries the Unicode collation code and data it sorted paths with, about 1.2 MB, so each platform package downloads less.
