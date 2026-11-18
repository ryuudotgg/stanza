---
packages:
  "@ryuugg/stanza": minor
---

### 🚨 Override patterns the formatters read differently leave their config unread

When an oxfmt, Prettier or Biome config holds an override or include pattern whose meaning differs between the formatters, Stanza now names that config in the unread notice and takes the width from the next source, even when the pattern's override does not cover the file. That covers extglobs such as `@(a|b).ts`, ranges such as `{1..3}.ts`, braces with one choice such as `{src}/**`, nested braces, a `/` or `**` inside braces such as `{src/**,lib/**}`, negated or POSIX classes such as `[!a]` and `[[:alpha:]]`, `**` inside a name such as `**.ts`, a `+` after a class or braces such as `[ab]+.ts`, a backslash or double quote, and a leading `!` in a Prettier or oxfmt pattern. A Biome pattern with `?` or a class does the same, since Biome rejects it. Write `{src/**,lib/**}` as `{src,lib}/**` to keep the config's width. A leading `!` in a Biome include list still excludes what it names.
