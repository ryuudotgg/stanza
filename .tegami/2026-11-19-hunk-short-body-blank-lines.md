---
packages:
  "@ryuugg/stanza": patch
---

### 🎨 Hunk runs unbrace a short body whatever blank lines its changed gap holds

When a changed gap sits in a block of 2 or 3 statements that are all compact once their braces come off, `--fix --changed --hunks` now removes the braces on every statement of that block, as a whole-file `--fix` would, whether or not the changed gap holds a blank line. It used to keep those braces when the changed gap had none, and `--check --changed --hunks` now reports the same braces.
