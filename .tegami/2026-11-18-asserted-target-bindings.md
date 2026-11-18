---
packages:
  "@ryuugg/stanza": patch
---

### 🐛 An asserted assignment target binds its name

An assignment to an identifier wrapped in a TypeScript `as`, `satisfies`, `!` or `<T>` assertion, such as `(a as any) = 1;`, now binds `a` the way `a = 1;` does, so the `if (a)` below it gets the same `guard-join` finding. Before, the wrapped target bound no name and the blank line between them stayed.
