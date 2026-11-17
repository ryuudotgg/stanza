---
packages:
  "@ryuugg/stanza": patch
---

### 🎨 File selection starts fewer git processes and runs them together

`--changed`, `--changed --hunks` and the hook after an edit or at the end of a turn run the git calls that pick changed files side by side instead of one after another, and a `Stop` does the same across the repositories it names. A repository that tracks a `.gitattributes` no longer makes every hook event, `--changed` run and directory run start `git check-attr`: Stanza skips it when no attribute source git would read can name `linguist-generated` or `stanza`. When the git index shows that every file an edit or a turn's transcript names is already tracked, `stanza hook` no longer starts `git ls-files --others`, which can only list untracked files. `--staged` reads files with a clean or smudge `filter` through one `git cat-file --batch --filters`, so five filtered files start one git process where they used to start five. Files whose driver runs a `filter.<driver>.process`, and paths holding a newline, ending in a carriage return or starting with a space or tab, are still read one at a time. A check outside any repository usually learns that from the files on disk, so a file under `/tmp` starts no git process at all, and a directory with nothing to format is listed once instead of twice. The files picked and everything printed stay the same.
