---
packages:
  "@ryuugg/stanza": patch
---

### 🎨 Large destructuring patterns check quickly

A declaration, assignment or parameter list that destructures thousands of names no longer slows `--check` and `--fix` down with every name it adds. A function whose `const [...]` pattern binds 16,000 names drops from about 2.4 billion instructions to about 50 million, and every finding names the same binding it did before.
