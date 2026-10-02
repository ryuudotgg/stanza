## 0.2.0

### Blank line rules apply at module top level

The blank line rules now cover the gaps between top level statements, not just the ones inside blocks. A `let` that the function below it reads joins that function and gets a blank line above it, a multi-line statement gets a blank line after it, and a run of six statements is reported as a wall. `export const` and `export function` count as the declaration they export. Imports and re-exports stay one group, with no blank line forced between them and no wall reported for them. [short-body](https://stanza.ryuu.gg/rules/short-body) skips the top level, so a short file loses a blank line only when another rule asks for it.

## 0.1.2

### JSX element names count as uses of a binding

A component used as a JSX element, such as `<Row />` or `<Icons.Close />`, now counts as a read of `Row` or `Icons`. Its declaration joins the statement that renders it, the same way any other use would. Lowercase intrinsic elements like `<div>` are not treated as bindings.

## 0.1.1

### Generated variants are skipped for every extension

`.gen` files are now skipped for every JavaScript and TypeScript extension (`.gen.ts`, `.gen.tsx`, `.gen.mts`, `.gen.cts`, `.gen.js`, `.gen.jsx`, `.gen.mjs`, `.gen.cjs`), and `.min` files for `.js`, `.mjs` and `.cjs` (#106).
