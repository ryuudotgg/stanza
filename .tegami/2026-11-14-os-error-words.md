---
packages:
  "@ryuugg/stanza": patch
---

### 🎨 Failures name the system's reason in plain words

When the system refuses a read, a write or a start, stanza now says why in the system's own words, as in `stanza: cannot read locked: permission denied` or the `parse` finding `cannot read: permission denied`, where it used to print a code such as `EACCES`, a `(os error 13)` suffix or nothing at all. The npm launcher does the same when it cannot start the binary, naming the platform package rather than its path, and `stanza explain` names the real reason it could not read a lint config. Only the words change: the same failures go to the same streams with the same exit codes and rule ids.
