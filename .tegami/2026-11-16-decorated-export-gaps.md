---
packages:
  "@ryuugg/stanza": patch
---

### 🐛 Decorated exported classes start at their decorator

A decorated `export class` or `export default class` now starts at its first decorator, like a decorated class without `export`. `--fix` puts the blank line above the decorator instead of between the decorator and `export`, adds the blank line after a multi-line class, and a `// stanza-ignore` above the decorator holds the class's gaps. A class written as `export @dec class` still starts at `export`.
