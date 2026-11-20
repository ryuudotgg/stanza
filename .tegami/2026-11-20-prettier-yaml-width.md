---
packages:
  "@ryuugg/stanza": patch
---

### 🐛 YAML Prettier configs give their width and overrides

`.prettierrc.yaml`, `.prettierrc.yml` and an extensionless `.prettierrc` written in YAML now give `--fix` the same width, tab width and `overrides` as the same settings in `.prettierrc.json`, including flow maps, a leading `---` and comments or strings holding U+2028 or U+2029. Before, any `overrides` list made the file unread and `--fix` fell back to the next width source. A YAML config that uses syntax Stanza does not read, such as an anchor, an alias, a tag or a multiline scalar, is now unread as a whole, with one notice, even when that syntax sits under a key unrelated to width. Before, Stanza could take a width from a file it had only partly read. An extensionless `.prettierrc` holding JavaScript, such as `{ printWidth: 60 + 60 }` or JSON with `//` comments, is now unread too, since Prettier refuses to load it.
