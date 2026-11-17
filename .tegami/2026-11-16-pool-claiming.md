---
packages:
  "@ryuugg/stanza": patch
---

### 🎨 Every worker thread takes files from the start of a run

Worker threads now take one file at a time instead of 64, so a run of 64 to 512 files keeps every worker busy where before only one to eight of them got any files. Findings, fixes and their order are unchanged, and `STANZA_WORKERS` works as before.
