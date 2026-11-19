---
packages:
  "@ryuugg/stanza": patch
---

### 🐛 `--check --staged` ignores unstaged attribute edits to filtered files

A staged file with a `filter` attribute is now converted with every attribute the index holds. Before, an unstaged `.gitattributes` edit that only added `working-tree-encoding`, `text`, `eol`, `crlf` or `ident` changed the bytes `--staged` checked, so a UTF-16 encoding in the working tree turned a staged braces finding into a parse error.
