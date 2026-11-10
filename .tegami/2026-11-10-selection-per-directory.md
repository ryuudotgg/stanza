---
packages:
  "@ryuugg/stanza": patch
---

### 🎨 Many file paths are selected as fast as their directory

Stanza now resolves where a file really lives once per directory, not once per file and per parent folder. Passing 2000 files as arguments now costs about the same CPU as passing the one directory that holds them, where before it cost twice as much. Selecting a whole repository also uses less CPU. The files picked and everything printed stay the same.
