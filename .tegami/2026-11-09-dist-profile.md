---
packages:
  "@ryuugg/stanza": patch
---

### 🎨 The binary is about 500 KB smaller and a little faster

The `stanza` binary in each platform package is now built with whole program optimisation and without a regular expression parser it never used, so it downloads less and a serial `--check` runs about 5% fewer instructions. Every finding and fix is unchanged.
