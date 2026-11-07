## 0.4.1

_November 7, 2026_

### 🎨 Improved

- **Smaller binaries** ([#197](https://github.com/ryuudotgg/stanza/pull/197))\
  The binaries are about a fifth smaller, since release builds now strip their symbol tables.

### 🐛 Fixed

- **The Linux glibc builds run on glibc 2.17 and newer** ([#197](https://github.com/ryuudotgg/stanza/pull/197))\
  `stanza-linux-x64` and `stanza-linux-arm64`, from npm or the release archives, now start on any distribution with glibc 2.17 or newer, CentOS 7 included. Before, the arm64 build stopped with `GLIBC_2.39 not found` on Debian 12, Ubuntu 22.04 and every other release older than glibc 2.39, and the x64 build needed glibc 2.34.

## 0.4.0

_November 7, 2026_

### 🚨 Heads up

- **Stanza no longer needs Bun** ([#194](https://github.com/ryuudotgg/stanza/pull/194))\
  `@ryuugg/stanza` now runs a native binary, so `npm i -g @ryuugg/stanza` or `npx @ryuugg/stanza` works with only Node.js installed, and `bun add -g` keeps working. The install pulls one `@ryuugg/stanza-<platform>` package for your os, cpu and libc, the same binary the release archives carry. There is still no Windows binary, so run it inside WSL there. `STANZA_RAW_TRANSFER` no longer does anything.

### ✨ New

- **A git attribute keeps stanza off a file** ([#164](https://github.com/ryuudotgg/stanza/pull/164))\
  Write `path/to/file.ts -stanza` or `path/to/file.ts stanza=false` in `.gitattributes` to keep stanza off a file without editing it. No selection source takes the file: paths, `--changed`, `--staged`, `--stdin` and the hook all skip it. `--check` on it alone says the stanza attribute opted it out, and `stanza explain` names the attribute as the reason both modes skip it.

### 🎨 Improved

- **A body that fits goes onto its header line when its braces go** ([#168](https://github.com/ryuudotgg/stanza/pull/168))\
  When the `braces` rule removes the braces around a body, `--fix` now prints the body on its header line, as `if (done) return;`, when that line fits the formatter width, the way oxfmt and Biome print it. Before, the body stayed on its own line, so the formatter's check failed until it joined the line itself, and the next stanza run changed the blank lines around it again. A body that does not fit, a header that spans lines, or a line comment before the brace keeps the body on its own line.

### 🐛 Fixed

- **A braceless if/else that fits the formatter width counts as compact** ([#166](https://github.com/ryuudotgg/stanza/pull/166))\
  A braceless `if` with an `else`, printed by Biome or oxfmt as `if (ready) send();` over `else close();`, now counts as compact when every arm fits the formatter width. In a body of two or three compact statements, `short-body` keeps it tight with no blank lines, so the formatter's layout no longer gets a `block-spacing` finding that `--fix` cannot settle.
- **Generated headers explain an empty run** ([#169](https://github.com/ryuudotgg/stanza/pull/169))\
  When every file you name is skipped because its header marks it generated, such as `// @generated`, `--check` and `--fix` now print `stanza: nothing to format in <path>, generated files are skipped` on stderr instead of nothing, the same line `.gen.ts` files already get. The exit status is unchanged.
- **Skipped submodules are named on stderr** ([#170](https://github.com/ryuudotgg/stanza/pull/170))\
  A path in a superproject still leaves out the files inside its git submodules, and `--check` and `--fix` now say so with `stanza: skipped git submodule <path>, pass its path to format its files` instead of nothing. `--changed` and `--staged` print the same line for a submodule they skip. A selection holding only a submodule no longer claims git ignores its files, and the exit status is unchanged.
- **Unreadable paths are reported in one voice** ([#171](https://github.com/ryuudotgg/stanza/pull/171))\
  A file `--check` or `--fix` cannot read or write gets a `parse` or `write` finding reading `cannot read: <code>` or `cannot write: <code>`, such as `cannot read: EACCES`, instead of raw error text with an absolute path. A directory or named path stanza cannot reach prints `stanza: cannot read <path>: <code>` and exits 2, including a directory inside a git repository, which was silently skipped with exit 0 before. A symlink loop no longer reads as `no such file`.

## 0.3.3

### Skip unnecessary Git attribute checks

Path runs, `--changed`, `--stdin <path>`, `explain` and hooks skip the Git attribute subprocess when no attribute source can apply to the files, no `attr.*` config is set, and every `GIT_*` variable set is one Stanza knows cannot affect attribute lookup. Anything else, including `GIT_ATTR_SOURCE` or an attributes path Stanza cannot rebuild, still asks Git, and `--staged` always asks Git.

### Explain accepts paths starting with a dash

`stanza explain` now accepts `--` before a location whose path starts with `-`. Flags are parsed only before the first `--`; arguments after it are literal.

### Codex SubagentStop checks the subagent's own writes

A Codex SubagentStop hook now reads the subagent rollout at `agent_transcript_path` and checks the files that subagent patched, as Stop does for a Codex session. Before, it read only Claude Code transcripts, so a Codex subagent's files went unchecked.

### Explain counts lines like an editor

`stanza explain` treats a final newline as the end of the last line rather than the start of another, so a line beyond the end of the file now gets the right count, such as `a.ts has 2 lines, not 3` for a two line file ending in a newline, and an empty file has 0 lines.

### Fewer Git processes when a Git config uses include or includeIf

Finding the repository root no longer starts an extra Git process when a Git config file uses `include` or `includeIf`, so a check in such a setup starts as many Git processes as one without them. A repository whose path ends in a space also works now, where `--check` used to exit 2 with "cannot change to" and the path without its trailing space.

### Lost worker threads are reported and their files still formatted

When a run that uses worker threads loses one, because it fails to start or dies after taking files, Stanza reports the loss with a `stanza:` line on stderr and exits 2, and still prints the same findings and writes the same files as a serial run. Before, a worker that failed to start went unreported and a worker that died after taking files hung the run. Runs that start no worker thread, such as a single file or `STANZA_WORKERS=0`, no longer load the worker machinery, so they start slightly faster.

### Walls with a comment inside a joined group are reported again

`wall` again reports a run of joined statements, such as six guards, when a comment sits directly above one of them, because a blank line between that comment and its statement survives `--fix` and splits the run. Runs that no kept blank line can break stay unreported, and `--fix` output is unchanged.

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
