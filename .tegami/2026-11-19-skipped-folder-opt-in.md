---
packages:
  "@ryuugg/stanza": minor
---

### ✨ The stanza attribute opts files in skipped folders back in

Setting the `stanza` git attribute on a file, such as `dist/** stanza` in `.gitattributes`, now lets every source take it even though it sits below an always skipped folder like `dist`, `build` or `node_modules`. Paths, `--changed`, `--staged`, `--stdin` and the hook all format it, while declaration files, generated names, `linguist-generated` and generated headers still skip it. Before, setting the attribute changed nothing.
