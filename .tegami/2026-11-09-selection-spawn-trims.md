---
packages:
  "@ryuugg/stanza": patch
---

### 🎨 File selection starts fewer git processes

Outside any repository, a check or a `PreToolUse` hook no longer starts a `git rev-parse` that can only fail, so a file under `/tmp` or another directory git would not search starts no git process at all. A repository's `.gitmodules` is read without starting `git config`, and a directory with nothing to format is listed once instead of twice. The files picked and everything printed stay the same.
