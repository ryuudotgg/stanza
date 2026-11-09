---
packages:
  "@ryuugg/stanza": patch
---

### 🐛 An internal error on one file no longer stops the run

When stanza hits a bug while formatting a file, it leaves that file untouched, reports it as an `error` finding with one `stanza:` line on stderr naming it, formats every other file and exits 2. Before, the whole run stopped with exit 101 and a Rust panic message. `stanza hook` reports the file the same way and exits 1, so the agent is not blocked.
