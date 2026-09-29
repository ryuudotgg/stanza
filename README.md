# Stanza

An opinionated formatter for the spacing inside TypeScript and JavaScript function bodies, the one thing other formatters leave alone, so it runs alongside them without fighting. It edits only blank lines and brace tokens, is deterministic and idempotent, and checks seven hundred files in about half a second.

The style is mine, hence opinionated. A function body reads as a sequence of steps, one blank line between steps, none inside a step. A fetch and the `if (!response.ok)` that guards it are one step. A ten line query and the `return` after it are two. If that is not how you read code, this tool will annoy you.

Before and after `stanza --fix`:

```ts
function publish(draft: Draft) {
  if (!draft.title) throw new Error("title required");
  const slug = slugify(draft.title);
  if (exists(slug)) throw new Error(`${slug} is taken`);
  save(draft, slug);
  notify(draft.author);
}
```

```ts
function publish(draft: Draft) {
  if (!draft.title) throw new Error("title required");

  const slug = slugify(draft.title);
  if (exists(slug)) throw new Error(`${slug} is taken`);

  save(draft, slug);
  notify(draft.author);
}
```

## Install

Stanza needs Bun 1.4 or later, even when npm installs it.

```
bunx @ryuugg/stanza --check src    # run once without installing
bun add -g @ryuugg/stanza          # put stanza on your PATH
```

<details>
<summary>Release binaries, for machines without Bun</summary>

Take the binary for your machine from the [latest release](https://github.com/ryuudotgg/stanza/releases/latest), check it against `SHA256SUMS`, and put it on your `PATH`. There is no Windows binary: run the Linux build inside WSL. [Release Binaries](https://stanza.ryuu.gg/guides/release-binaries) walks through the download, the checksum and musl.

| Asset                     | Platform                                                  |
| ------------------------- | --------------------------------------------------------- |
| `stanza-darwin-arm64`     | macOS on Apple silicon                                    |
| `stanza-darwin-x64`       | macOS on Intel                                            |
| `stanza-linux-x64`        | Linux x64 with glibc: Debian, Ubuntu, Fedora, most others |
| `stanza-linux-arm64`      | Linux arm64 with glibc                                    |
| `stanza-linux-x64-musl`   | Linux x64 with musl: Alpine and other musl distributions  |
| `stanza-linux-arm64-musl` | Linux arm64 with musl                                     |

</details>

> [!WARNING]
> The macOS binaries are signed ad hoc, not notarized, so Gatekeeper stops a copy downloaded through a browser. Clear the flag once with `xattr -d com.apple.quarantine ~/.local/bin/stanza`, or download with curl.

## Usage

```
stanza --check src               # report findings, change nothing
stanza --fix src                 # apply every rule it can decide on its own
stanza --fix --changed           # only the files changed against HEAD
stanza explain src/cli.ts:120    # which rule decides the gap at that line
stanza --help                    # every flag and the rule catalog
```

Exit 0 when clean, 1 when findings remain, 2 on a usage error, a file that failed to parse or could not be written, or a failed git command. The [CLI reference](https://stanza.ryuu.gg/reference/cli) covers every flag.

## Hooks

To format each Write before it lands and run a Stop pass at the end of each turn in Claude Code, add both hooks to `.claude/settings.json`:

```
{ "hooks": { "PreToolUse": [{ "matcher": "Write", "hooks": [{ "type": "command", "command": "stanza hook" }] }], "Stop": [{ "hooks": [{ "type": "command", "command": "stanza hook" }] }] } }
```

To check what you are about to commit, add this to `.git/hooks/pre-commit` and make the file executable:

```sh
#!/bin/sh
exec stanza --check --staged
```

> [!IMPORTANT]
> In a repository whose existing code does not follow these rules, add `--hunks` to both: `stanza hook --hunks` and `exec stanza --check --staged --hunks`. See [Existing Codebases](https://stanza.ryuu.gg/guides/existing-codebases).

## Why

Agent-written code tends to arrive as one block: a forty line function without a single blank line, the guard for a value three lines away from it. It is correct, and it is painful to read.

Writing the rules into an `AGENTS.md` helps less than you would think. The two agents that wrote the first version of this tool had these exact rules in their prompts. One came back clean. The other came back with eighteen findings, nine of them plain rule violations.

So I turned the rules into a tool and run it as a hook at the end of every agent turn. It fixes what it can decide on its own and reports the rest.

Also, I just wanted to play around with oxc-parser. Everything above is a very elaborate excuse.

## Documentation

The full docs live at [stanza.ryuu.gg](https://stanza.ryuu.gg): [Getting Started](https://stanza.ryuu.gg/getting-started), the [rules](https://stanza.ryuu.gg/rules) and the [directives](https://stanza.ryuu.gg/rules/directives) that opt out of them, the [CLI reference](https://stanza.ryuu.gg/reference/cli), guides for the [pre-commit hook](https://stanza.ryuu.gg/guides/pre-commit), [editors](https://stanza.ryuu.gg/guides/editors) and [existing codebases](https://stanza.ryuu.gg/guides/existing-codebases), and setup for [Claude Code](https://stanza.ryuu.gg/agents/claude-code) and [Codex](https://stanza.ryuu.gg/agents/codex). To build from source and run the checks, see [CONTRIBUTING.md](CONTRIBUTING.md).

## Authors

- Ryuu ([@ryuudotgg](https://github.com/ryuudotgg))

## License

This project is licensed under the MIT License, see [LICENSE.md](LICENSE.md) for details.
