---
packages:
  "@ryuugg/stanza": patch
---

### 🐛 A repository inside a folder outside git keeps its attributes

Running `stanza --check .` or `--fix .` in a folder that is not in git now hands each repository found below it to git, so that repository's ignore rules, `-stanza` opt outs and `linguist-generated` marks apply exactly as when you pass its path. Before, the walk entered the repository like any other folder, applied only `.gitignore` files, and checked and fixed files its `.gitattributes` excludes. A `.gitignore` outside the repository can still skip its folder, but no longer reaches the files inside it. A folder git refuses as a repository, such as one holding an empty `.git`, now prints git's error and makes the run exit 2, as passing its path does. With git missing from `PATH`, the walk is unchanged.
