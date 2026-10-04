---
packages:
  "@ryuugg/stanza": patch
---

### Explain accepts paths starting with a dash

`stanza explain` now accepts `--` before a location whose path starts with `-`. Flags are parsed only before the first `--`; arguments after it are literal.
