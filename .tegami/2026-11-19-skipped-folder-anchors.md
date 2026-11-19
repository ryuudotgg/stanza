---
packages:
  "@ryuugg/stanza": patch
---

### 🐛 A folder inside a skipped folder stays skipped

In a git repository, `stanza --check dist` and `stanza --check .` run inside `dist` now skip the files there, as `stanza --check .` and `stanza --check dist/a.ts` already did, and print that its files are always skipped. Before, passing the folder itself, or running from inside it, checked and fixed every file in it. Setting the `stanza` attribute still opts those files in, and a folder below it that git cannot open now prints its `cannot read` line instead of leaving its files out without a word. Outside git the folder you pass is still where the skipped names start counting.
