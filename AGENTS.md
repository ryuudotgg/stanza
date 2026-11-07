# AGENTS.md

## Development

Stanza is the Rust crate in `crates/stanza`. The Bun tests and scripts drive its release binaries, so run `cargo build --release` before `bun run test`, `bun run self-check` or `bun scripts/corpus.ts`. [Checks in CONTRIBUTING.md](CONTRIBUTING.md#checks) lists the commands CI runs; run all of them before opening a pull request.

- A behaviour change gets a Rust test in the module it touches, or a case in the Bun test file for its area when it shows through the CLI.
- `self-check` formats the repository's own TypeScript with the binary. Fix what it reports with `target/release/stanza --fix <files>`.
- To show a change leaves output alone, compare against a build of `origin/main` with `bun scripts/parity.ts`, as [Bench, Corpus and Parity](CONTRIBUTING.md#bench-corpus-and-parity) describes.

## Releases

Stanza releases through [Tegami](https://tegami.fuma-nama.dev). When to write a note, its format and which bump to pick are in the [Releases section of CONTRIBUTING.md](CONTRIBUTING.md#releases).

- A pull request that changes what users of the CLI, the hook or the npm package see adds one note in `.tegami/`.
- Never edit `CHANGELOG.md` or `.tegami/publish-lock.yaml` by hand. Tegami writes both when it versions the pending notes.
- Never change the version in `package.json` or `npm/*/package.json` by hand. The Release workflow bumps all seven together in the version PR.
