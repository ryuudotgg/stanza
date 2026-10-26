# Contributing

## Setup

This repository is a Bun workspace with `docs/` as a member. From the root, install dependencies and run the CLI from source:

```
bun install
bun run src/cli.ts --check src
```

## Build

From the root, build for the current machine or select targets:

```
bun run build
bun run build --platform all
bun run build --platform darwin-arm64 --platform linux-x64 --outdir dist
```

Without `--platform`, the build writes `bin/stanza` for this machine. On Linux, it detects musl or glibc. `--platform` accepts `darwin-arm64`, `darwin-x64`, `linux-x64`, `linux-arm64`, `linux-x64-musl`, `linux-arm64-musl`, and `all`. You can repeat the option. An explicit platform selection installs the oxc addons for every platform first and writes `bin/stanza-<platform>` for each selected target. `--outdir <dir>` changes the output directory from `bin`.

Each entry in `src/compile/<platform>.ts` embeds that platform's oxc addon. To add a platform, add one file there. On macOS, `bun run build` re-signs macOS binaries with `codesign -s -`. Bun 1.4.0 writes an invalid signature into compiled executables on macOS, so the kernel kills them with SIGKILL before they run. A macOS binary built on another host is left unsigned and must be signed on a Mac.

## Checks

From the root:

```
bun run check
bun run check:fix
bun run typecheck
bun run self-check
bun run test
```

`check` runs oxlint and oxfmt. `check:fix` applies their fixes. `typecheck` runs TypeScript without emitting files. `self-check` runs Stanza on its own source. `test` runs every test file in parallel workers.

CI runs `check`, `typecheck`, and `self-check` on Ubuntu. It runs `test` on Ubuntu and macOS.

## Fixtures

`tests/fixtures` has a directory for each rule id, plus directories for cases such as `braces-enforced`, `ignore`, `reference-precision`, and `untouched`. Each case has a `.before.ts` or `.before.js` file and a matching `.after` file. Report only cases can also have a `.findings` file.

`tests/fixtures.test.ts` checks every before and after pair. A fix must match the after file, change only blank lines and brace tokens, and produce the same result when run twice. The after file must keep the program's shape. `--check` on that file must report only the report only rules listed in `.findings`, if any.

Docs pages include example files from `tests/fixtures`. For a docs example, add a fixture pair and include its files in the page instead of pasting the code.

## Bench and Corpus

Build before running the benchmark:

```
bun run build
STANZA_REPO=/path/to/repo bun run bench
```

`bun run bench` runs `bun scripts/bench.ts`. It times `bin/stanza` on one tiny file, fifty files from the named repo, and an export of the whole repo at HEAD. `STANZA_REPO` names that repo. `RUNS` sets the number of runs, with a default of 10. `STANZA` names the binary to time instead of `bin/stanza`.

It then times `stanza hook` events against the export and counts the git processes each one spawns. `STANZA_BASE` names a second binary to time against, run by run in turn with the first. `SPAWNS=1` prints the arguments of every git call.

Run the engine over real code with:

```
bun scripts/corpus.ts [--snapshot <file> | --against <file>] [--include-generated] [--braces] <dir>...
```

The corpus run reports files that break each invariant: idempotence, preservation, program shape, fixable left, agreement, directives, full hunk, empty hunk, and crash. It also reports parse failures. Generated files are skipped unless you pass `--include-generated`. `--braces` enables brace removal for every file.

Runs with at least 256 files use up to eight workers, capped at one fewer than the available CPUs. The main thread also judges files. Set `STANZA_WORKERS=0` to run serially, or set another count to override the default. Snapshots and summary order are the same in either mode.

`--snapshot <file>` writes a record of hashes for formatted output, findings, and explanations. Put the record outside the scanned directories. `--against <file>` compares a later run with that record and reports added or missing paths and differences in those three hashes.

## Docs Site

`docs/` is a Fumadocs app built with Next.js. Pages live under `docs/content/docs`. The site is served at `stanza.ryuu.gg` and deployed by Vercel, with a preview for each pull request.

From the repo root, start the dev server on port 3000:

```
bun run --cwd docs dev
```

Inside `docs/`, build the site with:

```
bun run build
```

The docs build runs `docs/scripts/validate.ts`, which checks links and requires every page in the sidebar. Root tests check that docs facts about CLI flags, rule summaries, release assets, and hook registrations match the code. Change the code and page together. No CI job builds the docs site.

## Releases

### Notes

A pull request that changes something a user of the CLI, the hook or the npm package would notice adds one Markdown note in `.tegami/`. Tests, CI, the docs site and refactors need no note.

Name the file `.tegami/<date>-<topic>.md`, with the date the note was written as `YYYY-MM-DD` and the topic in lowercase words joined by hyphens, such as `.tegami/2026-10-02-blank-line-fix.md`. The date keeps a release's entries in the order they were written.

A note names the package and its bump, then gives each change a `###` heading and one paragraph:

```md
---
packages:
  "@ryuugg/stanza": patch
---

### 🐛 Blank lines inside template literals are left alone

`--fix` no longer removes blank lines that sit inside a multiline template literal.
```

Start each heading with the emoji of its category, a space, then the title:

| Emoji | Category | Use it for                                                                |
| ----- | -------- | ------------------------------------------------------------------------- |
| 🚨    | Heads up | A change users must act on, such as a breaking change or a removed option |
| ✨    | New      | A new capability                                                          |
| 🎨    | Improved | Existing behaviour that works better or faster                            |
| 🐛    | Fixed    | A bug fix                                                                 |

There is no internal category, because a change no user would notice takes no note.

The title says what changed for the user in a few words, in sentence case with no trailing period. Name the outcome, not the work: "Explain counts lines like an editor", not "Fix off by one in explain".

The body is one plain paragraph in the present tense, saying what the user notices now, and what happened before when that helps. Put commands, flags, rules and file names in backticks. Leave out "This PR" and the pull request number, since the changelog links the pull request itself.

A note holds no other heading, list, table, code block or second paragraph. Give each change its own heading: a pull request with two unrelated changes has two headings, in one note or in two.

### What a release looks like

Tegami turns the pending notes into a section at the top of `CHANGELOG.md`. The section carries the date, then the entries grouped under the categories in the order of the table above, leaving out a category with no entry:

```md
## 0.4.0

_October 26, 2026_

### 🎨 Improved

- **Release binaries start faster** ([#146](https://github.com/ryuudotgg/stanza/pull/146))\
  The binaries on the GitHub release ship precompiled bytecode, so each run starts a few milliseconds sooner.

### 🐛 Fixed

- **Explain counts lines like an editor** ([#158](https://github.com/ryuudotgg/stanza/pull/158))\
  `stanza explain` treats a final newline as the end of the last line rather than the start of another.
```

Each entry links the pull request that added its note, read from the `(#158)` a squash merge puts at the end of its commit title. A note that reached `main` another way links its commit instead. The Publish workflow copies the section into the GitHub Release, so both read the same.

### Checking notes

```
bun run tegami version --dry-run
```

This checks every note in `.tegami/` and prints the section the Release workflow would write, changing no file. The version run makes the same checks and refuses, naming the note, a file name of another shape, a note with no frontmatter or no heading, text above the first heading, a heading other than `###`, a heading without a category or a title, or a body that is not one plain paragraph. Without that check, Tegami would drop a note with no frontmatter or no heading and say nothing.

### Bumps

Under 1.0, the bump follows the change, not the pull request's Conventional Commit type. A new capability or a breaking change is a minor. Extending or correcting existing behaviour is a patch.

#106 is a `feat:` pull request, but it extended which generated files are skipped, so its note is a patch.
