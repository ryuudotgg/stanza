---
packages:
  "@ryuugg/stanza": patch
---

### 🎨 One file costs the same in a crowded folder

Checking one named file, or one file read with `--stdin`, no longer reads a crowded folder in full, or the crowded folders above it, to find config files. Beside 50,000 other files, `stanza --check` on one file used about 20 times the CPU it uses now, and it now costs about the same as with no neighbours. A whole repository check on macOS costs no more than before, and the files picked and everything printed stay the same everywhere. On volumes other than APFS, HFS+, ext4, Btrfs, XFS and tmpfs, a folder is still read in full as before.
