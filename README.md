<h1 align="center">Stanza</h1>

<p align="center">
  An opinionated formatter for turning function bodies into readable steps.
</p>

<p align="center">
  <a href="https://stanza.ryuu.gg">Documentation</a>
  ·
  <a href="https://github.com/ryuudotgg/stanza/issues">Issues</a>
</p>

<p align="center">
  <a href="LICENSE.md"><img src="https://img.shields.io/github/license/ryuudotgg/stanza?style=for-the-badge&labelColor=000000" alt="MIT License"></a>
  <a href="https://github.com/ryuudotgg/stanza/actions/workflows/ci.yml"><img src="https://img.shields.io/github/actions/workflow/status/ryuudotgg/stanza/ci.yml?branch=main&style=for-the-badge&labelColor=000000&label=CI" alt="CI"></a>
</p>

## ✨ What is Stanza?

Stanza formats the spacing inside TypeScript and JavaScript function bodies, the one thing other formatters leave alone, so it runs alongside them without fighting. It edits only blank lines and brace tokens, is deterministic and idempotent, and checks seven hundred files in about half a second.

The style is mine, hence opinionated. A function body reads as a sequence of steps, one blank line between steps, none inside a step. A fetch and the `if (!response.ok)` that guards it are one step. A ten line query and the `return` after it are two. If that is not how you read code, this tool will annoy you.

Before and after `stanza --fix`:

```diff
 function publish(draft: Draft) {
   if (!draft.title) throw new Error("title required");
+
   const slug = slugify(draft.title);
   if (exists(slug)) throw new Error(`${slug} is taken`);
+
   save(draft, slug);
   notify(draft.author);
 }
```

## 🚀 Getting Started

Stanza needs Bun 1.4 or later, even when npm installs it.

```bash
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

> **Warning:** The macOS binaries are signed ad hoc, not notarized, so Gatekeeper stops a copy downloaded through a browser. Clear the flag once with `xattr -d com.apple.quarantine ~/.local/bin/stanza`, or download with curl.

</details>

### Commands

| Command                        | What it does                                              |
| ------------------------------ | --------------------------------------------------------- |
| `stanza --check <paths>`       | Report findings and change nothing.                       |
| `stanza --fix <paths>`         | Apply every rule it can decide on its own.                |
| `stanza --fix --changed`       | Fix the files changed against HEAD, plus untracked files. |
| `stanza --check --staged`      | Check what is staged, for a pre-commit hook.              |
| `stanza explain <file>:<line>` | Say which rule decides the gap at that line, and why.     |
| `stanza hook`                  | Run as a Claude Code or Codex hook.                       |

### Hooks

To format each Write before it lands, fix each Edit and MultiEdit as soon as it lands, and run a Stop pass at the end of each turn in Claude Code, add the three hooks to `.claude/settings.json`:

```json
{
  "hooks": {
    "PreToolUse": [
      {
        "matcher": "Write",
        "hooks": [
          {
            "type": "command",
            "command": "stanza hook"
          }
        ]
      }
    ],
    "PostToolUse": [
      {
        "matcher": "Edit|MultiEdit",
        "hooks": [
          {
            "type": "command",
            "command": "stanza hook"
          }
        ]
      }
    ],
    "Stop": [
      {
        "hooks": [
          {
            "type": "command",
            "command": "stanza hook"
          }
        ]
      }
    ]
  }
}
```

To check what you are about to commit, add this to `.git/hooks/pre-commit` and make the file executable:

```sh
#!/bin/sh
exec stanza --check --staged
```

> [!IMPORTANT]
> In a repository whose existing code does not follow these rules, add `--hunks` to each: `stanza hook --hunks` and `exec stanza --check --staged --hunks`. See [Existing Codebases](https://stanza.ryuu.gg/guides/existing-codebases).

## 💭 Why

Agent-written code tends to arrive as one block: a forty line function without a single blank line, the guard for a value three lines away from it. It is correct, and it is painful to read.

Writing the rules into an `AGENTS.md` helps less than you would think. The two agents that wrote the first version of this tool had these exact rules in their prompts. One came back clean. The other came back with eighteen findings, nine of them plain rule violations.

So I turned the rules into a tool and run it as a hook at the end of every agent turn. It fixes what it can decide on its own and reports the rest.

Also, I just wanted to play around with oxc-parser. Everything above is a very elaborate excuse.

## 📚 Documentation

Read the full docs at [stanza.ryuu.gg](https://stanza.ryuu.gg).

- [Getting Started](https://stanza.ryuu.gg/getting-started)
- [Rules](https://stanza.ryuu.gg/rules)
- [Directives](https://stanza.ryuu.gg/rules/directives)
- [CLI Reference](https://stanza.ryuu.gg/reference/cli)
- [Output and Exit Codes](https://stanza.ryuu.gg/reference/output)
- [Pre-Commit Hook](https://stanza.ryuu.gg/guides/pre-commit)
- [Editors](https://stanza.ryuu.gg/guides/editors)
- [Claude Code](https://stanza.ryuu.gg/agents/claude-code)
- [Codex](https://stanza.ryuu.gg/agents/codex)

## 🤝 Contributing

To build from source and run the checks, see [CONTRIBUTING.md](CONTRIBUTING.md). Report bugs on [GitHub Issues](https://github.com/ryuudotgg/stanza/issues).

## 👥 Authors

- Ryuu ([@ryuudotgg](https://github.com/ryuudotgg))

## 📄 License

This project is licensed under the MIT License, see [LICENSE.md](LICENSE.md) for details.
