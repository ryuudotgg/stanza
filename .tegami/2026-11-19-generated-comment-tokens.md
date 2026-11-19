---
packages:
  "@ryuugg/stanza": patch
---

### 🐛 Marker text inside a string no longer marks a file generated

A line such as `// @generated` that sits inside a template literal, or inside a quoted string continued with a backslash, used to make Stanza skip the whole file as generated. Only a real comment in the first ten lines marks a file generated now, so these files are checked and fixed like any other.
