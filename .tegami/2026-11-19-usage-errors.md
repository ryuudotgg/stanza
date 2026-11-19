---
packages:
  "@ryuugg/stanza": patch
---

### 🎨 Usage errors point at the help and suggest the flag you meant

A usage error now prints the problem and then ``run `stanza --help` for usage`` on stderr, in place of the four usage lines. A mistyped flag names the closest flag the command takes, so `stanza --chek src` prints `stanza: unknown flag --chek, did you mean --check?`, and `stanza explain` and `stanza hook` suggest from their own flags. Exit codes are unchanged.
