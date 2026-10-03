---
packages:
  "@ryuugg/stanza": patch
---

### Release binaries start faster

The binaries on the GitHub release now ship precompiled bytecode, so each run skips parsing Stanza's own code and starts a few milliseconds sooner. Each binary is about 2 MB larger. Output is unchanged.
