---
packages:
  "@ryuugg/stanza": patch
---

### 🐛 Large lint configs no longer stall a run or exhaust memory

Reading an ESLint config used to take time or memory that grew with the square of its size in a few shapes: an object spread into many entries, inline `extends` nested deeply, many extended files, many aliases or `exports.x =` assignments, and a `.eslintrc.yaml` line full of ` #` before a lone carriage return. A 90 KB `eslint.config.js` that spreads one 4000 key object into 4000 entries now checks in 43 MB, where it took 900 MB. Every config resolves to the same settings as before.
