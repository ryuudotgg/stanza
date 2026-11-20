---
packages:
  "@ryuugg/stanza": patch
---

### 🐛 One Prettier config per directory sets the width

When a directory holds several Prettier configs, `--fix` and `stanza explain` now take the width from the one Prettier reads, the first in Prettier's search order, and ignore the rest. A `package.json` whose `prettier` key holds a config wins over a `.prettierrc` beside it, and a `package.yaml` whose `prettier` key holds a config is now read, right after `package.json`. Before, every Prettier config in the directory competed and the smallest width won, and `package.yaml` was ignored. If Stanza cannot read the config Prettier would pick, it names that file in the unread notice and takes the width from the next source, not from another Prettier config beside it. A Prettier config holding only `null`, `false` or an empty string now counts as an empty config, as it does in Prettier, so the width is Prettier's default and no notice is printed.
