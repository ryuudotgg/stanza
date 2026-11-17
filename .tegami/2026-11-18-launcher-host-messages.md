---
packages:
  "@ryuugg/stanza": patch
---

### 🐛 The npm command names supported platforms and honours AGENT_HOOKS=0

On a platform with no Stanza build, such as Windows or FreeBSD, `stanza` now says so and lists the six supported platforms, where it used to suggest installing a package and downloading a binary that do not exist. With `AGENT_HOOKS=0`, `stanza hook` prints nothing and exits 0 even when no binary can start, under `sh`, Node.js and Bun.
