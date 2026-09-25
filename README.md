# Stanza

An opinionated formatter for the spacing inside TypeScript and JavaScript function bodies, the one thing other formatters leave alone, so it runs alongside them without fighting. It edits only blank lines and brace tokens, is deterministic and idempotent, and checks seven hundred files in about half a second.

The style is mine, hence opinionated. A function body reads as a sequence of steps, one blank line between steps, none inside a step. A fetch and the `if (!response.ok)` that guards it are one step. A ten line query and the `return` after it are two. If that is not how you read code, this tool will annoy you.

## Why

Agent-written code tends to arrive as one block: a forty line function without a single blank line, the guard for a value three lines away from it. It is correct, and it is painful to read.

Writing the rules into an `AGENTS.md` helps less than you would think. The two agents that wrote the first version of this tool had these exact rules in their prompts. One came back clean. The other came back with eighteen findings, nine of them plain rule violations.

So I turned the rules into a tool and run it as a hook at the end of every agent turn. It fixes what it can decide on its own and reports the rest.

Also, I just wanted to play around with oxc-parser. Everything above is a very elaborate excuse.

## Use

```
stanza --fix <paths...>      apply every deterministic rule in place
stanza --check <paths...>    report only, change nothing
stanza --fix --changed       files from `git diff --name-only HEAD` plus untracked files
stanza --check --changed
--json                       findings as a JSON array, for hooks
--no-braces                  turn off the braces rule, keep the blank line rules
```

Directories recurse. Inside a git work tree the file list comes from `git ls-files`, so `.gitignore` applies exactly. Skipped always: `*.d.ts`, `*.gen.ts`, `*.generated.*`, `*.min.js`, the directories `node_modules`, `dist`, `build`, `.next`, `out`, `coverage`, `migrations` and `drizzle`, files marked `linguist-generated` in `.gitattributes`, and files whose first ten lines say `@generated`, `DO NOT EDIT` or `automatically generated`.

Output is one finding per line: `path:line:col rule-id message`. Exit 0 when clean, 1 when findings remain, 2 on a usage error or when a file failed to parse. A file that fails to parse is reported and left untouched.

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

| rule              | what it does                                                                                                                                                                                                         |
| ----------------- | -------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `after-multiline` | a statement that spans several lines is followed by a blank line                                                                                                                                                     |
| `switch-clauses`  | one blank line between switch clauses; a fall through label with an empty body stays directly above the next label; a blank line between clauses is never removed                                                    |
| `edge-blank`      | no blank line right after `{` or right before `}`                                                                                                                                                                    |
| `guard-join`      | a single-line declaration or assignment is followed directly by an `if` that references what it binds, whatever the body size and with or without `else`                                                             |
| `consume-join`    | a single-line declaration is followed directly by a `return` or `switch` that references what it binds                                                                                                               |
| `use-join`        | a single-line declaration is followed directly by a loop, `try` or function declaration that references what it binds                                                                                                |
| `guard-chain`     | consecutive single-line guards (`if` with a one statement body and no `else`) have no blank line between them                                                                                                        |
| `let-step`        | a `let` that joins the block below it gets a blank line above it, so it starts its own step                                                                                                                          |
| `after-guard`     | a single-line guard that returns, throws, continues or breaks is followed by a blank line, unless the next statement is another `if` or a return                                                                     |
| `short-body`      | a function body of two or three single-line statements has no blank lines. A braceless `if` or loop whose header and body each sit on one line counts as single-line here, because the formatter puts it on one line |
| `braces`          | a control flow body that is a single statement has no braces                                                                                                                                                         |

A multi-line declaration never joins: `after-multiline` wins. A name that appears only inside a nested function body does not count as a guard or return consuming it. Comments stay attached to the statement below them, so an inserted blank line goes above the leading comments.

The braces rule keeps braces where removing them would change parsing (a dangling `else`, a declaration as the body, a statement without a trailing `;` that the next line could continue), where the block holds a comment, and in a repo that enforces braces through Biome `useBlockStatements`, ESLint `curly` or Oxlint `curly`. Line breaking is left to the formatter.

`--no-braces` turns the braces rule off and keeps the blank line rules. Both hooks append the contents of `STANZA_FLAGS` to their stanza calls, so `STANZA_FLAGS=--no-braces` opts a repo out through the environment.

Reported by `--check`, never fixed:

| rule            | what it reports                                                                                                                                                                                           |
| --------------- | --------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `block-spacing` | a multi-line block directly under a statement, when no join rule explains it. Guard chains, parallel `if` runs and the `setBusy(true)` then `try { } finally { setBusy(false) }` bracket are not reported |
| `wall`          | six or more consecutive single-line statements with no blank line                                                                                                                                         |

## Invariants

The fixture tests under `tests/fixtures` check, for every before and after pair: only blank lines and brace tokens change, fixing twice equals fixing once, and `--check` on the after file reports only the report-only rules. `bun scripts/compare-commit.ts` exports a base commit of a repo, runs `--fix` over it and diffs the result against a target commit where the same style was applied by hand (`STANZA_REPO`, `STANZA_BASE`, `STANZA_TARGET`). `bun scripts/bench.ts` times the binary on one file, fifty files and a whole repo export (`STANZA_REPO`).

## Hooks

`hook.sh` is a Stop hook for Claude Code and Codex. It runs `--fix --changed` and then `--check --changed` in the agent's working directory and blocks the reply with the remaining findings, as a JSON block decision. It respects `AGENT_HOOKS=0`. `git-hooks/pre-commit` is an optional global pre-commit hook for `core.hooksPath`. It checks out the staged blobs into a temporary directory, runs `--check` there with `STANZA_CONFIG_ROOT` set to the repo root, so each staged file is judged by the lint config at its real path, including nested configs and packages under `node_modules`, and chains to the repo's own `.git/hooks/pre-commit` first. `STANZA_CONFIG_ROOT` is plumbing for this hook: when set, the CLI resolves lint config for a file under the working directory at the same relative path under that root. When unset, nothing changes.

## 👥 Authors

- Ryuu ([@ryuudotgg](https://github.com/ryuudotgg))

## License

This project is licensed under the MIT License - see [LICENSE.md](LICENSE.md) for details.
