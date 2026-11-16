---
packages:
  "@ryuugg/stanza": patch
---

### 🐛 A file that cannot be opened is not reported as truncated

When `--fix` writes a file in place and cannot open it, the `write` finding says `cannot write: <reason>` and the file is left as it was. Before, stanza tried to restore the untouched file anyway, and when that failed too it warned `restore failed, the file may be truncated`. That warning now appears only when a write failed after the file was cut.
