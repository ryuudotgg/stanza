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

`bun run bench` runs `bun scripts/bench.ts`. It times `bin/stanza` on one tiny file, fifty files from the named repo, and an export of the whole repo at HEAD. `STANZA_REPO` names that repo. `RUNS` sets the number of runs, with a default of 10.

Run the engine over real code with:

```
bun scripts/corpus.ts [--snapshot <file> | --against <file>] [--include-generated] [--braces] <dir>...
```

The corpus run reports files that break each invariant: idempotence, preservation, program shape, fixable left, agreement, directives, full hunk, empty hunk, and crash. It also reports parse failures. Generated files are skipped unless you pass `--include-generated`. `--braces` enables brace removal for every file.

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

Name the file anything, such as `.tegami/2026-10-02-blank-line-fix.md`. A note names the package and its bump, then says what changed for the user:

```
---
packages:
  "@ryuugg/stanza": patch
---

### Blank lines inside template literals are left alone

`--fix` no longer removes blank lines that sit inside a multiline template literal.
```

Tegami turns pending notes into a section in `CHANGELOG.md` at the repo root. The Publish workflow uses that section for the GitHub Release notes.

### Bumps

Under 1.0, the bump follows the change, not the pull request's Conventional Commit type. A new capability or a breaking change is a minor. Extending or correcting existing behaviour is a patch.

#106 is a `feat:` pull request, but it extended which generated files are skipped, so its note is a patch.

### Release path

Run the Release workflow from the Actions tab on `main`. Anyone with write access can run it. It versions pending notes, bumps `package.json`, writes the `CHANGELOG.md` section and writes `.tegami/publish-lock.yaml`. It opens or updates the version PR `chore: release <version>` from `tegami/version-packages` and requests review from ryuudotgg. It never publishes and fails if there are no pending notes.

The version PR uses the workflow token. Its CI runs wait until someone with write access clicks "Approve workflows to run" in the merge box.

Merging the version PR into `main` starts the Publish workflow. When the push adds or modifies the publish lock, it builds the six binaries and `SHA256SUMS`. It smoke tests the darwin-arm64 and linux-x64 binaries and the npm package, then waits for ryuudotgg to approve the publish job in the `npm` environment. Every publish needs that approval.

After approval, Publish publishes to npm with provenance, pushes the `v<version>` tag and creates the GitHub Release with the six binaries, `SHA256SUMS` and the changelog section.

If publishing fails after the merge, rerun the failed jobs of that run. A manual Publish run proceeds only on `main`, with a publish lock present, when HEAD is the last commit that changed that lock. If `main` has moved past that commit, the manual run fails. Without the lock, or on another branch, it skips publishing.

Pushing a tag publishes nothing. The Publish workflow creates the tag.
