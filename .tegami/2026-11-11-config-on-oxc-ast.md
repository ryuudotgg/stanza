---
packages:
  "@ryuugg/stanza": patch
---

### 🎨 Smaller binary and lighter config reads

JavaScript and TypeScript configs are now read straight from the parsed syntax tree instead of a JSON copy of it, so the `stanza` binary is about 210 KB smaller and a large config takes far less memory to read. A repository whose `eslint.config.js` imports a 600 KB module checks in about 25 MB, where it took 164 MB. Every brace setting, line width and notice is unchanged.
