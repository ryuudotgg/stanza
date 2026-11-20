---
packages:
  "@ryuugg/stanza": patch
---

### 🚨 Internal errors name where they started

An internal error now ends with the place in stanza's source where it started, as ` at <file>:<line>`, in both the stderr line and the `error` finding's message, such as `internal error (index out of bounds at crates/stanza/src/step.rs:212), left untouched`. The report stays on one line and prints no backtrace, with or without `RUST_BACKTRACE`. A tool that matched the whole message of an `error` finding or an internal error line should match its start instead.
