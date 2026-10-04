## 0.3.2

### Release binaries start faster

The binaries on the GitHub release now ship precompiled bytecode, so each run skips parsing Stanza's own code and starts a few milliseconds sooner. Each binary is about 2 MB larger. Output is unchanged.

### Whole repo runs look up each directory once

`--check` and `--fix` resolve a directory's real path, look for config files in it and read the `package.json` files above it once per run, rather than once per file. Large repos spend less time on file system calls. Output is unchanged.

### Help lists all supported hook events

`--help` now lists Stop, SubagentStop, PostToolUse on Edit, MultiEdit and Codex
`apply_patch`, and PreToolUse on Write. The `--json` description no longer says
it is for hooks.

### The hook asks git only about the files you wrote

`stanza hook` asks git only about the files an edit or a Stop transcript names, rather than walking the whole worktree, so hooks stay fast in large repos. Results are unchanged.

### Fewer git processes during repository discovery

The CLI and hooks find plain repository roots in process and reuse each directory lookup within an invocation. Worktrees, submodules and uncertain configurations still use git.

### SubagentStop checks only the subagent's own writes

`stanza hook` now uses `agent_transcript_path` for SubagentStop without checking the main session or other agents. Missing, unreadable and empty agent transcripts leave repository files untouched and exit silently. Stop keeps its whole-session scope.

### Joined guards and declarations no longer report unavoidable walls

`wall` no longer reports groups of six or more statements that join rules keep together. Shorter joined groups still count toward clearable walls, and separate walls in the same body still report. Join rules and fixed output stay unchanged.

### Large CLI runs use worker threads

`--check` and `--fix` format large file selections on worker threads. Output and fixed files stay identical to serial runs. Hooks and small selections stay on the main thread.

## 0.3.1

### Faster checks on large repos

`--check` and `--fix` walk each file's syntax tree with less work per node, so a whole repo run finishes sooner. Results are unchanged.

### Brace answers no longer depend on which directories ran first

A lint config too large to read in time is reported as unread whether or not another directory in the same run already read a shared config it extends. Before, running `--check` or `--fix` on several directories at once could settle braces for such a config that a run on its directory alone left unread.

### `--fix` parses each rewritten file once fewer

`--fix` no longer parses a rewritten file a second time when Stanza already parsed that exact text, so fixing a large repo is faster. Fixed text that does not parse is still left untouched.

### Faster checks and fixes across many files

Once a run has parsed about half a megabyte of source, Stanza reads the parser's syntax tree from a shared buffer instead of decoding JSON, in the release binaries and in installs from npm or bunx. Output is unchanged, and short runs such as the agent hooks stay on JSON. A file the faster path cannot handle, such as a very deep expression or one over a million characters, is parsed the old way on its own. `STANZA_RAW_TRANSFER=0` turns the faster path off.

## 0.3.0

### Compressed release binaries

Every GitHub release now carries a `.tar.xz` archive per platform, a quarter to a third of the raw download, listed in SHA256SUMS. The raw binaries stay where they were.

### The hook returns at once when there is nothing to format

`stanza hook` now answers an event it ignores, or an edit that touched no TypeScript or JavaScript file such as `package.json`, without running git or loading the formatter. Those calls now take about as long as `stanza --version`.

## 0.2.0

### Blank line rules apply at module top level

The blank line rules now cover the gaps between top level statements, not just the ones inside blocks. A `let` that the function below it reads joins that function and gets a blank line above it, a multi-line statement gets a blank line after it, and a run of six statements is reported as a wall. `export const` and `export function` count as the declaration they export. Imports and re-exports stay one group, with no blank line forced between them and no wall reported for them. [short-body](https://stanza.ryuu.gg/rules/short-body) skips the top level, so a short file loses a blank line only when another rule asks for it.

## 0.1.2

### JSX element names count as uses of a binding

A component used as a JSX element, such as `<Row />` or `<Icons.Close />`, now counts as a read of `Row` or `Icons`. Its declaration joins the statement that renders it, the same way any other use would. Lowercase intrinsic elements like `<div>` are not treated as bindings.

## 0.1.1

### Generated variants are skipped for every extension

`.gen` files are now skipped for every JavaScript and TypeScript extension (`.gen.ts`, `.gen.tsx`, `.gen.mts`, `.gen.cts`, `.gen.js`, `.gen.jsx`, `.gen.mjs`, `.gen.cjs`), and `.min` files for `.js`, `.mjs` and `.cjs` (#106).
