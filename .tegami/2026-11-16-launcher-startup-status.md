---
packages:
  "@ryuugg/stanza": patch
---

### 🐛 The npm command starts through a working runtime and exits with its own status

When the platform package holds something other than a regular executable file, `stanza` now falls back like a missing binary and exits 1 for `stanza hook` and 2 otherwise, where the shell used to exit 126. With no platform binary to run, it starts Bun or Node.js only after that runtime answers `--version`, so a version manager's `bun` shim with no Bun behind it no longer stops a machine that has Node.js. `sh stanza` run inside `node_modules/.bin` also finds its package now.
