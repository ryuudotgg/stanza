---
packages:
  "@ryuugg/stanza": patch
---

### 🎨 Runs with many findings use less memory

Stanza used to keep a copy of the file name on every finding and build the whole report before printing the first line. Findings now share their file's name and the report goes out in pieces as it is written, so a `--check` run with hundreds of thousands of findings needs about half the memory it did, with or without `--json`. The report itself is unchanged, byte for byte and in the same order.
