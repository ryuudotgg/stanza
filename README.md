# Stanza

An opinionated formatter for the spacing inside TypeScript and JavaScript function bodies, the one thing other formatters leave alone, so it runs alongside them without fighting. It edits only blank lines and brace tokens, is deterministic and idempotent, and checks seven hundred files in about half a second.

The style is mine, hence opinionated. A function body reads as a sequence of steps, one blank line between steps, none inside a step. A fetch and the `if (!response.ok)` that guards it are one step. A ten line query and the `return` after it are two. If that is not how you read code, this tool will annoy you.

## Install

With Bun 1.4 or later, use the npm package:

```
bunx @ryuugg/stanza --check src    # run once without installing
bun add -g @ryuugg/stanza          # put stanza on your PATH
```

The package needs Bun even when npm installs it. On a machine without Bun, running it through npm or npx formats nothing and prints one line naming what to install:

```
stanza: needs Bun 1.4 or later (https://bun.sh) or a release binary (https://github.com/ryuudotgg/stanza/releases/latest)
```

### Release binaries

Without Bun, take a binary from the [latest release](https://github.com/ryuudotgg/stanza/releases/latest). Pick the one for your machine:

| Asset                     | Platform                                                  |
| ------------------------- | --------------------------------------------------------- |
| `stanza-darwin-arm64`     | macOS on Apple silicon                                    |
| `stanza-darwin-x64`       | macOS on Intel                                            |
| `stanza-linux-x64`        | Linux x64 with glibc: Debian, Ubuntu, Fedora, most others |
| `stanza-linux-arm64`      | Linux arm64 with glibc                                    |
| `stanza-linux-x64-musl`   | Linux x64 with musl: Alpine and other musl distributions  |
| `stanza-linux-arm64-musl` | Linux arm64 with musl                                     |

Not sure which libc a Linux machine runs? `ldd --version 2>&1 | head -n 1` names musl on a musl system and glibc or GNU libc everywhere else. The musl builds load the C++ runtime, which Alpine leaves out: `apk add libstdc++` first.

There is no Windows binary. On Windows, run stanza inside WSL with the Linux build for your distribution.

Download the asset and `SHA256SUMS`, check the one against the other, then put the binary on your `PATH`:

```
asset=stanza-darwin-arm64
cd "$(mktemp -d)"
curl -fsSLO https://github.com/ryuudotgg/stanza/releases/latest/download/$asset
curl -fsSLO https://github.com/ryuudotgg/stanza/releases/latest/download/SHA256SUMS
grep " $asset\$" SHA256SUMS | shasum -a 256 -c &&
  mkdir -p ~/.local/bin &&
  install -m 755 $asset ~/.local/bin/stanza
```

The check prints `stanza-darwin-arm64: OK`. Anything else means the download is damaged, and the install does not run. On Linux without `shasum`, use `sha256sum -c` in its place. Once `~/.local/bin` is on your `PATH`, `stanza --version` confirms the install.

On macOS, a binary downloaded through a browser instead of curl carries the quarantine flag. The binaries are signed ad hoc, not notarized, so Gatekeeper stops that copy at launch. Clear the flag once:

```
xattr -d com.apple.quarantine ~/.local/bin/stanza
```

To run it as a Claude Code hook, a Codex hook or a git pre-commit hook, see [Hooks](#hooks).

## Why

Agent-written code tends to arrive as one block: a forty line function without a single blank line, the guard for a value three lines away from it. It is correct, and it is painful to read.

Writing the rules into an `AGENTS.md` helps less than you would think. The two agents that wrote the first version of this tool had these exact rules in their prompts. One came back clean. The other came back with eighteen findings, nine of them plain rule violations.

So I turned the rules into a tool and run it as a hook at the end of every agent turn. It fixes what it can decide on its own and reports the rest.

Also, I just wanted to play around with oxc-parser. Everything above is a very elaborate excuse.

## Use

The flags, as `--help` prints them:

```
--fix                  apply every deterministic rule in place
--check                report only, change nothing
--changed              files from `git diff --name-only HEAD` plus untracked files
--hunks                with --changed or --staged, only gaps and blocks touching changed lines
--staged               the staged content of staged files, for pre-commit
--stdin <path>         source on stdin, fixed text on stdout, findings on stderr
--json                 findings as a JSON array, for hooks
--braces               turn on the braces rule even when lint config turns it off or cannot be read
--no-braces            turn off the braces rule, keep the blank line rules
explain <file>:<line>  which rule decides the gap or braced body at that line, and why
hook                   the Stop hook and PreToolUse hook on Write, reads JSON on stdin
--help                 usage, flags and the rule catalog
--version              the version, and for a built binary the commit it was built from
```

### Which files

`stanza --fix <paths...>` and `stanza --check <paths...>` take files and directories. Directories recurse. Inside a git work tree the file list comes from `git ls-files`, so `.gitignore` applies exactly. Put `--` before paths that start with `-`.

Outside git, stanza walks the directory and applies every `.gitignore` in it with git's pattern rules.

Skipped always: `*.d.ts`, `*.d.mts`, `*.d.cts`, `*.gen.ts`, `*.gen.tsx`, `*.generated.*`, `*.min.js`, the directories `node_modules`, `dist`, `build`, `.next`, `out`, `coverage`, `migrations` and `drizzle`, files marked `linguist-generated` in `.gitattributes`, and files whose first ten lines say `@generated`, `DO NOT EDIT` or `automatically generated`.

### Changed files and hunks

```
stanza --fix --changed
stanza --check --changed --hunks
```

`--changed` covers the whole repository whatever the working directory. Before the first commit it takes every tracked and untracked file.

`--hunks` narrows `--changed` to the lines changed against HEAD: a blank line rule applies only where one of the two statements around the gap, or a blank line between them, is on a changed line, and braces come off only a block that holds a changed line or sits next to such a gap. Braces stanza removed do not make a line changed, a deleted line that held only a brace is no change at all, and any other deleted line counts only for the gap it sat in, so a second run changes nothing. Untracked files count as changed throughout. In a repository whose existing code does not follow these rules, `--hunks` is what lets both hooks run: `stanza hook --hunks` and `stanza --check --staged --hunks`.

### Staged checks

```
stanza --check --staged
stanza --check --staged --hunks
```

`--staged` checks what is in the index, not the working tree, picked by the same rules as `--changed`, with `.gitattributes` also read from the index. A staged file whose `filter` differs between the index and the working tree needs git 2.40 or later. Lint config still comes from each file's real path. It only works with `--check`. When findings `--fix` can apply remain, the last line on stderr is the command that fixes those files and stages them again. Files that also have unstaged changes are listed on their own line instead, to fix and restage by hand, so no unstaged work gets staged.

With `--hunks` it narrows to the lines the index changes against HEAD, and before the first commit every staged line counts. A staged file with a `filter` counts as changed throughout, since its index blob is not the text being checked. `--fix` has no staged line scope, so under `--hunks` the files whose scope is narrower than the whole file are listed on stderr to fix by hand and restage with `git add -p` instead of in the command.

### Output and exit codes

For `--fix` and `--check`, output is one finding per line: `path:line:col rule-id message`. `--json` prints the findings as a JSON array instead. Exit 0 when clean, 1 when findings remain, 2 on a usage error that names the problem, when a file failed to parse or its fixes could not be written, or when a git command failed while picking files. A file that fails to parse is reported and left untouched.

### Explain

```
stanza explain src/cli.ts:120
```

`stanza explain <file>:<line>` explains the gap that ends at that line and the braced body that starts there. For a gap it prints the two statements, the rule that decided, what it found (the name a declaration binds and the statement that reads it, a guard, a statement spanning several lines), the rules it outranked and what `--fix` would do. For a single statement body it says whether `--fix` removes the braces or why they stay: code after the body could continue the statement, a comment sits inside the braces, `--no-braces`, or the lint config file that enforces braces. It takes only `--no-braces`, and explains a file even when `--fix` and `--check` would skip it. Exit 0 when something was explained, 1 when nothing ends or starts on that line, 2 on a usage error or a file that cannot be read or parsed.

### Editors and stdin

```
stanza --fix --stdin <path>
stanza --check --stdin <path>
```

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

## Rules

Scope: statement lists inside blocks. Function bodies, arrow block bodies, methods, `if`, loop and `try` blocks, and switch clause bodies. The blank line rules stop at module top level. The braces rule does not: a single statement control flow body at top level loses its braces too.

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

`--no-braces` turns the braces rule off and keeps the blank line rules. `--braces` turns it on even where lint config turns it off or stanza cannot read the config. When stanza cannot tell whether a config enforces braces, it keeps them and prints one line on stderr per run naming the config file, except under `--fix --stdin --json`, where stderr holds the findings; either flag settles it. `stanza hook` takes `--braces`, `--no-braces` and `--hunks` in its command.

Reported by `--check`, never fixed:

| rule            | what it reports                                                                                                                                                                                           |
| --------------- | --------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `block-spacing` | a multi-line block directly under a statement, when no join rule explains it. Guard chains, parallel `if` runs and the `setBusy(true)` then `try { } finally { setBusy(false) }` bracket are not reported |
| `wall`          | six or more consecutive single-line statements with no blank line                                                                                                                                         |

When a rule gets a case wrong, a directive opts out of it. `// stanza-ignore` on its own line directly above a statement leaves the blank lines above and below that statement as they are, reports nothing about them, and keeps the statement's braces; the code inside it is still formatted. It is a leading comment, so it moves with the statement, and a blank line between the two cancels it. `/* stanza-off */` and `/* stanza-on */` leave everything between them alone, nested blocks included. Pairs nest, so a second `stanza-off` needs its own `stanza-on`. A `stanza-off` with no `stanza-on` in the same block, class body included, runs to the end of that block.

## Hooks

`stanza hook` is the Stop hook for Claude Code and Codex, and the PreToolUse hook on Write for Claude Code. To format each Write before it lands and run a Stop pass at the end of each turn, add both to `.claude/settings.json`:

```
{ "hooks": { "PreToolUse": [{ "matcher": "Write", "hooks": [{ "type": "command", "command": "stanza hook" }] }], "Stop": [{ "hooks": [{ "type": "command", "command": "stanza hook" }] }] } }
```

The Stop hook reads the hook's JSON from stdin and runs one `--fix` pass over changed files in the repository at its `cwd` that the agent created or edited with Write, Edit or MultiEdit according to the transcript at `transcript_path`, so files a human left dirty stay untouched. Without a readable Claude Code transcript (Codex, or no `transcript_path`), it takes every changed file, as `--changed` does. When findings remain that `--fix` cannot apply, it prints a JSON block decision whose reason lists them with a line per rule. It prints nothing when the files come out clean, when `stop_hook_active` is true, when `AGENT_HOOKS=0`, or when `cwd` is outside a git repository. If a file it rewrote still has a finding, the reason says to read that file again before editing it. A file whose fixes it could not write is listed with the error, and the rest of the pass still runs.

The input is a JSON object. `hook_event_name` picks the mode: absent, `Stop` or `SubagentStop` runs the Stop pass, `PreToolUse` formats a Write, and any other event prints nothing. `cwd` defaults to the working directory, `stop_hook_active` to false, and `transcript_path` is optional but must be a string. Other fields are ignored. Bad input, a flag other than `--braces`, `--no-braces` or `--hunks`, both braces flags together, or a git failure while picking files prints a message on stderr and exits 1, which Claude Code shows as a notice without blocking. It never exits 2, because a Stop hook that exits 2 blocks the agent.

The Claude Code PreToolUse hook formats Write content before the file lands. Edit and MultiEdit are left to the Stop hook. It prints nothing when nothing changes, for non TS/JS paths, excluded or generated files, content that does not parse, paths outside the repository at `cwd`, or a file tracked in HEAD when `--hunks` is set. `--braces`, `--no-braces` and lint config apply as for any file. When it changes content, `additionalContext` tells the agent to read the formatted file before editing it. A git failure prints a message on stderr and exits 1, which Claude Code shows as a notice while the Write goes ahead unformatted. It never sets a permission decision, so the user's prompt is unchanged, and it never exits 2, which would deny the Write. Format on write works only in Claude Code.

To check what you are about to commit, add it to `.git/hooks/pre-commit` and make that file executable:

```sh
#!/bin/sh
exec stanza --check --staged
```

In a repository whose existing code does not follow these rules, add `--hunks` to both commands: `stanza hook --hunks` in the registration and `exec stanza --check --staged --hunks` in the pre-commit hook.

## Developing

Run from source with `bun run src/cli.ts`, or build the binary:

```
bun run build                          # bin/stanza, for this machine
bun run build --platform all           # bin/stanza-<platform>, every platform
bun run check                          # oxlint and oxfmt
bun run check:fix                      # apply their fixes
bun run typecheck
bun run self-check                     # the tool on its own source
bun run test                           # every test file, in parallel workers
```

`bun run build` re-signs the binary with `codesign -s -` because Bun 1.4.0 on macOS writes an invalid signature into the compiled executable, and the kernel kills it with SIGKILL before it runs. Each platform has an entry in `src/compile/<platform>.ts` that embeds its oxc addon. `bun run build` builds the one matching this machine, musl or glibc on Linux, to `bin/stanza`, and adding a platform means adding one file there. `--platform` takes `darwin-arm64`, `darwin-x64`, `linux-x64`, `linux-arm64`, `linux-x64-musl`, `linux-arm64-musl` or `all`, may be repeated, writes `bin/stanza-<platform>`, installs every platform's oxc addon first, and re-signs the macOS binaries. `--outdir <dir>` writes the binaries somewhere other than `bin`.

### Invariants

The fixture tests under `tests/fixtures` check, for every before and after pair: only blank lines and brace tokens change, fixing twice equals fixing once, and `--check` on the after file reports only the report-only rules. `bun scripts/compare-commit.ts` exports a base commit of a repo, runs `--fix` over it and diffs the result against a target commit where the same style was applied by hand (`STANZA_REPO`, `STANZA_BASE`, `STANZA_TARGET`). `bun scripts/bench.ts` times the binary on one file, fifty files and a whole repo export (`STANZA_REPO`).

### Hook launchers

The checkout carries two launchers for running the hooks from here instead of from an installed stanza.

`hook.sh` launches `stanza hook` from this checkout and forwards `STANZA_FLAGS`, so `STANZA_FLAGS=--braces` or `STANZA_FLAGS=--hunks` sets the hook's flags through the environment. It turns an exit 2 into 1, so a `bin/stanza` built before `hook` existed shows a notice instead of blocking; rebuild it with `bun run build`. A `bin/stanza` built before the PreToolUse hook existed treats every event as Stop, so rebuild it before `hook.sh` serves the PreToolUse entry.

`git-hooks/pre-commit` is an optional global pre-commit hook for `core.hooksPath`. It chains to the repo's own `pre-commit` hook first, from the common git directory so linked worktrees run it too, then runs `stanza --check --staged` with `STANZA_FLAGS`. A `bin/stanza` that predates `--staged`, or `--staged --hunks`, blocks the commit with its usage message; rebuild it with `bun run build`.

Both launchers pick what to run through `launch.sh`: stanza from this checkout's `src` when `bun` is on the hook's `PATH` and `bun install` has run here, so an edit takes effect on the next run without a rebuild, otherwise `bin/stanza`. If neither is available, they print a message on stderr and exit 1. An installed stanza registered directly as `stanza hook` reads no `STANZA_FLAGS`; there the flags go in the command.

## Authors

- Ryuu ([@ryuudotgg](https://github.com/ryuudotgg))

## License

This project is licensed under the MIT License, see [LICENSE.md](LICENSE.md) for details.
