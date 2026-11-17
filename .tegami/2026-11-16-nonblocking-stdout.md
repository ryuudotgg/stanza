---
packages:
  "@ryuugg/stanza": patch
---

### 🐛 A full non-blocking pipe waits for its reader

When stdout or stderr is a non-blocking pipe, stanza now waits for the reader to drain a full pipe instead of stopping. The reader gets the whole report and the same exit code as from an ordinary pipe, where before a report past 64 KiB stopped short with `cannot write output: resource temporarily unavailable` and exit 2.
