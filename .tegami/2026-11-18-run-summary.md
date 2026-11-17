---
packages:
  "@ryuugg/stanza": minor
---

### ✨ Runs end with a summary line

`--check` and `--fix` over paths, `--changed` or `--staged` now end with one line on stderr that counts what the run found and did, such as `4 findings in 2 files: 2 fixable with --fix, 2 by hand`, `checked 12 files, no findings` or `rewrote 2 files, no findings left`, and a file that failed adds a clause such as `, 1 file failed`. A `--changed` or `--staged` run that selects nothing now says `no changed files to check` or `no staged files to check` instead of printing nothing. Under `--staged` the repair lines now come last, after the lines about names that are not UTF-8, the config notices and the summary. `--json`, `--stdin`, `stanza explain` and the hook print no summary, and the exit code is unchanged.
