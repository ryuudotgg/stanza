# Stanza

An opinionated formatter for the spacing inside TypeScript and JavaScript function bodies, the one thing other formatters leave alone, so it runs alongside them without fighting. It edits only blank lines and brace tokens, is deterministic and idempotent, and checks seven hundred files in about half a second.

The style is mine, hence opinionated. A function body reads as a sequence of steps, one blank line between steps, none inside a step. A fetch and the `if (!response.ok)` that guards it are one step. A ten line query and the `return` after it are two. If that is not how you read code, this tool will annoy you.

## Install

The npm package runs under Bun 1.4 or later:

```
bunx @ryuugg/stanza --check src    # run once without installing
bun add -g @ryuugg/stanza          # put stanza on your PATH
```

Without Bun, download the binary for your platform from the [latest release](https://github.com/ryuudotgg/stanza/releases/latest) into a directory on your `PATH`. Assets are named `stanza-<platform>`, with checksums in `SHA256SUMS`:

```
mkdir -p ~/.local/bin
curl -fsSLo ~/.local/bin/stanza https://github.com/ryuudotgg/stanza/releases/latest/download/stanza-darwin-arm64
chmod +x ~/.local/bin/stanza
```

To run it at the end of every Claude Code turn, add the Stop hook to `.claude/settings.json` (see [Hooks](#hooks)):

```json
{ "hooks": { "Stop": [{ "hooks": [{ "type": "command", "command": "stanza hook" }] }] } }
```

To check what you are about to commit, add it to `.git/hooks/pre-commit` and make that file executable:

```sh
#!/bin/sh
exec stanza --check --staged
```

## Why

Agent-written code tends to arrive as one block: a forty line function without a single blank line, the guard for a value three lines away from it. It is correct, and it is painful to read.

Writing the rules into an `AGENTS.md` helps less than you would think. The two agents that wrote the first version of this tool had these exact rules in their prompts. One came back clean. The other came back with eighteen findings, nine of them plain rule violations.

So I turned the rules into a tool and run it as a hook at the end of every agent turn. It fixes what it can decide on its own and reports the rest.

Also, I just wanted to play around with oxc-parser. Everything above is a very elaborate excuse.

## Use

```
stanza --fix <paths...>              apply every deterministic rule in place
stanza --check <paths...>            report only, change nothing
stanza --fix --changed               files from `git diff --name-only HEAD` plus untracked files
stanza --check --changed
stanza --fix --changed --hunks       only gaps and blocks touching changed lines
stanza --check --staged              the staged content of staged files, for pre-commit
stanza --fix --stdin <path>          source on stdin, fixed text on stdout, findings on stderr
stanza --check --stdin <path>
stanza hook [--no-braces] [--hunks]  the Stop hook, reads its JSON on stdin
stanza --help                        usage, flags and the rule catalog
stanza --version                     the version, and for a built binary the commit it was built from
--json                               findings as a JSON array, for hooks
--no-braces                          turn off the braces rule, keep the blank line rules
```

`--changed` covers the whole repository whatever the working directory. Before the first commit it takes every tracked and untracked file.

`--hunks` narrows `--changed` to the lines changed against HEAD: a blank line rule applies only where one of the two statements around the gap, or a blank line between them, is on a changed line, and braces come off only a block that holds a changed line or sits next to such a gap. Untracked files count as changed throughout. It lets the Stop hook run in a repository whose existing code does not follow these rules: `STANZA_FLAGS=--hunks`.

`--staged` checks what is in the index, not the working tree, picked by the same rules as `--changed`, with `.gitattributes` also read from the index. A staged file whose `filter` differs between the index and the working tree needs git 2.40 or later. Lint config still comes from each file's real path. It only works with `--check`. When findings `--fix` can apply remain, the last line on stderr is the command that fixes those files and stages them again. Files that also have unstaged changes are listed on their own line instead, to fix and restage by hand, so no unstaged work gets staged. Put `--` before paths that start with `-`.

Directories recurse. Inside a git work tree the file list comes from `git ls-files`, so `.gitignore` applies exactly. Skipped always: `*.d.ts`, `*.gen.ts`, `*.generated.*`, `*.min.js`, the directories `node_modules`, `dist`, `build`, `.next`, `out`, `coverage`, `migrations` and `drizzle`, files marked `linguist-generated` in `.gitattributes`, and files whose first ten lines say `@generated`, `DO NOT EDIT` or `automatically generated`.

For `--fix` and `--check`, output is one finding per line: `path:line:col rule-id message`. Exit 0 when clean, 1 when findings remain, 2 on a usage error that names the problem, when a file failed to parse or its fixes could not be written, or when a git command failed while picking files. A file that fails to parse is reported and left untouched.

With `--stdin`, the path only names the buffer: it picks the extension, the lint config and the skip rules, and need not exist. To format on save, pipe the buffer through `--fix --stdin` after oxfmt. With conform.nvim:

```lua
require("conform").setup({
  formatters = {
    stanza = { command = "stanza", args = { "--fix", "--stdin", "$FILENAME" }, exit_codes = { 0, 1 } },
  },
  formatters_by_ft = { typescript = { "oxfmt", "stanza" }, typescriptreact = { "oxfmt", "stanza" } },
})
```

Exit 1 means findings remain that `--fix` cannot apply, so the editor has to accept it as success.

Run from source with `bun run src/cli.ts`, or build the binary:

```
bun run build                          # bin/stanza, for this machine
bun run build --platform all           # bin/stanza-<platform>, every platform
bun run check                          # oxlint and oxfmt
bun run check:fix                      # apply their fixes
bun run typecheck
bun run self-check                     # the tool on its own source
bun test
```

`bun run build` re-signs the binary with `codesign -s -` because Bun 1.4.0 on macOS writes an invalid signature into the compiled executable, and the kernel kills it with SIGKILL before it runs. Each platform has an entry in `src/compile/<platform>.ts` that embeds its oxc addon. `bun run build` builds the one matching this machine, musl or glibc on Linux, to `bin/stanza`, and adding a platform means adding one file there. `--platform` takes `darwin-arm64`, `darwin-x64`, `linux-x64`, `linux-arm64`, `linux-x64-musl`, `linux-arm64-musl` or `all`, may be repeated, writes `bin/stanza-<platform>`, installs every platform's oxc addon first, and re-signs the macOS binaries. `--outdir <dir>` writes the binaries somewhere other than `bin`.

## Rules

Scope: statement lists inside blocks. Function bodies, arrow block bodies, methods, `if`, loop and `try` blocks, and switch clause bodies. Module top level is out of scope.

Applied by `--fix`:

| rule              | what it does                                                                                                                                                                                                                                                                        |
| ----------------- | ----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `after-multiline` | a statement that spans several lines is followed by a blank line                                                                                                                                                                                                                    |
| `switch-clauses`  | one blank line between switch clauses; a fall through label with an empty body stays directly above the next label; a blank line between clauses is never removed                                                                                                                   |
| `edge-blank`      | no blank line right after `{` or right before `}`                                                                                                                                                                                                                                   |
| `guard-join`      | a single-line declaration or assignment is followed directly by an `if` that references what it binds, whatever the body size and with or without `else`                                                                                                                            |
| `consume-join`    | a single-line declaration or assignment is followed directly by a `return` or `switch` that references what it binds                                                                                                                                                                |
| `use-join`        | a single-line declaration is followed directly by a loop, `try` or function declaration that references what it binds                                                                                                                                                               |
| `guard-chain`     | consecutive single-line guards (`if` with a one statement body and no `else`) have no blank line between them                                                                                                                                                                       |
| `let-step`        | a `let` that joins the block below it gets a blank line above it, so it starts its own step                                                                                                                                                                                         |
| `after-guard`     | a single-line guard that returns, throws, continues or breaks is followed by a blank line, unless the next statement is an `if` or a jump (`return`, `throw`, `break`, `continue`)                                                                                                  |
| `short-body`      | a block of two or three single-line statements has no blank lines, whether it is a function body, a nested block or a switch clause body. A braceless `if` or loop whose header and body each sit on one line counts as single-line here, because the formatter puts it on one line |
| `braces`          | a control flow body that is a single statement has no braces                                                                                                                                                                                                                        |

A multi-line declaration never joins: `after-multiline` wins. A name that appears only inside a nested function body does not count as a guard or return consuming it. Comments stay attached to the statement below them, so an inserted blank line goes above the leading comments.

The braces rule keeps braces where removing them would change parsing (a dangling `else`, a declaration as the body, a statement without a trailing `;` that the next line could continue), where the block holds a comment, and in a repo that enforces braces through Biome `useBlockStatements`, ESLint `curly` or Oxlint `curly`. Line breaking is left to the formatter.

`--no-braces` turns the braces rule off and keeps the blank line rules. Both hook launchers forward `STANZA_FLAGS` to stanza, so `STANZA_FLAGS=--no-braces` opts a repo out through the environment. The Stop hook takes `--no-braces` and `--hunks`; registered directly as `stanza hook`, it reads no `STANZA_FLAGS`, so put the flags in the command.

Reported by `--check`, never fixed:

| rule            | what it reports                                                                                                                                                                                           |
| --------------- | --------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `block-spacing` | a multi-line block directly under a statement, when no join rule explains it. Guard chains, parallel `if` runs and the `setBusy(true)` then `try { } finally { setBusy(false) }` bracket are not reported |
| `wall`          | six or more consecutive single-line statements with no blank line                                                                                                                                         |

When a rule gets a case wrong, a directive opts out of it. `// stanza-ignore` on its own line directly above a statement leaves the blank lines above and below that statement as they are, reports nothing about them, and keeps the statement's braces; the code inside it is still formatted. It is a leading comment, so it moves with the statement, and a blank line between the two cancels it. `/* stanza-off */` and `/* stanza-on */` leave everything between them alone, nested blocks included. Pairs nest, so a second `stanza-off` needs its own `stanza-on`. A `stanza-off` with no `stanza-on` in the same block, class body included, runs to the end of that block.

## Invariants

The fixture tests under `tests/fixtures` check, for every before and after pair: only blank lines and brace tokens change, fixing twice equals fixing once, and `--check` on the after file reports only the report-only rules. `bun scripts/compare-commit.ts` exports a base commit of a repo, runs `--fix` over it and diffs the result against a target commit where the same style was applied by hand (`STANZA_REPO`, `STANZA_BASE`, `STANZA_TARGET`). `bun scripts/bench.ts` times the binary on one file, fifty files and a whole repo export (`STANZA_REPO`).

## Hooks

`stanza hook` is the Stop hook for Claude Code and Codex. It reads the hook's JSON from stdin and runs one `--fix` pass over changed files in the repository at its `cwd` that the agent created or edited with Write, Edit or MultiEdit according to the transcript at `transcript_path`, so files a human left dirty stay untouched. Without a readable Claude Code transcript (Codex, or no `transcript_path`), it takes every changed file, as `--changed` does. When findings remain that `--fix` cannot apply, it prints a JSON block decision whose reason lists them with a line per rule. It prints nothing when the files come out clean, when `stop_hook_active` is true, when `AGENT_HOOKS=0`, or when `cwd` is outside a git repository. If a file it rewrote still has a finding, the reason says to read that file again before editing it. A file whose fixes it could not write is listed with the error, and the rest of the pass still runs.

The input is a JSON object. `cwd` defaults to the working directory, `stop_hook_active` to false, and `transcript_path` is optional but must be a string. Other fields are ignored. Bad input, a flag other than `--no-braces` or `--hunks`, or a git failure while picking files prints a message on stderr and exits 1, which Claude Code shows as a notice without blocking. It never exits 2, because a Stop hook that exits 2 blocks the agent. [Install](#install) shows how to register it.

`hook.sh` launches `stanza hook` from this checkout and forwards `STANZA_FLAGS`. It turns an exit 2 into 1, so a `bin/stanza` built before `hook` existed shows a notice instead of blocking; rebuild it with `bun run build`.

`git-hooks/pre-commit` is an optional global pre-commit hook for `core.hooksPath`. It chains to the repo's own `.git/hooks/pre-commit` first, then runs `stanza --check --staged` with `STANZA_FLAGS`, minus `--hunks`, which only the Stop hook takes. If the `bin/stanza` it falls back to predates `--staged`, it lets the commit through with a notice to run `bun run build`.

Both launchers run stanza from this checkout's `src` when `bun` is on the hook's `PATH` and `bun install` has run here, so an edit takes effect on the next run without a rebuild. Otherwise they run `bin/stanza`. If neither is available, they print a message on stderr and exit 1.

## 👥 Authors

- Ryuu ([@ryuudotgg](https://github.com/ryuudotgg))

## License

This project is licensed under the MIT License - see [LICENSE.md](LICENSE.md) for details.
