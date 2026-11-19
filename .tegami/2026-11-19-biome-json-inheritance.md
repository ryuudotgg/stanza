---
packages:
  "@ryuugg/stanza": patch
---

### 🐛 Biome `extends` reads every base as JSON

A Biome config and each file it `extends` are now read as JSON with comments and trailing commas, whatever the file's suffix, as Biome reads them. A relative `extends` path is taken exactly as written, with no extension added, and Stanza follows at most 10 nested `extends`. Before, an `extends` to a `.js` file ran it as JavaScript, so a JavaScript base that turned `useBlockStatements` off let `--fix` remove braces, and a `.js` base holding JSON kept braces with a false notice. A base that is not JSON, cannot be found, sits in a cycle or past the 10th level now keeps braces with the notice naming the config, even when a later override turns the rule off, since Biome rejects the whole config then.
