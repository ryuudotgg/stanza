---
packages:
  "@ryuugg/stanza": patch
---

### 🐛 The Linux glibc builds run on glibc 2.17 and newer

`stanza-linux-x64` and `stanza-linux-arm64`, from npm or the release archives, now start on any distribution with glibc 2.17 or newer, CentOS 7 included. Before, the arm64 build stopped with `GLIBC_2.39 not found` on Debian 12, Ubuntu 22.04 and every other release older than glibc 2.39, and the x64 build needed glibc 2.34.

### 🎨 Smaller binaries

The binaries are about a fifth smaller, since release builds now strip their symbol tables.
