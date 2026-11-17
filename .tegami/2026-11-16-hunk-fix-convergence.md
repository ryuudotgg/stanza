---
packages:
  "@ryuugg/stanza": patch
---

### 🐛 A second hunk fix keeps blank lines the first one left alone

When `--fix --changed --hunks` removed the braces of a body that started with a blank line, the next run could count the joined line as an edit and remove a blank line outside the first run's scope, such as the one above the function's closing brace. The next run now counts the joined line and the blank line it absorbed as unchanged, so a repeated run changes no byte.
