## 0.1.2

### JSX element names count as uses of a binding

A component used as a JSX element, such as `<Row />` or `<Icons.Close />`, now counts as a read of `Row` or `Icons`. Its declaration joins the statement that renders it, the same way any other use would. Lowercase intrinsic elements like `<div>` are not treated as bindings.

## 0.1.1

### Generated variants are skipped for every extension

`.gen` files are now skipped for every JavaScript and TypeScript extension (`.gen.ts`, `.gen.tsx`, `.gen.mts`, `.gen.cts`, `.gen.js`, `.gen.jsx`, `.gen.mjs`, `.gen.cjs`), and `.min` files for `.js`, `.mjs` and `.cjs` (#106).
