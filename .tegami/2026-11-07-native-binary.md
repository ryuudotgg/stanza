---
packages:
  "@ryuugg/stanza": minor
---

### 🚨 Stanza no longer needs Bun

`@ryuugg/stanza` now runs a native binary, so `npm i -g @ryuugg/stanza` or `npx @ryuugg/stanza` works with only Node.js installed, and `bun add -g` keeps working. The install pulls one `@ryuugg/stanza-<platform>` package for your os, cpu and libc, the same binary the release archives carry. There is still no Windows binary, so run it inside WSL there. `STANZA_RAW_TRANSFER` no longer does anything.
