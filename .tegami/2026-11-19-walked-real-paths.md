---
packages:
  "@ryuugg/stanza": patch
---

### 🐛 A symlinked folder outside git is walked from where it leads

Outside git, `stanza --check link real` with `link` pointing at `real` used to report each file twice, once under each name, and a pooled `--fix` over both could stop with `write changed while stanza ran`. A folder argument outside git is now walked from its real location, as one inside a repository already is: each file is checked once under its real name, an absolute path through a symlinked folder such as `/tmp` prints the same relative name, and the `.gitignore` files above the link's target apply.
