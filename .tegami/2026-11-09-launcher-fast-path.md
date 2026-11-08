---
packages:
  "@ryuugg/stanza": patch
---

### 🎨 The npm command starts the binary sooner

`stanza` from a local, npx, npm global or Bun global install now reaches the platform binary without starting `readlink` or `uname`, so each run through `node_modules/.bin/stanza`, a hook included, starts a few milliseconds sooner on macOS on Apple silicon and on Linux.

### 🐛 Apple silicon runs the Intel package when it is the only one installed

On a Mac with Apple silicon where only `@ryuugg/stanza-darwin-x64` is installed, as an install from an Intel build of Node.js leaves it, `stanza` now runs that binary under Rosetta. Before, it stopped with `no binary package for darwin-arm64`.
