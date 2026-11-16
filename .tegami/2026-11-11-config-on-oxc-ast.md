---
packages:
  "@ryuugg/stanza": patch
---

### 🎨 Smaller binary

JavaScript and TypeScript configs are now read straight from the parsed syntax tree instead of a JSON copy of it, so the `stanza` binary is about 210 KB smaller. Every brace setting, line width and notice is unchanged.
