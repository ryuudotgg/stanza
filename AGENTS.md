# AGENTS.md

## Releases

Stanza releases through [Tegami](https://tegami.fuma-nama.dev). When to write a note, its format and which bump to pick are in the [Releases section of CONTRIBUTING.md](CONTRIBUTING.md#releases).

- A pull request that changes what users of the CLI, the hook or the npm package see adds one note in `.tegami/`.
- Never edit `CHANGELOG.md` or `.tegami/publish-lock.yaml` by hand. Tegami writes both when it versions the pending notes.
- Never change the version in `package.json` by hand. The Release workflow bumps it in the version PR.
