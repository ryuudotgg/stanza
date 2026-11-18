---
packages:
  "@ryuugg/stanza": patch
---

### 🎨 The npm `stanza` command starts sooner on Intel Macs

On an Intel Mac the `stanza` command installed from npm now recognizes the machine from its x86_64 system library cache and starts the binary straight away, without running `uname` first. Apple silicon Macs, Linux and other systems start as before.
