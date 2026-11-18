---
packages:
  "@ryuugg/stanza": patch
---

### 🐛 A stanza-off in a decorator callback ends with the callback

A `stanza-off` inside the callback of a decorator now stops at the end of that callback, like one in any other function. Before, it ran to the end of the file in two cases: a TypeScript parameter decorator such as `constructor(@d(() => { ... }) x: T)` on a parameter that was not optional, and a class decorator written before `export`. Everything after it kept braces and blank lines `--fix` should have changed.

### 🐛 A changed leading comment scopes a TypeScript file like a JavaScript one

With `--changed --hunks` or `--staged --hunks`, editing a comment above the first statement of a `.ts` file now checks the gaps of the top level statements, as it already did for `.js`. Before, the `.ts` file reported nothing for that change.
