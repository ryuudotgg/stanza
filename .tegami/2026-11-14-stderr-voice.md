---
packages:
  "@ryuugg/stanza": patch
---

### 🎨 Notices and errors read plainly and name the program that printed them

Lines on stderr now say `cannot` where they said `could not`, as in `stanza: cannot read the line width from .prettierrc.json, so the next source sets it`, and an internal error reads `left untouched` like the findings do. `stanza hook` prints its brace and line width notices after `stanza hook:` instead of `stanza:`, and so does the npm launcher when it runs as `stanza hook`. A git failure names the folder git ran in relative to the working directory, or leaves it out when they are the same, and names the index as git does, such as `stanza: git diff failed: fatal: .git/index: index file smaller than expected`, where it used to print absolute paths and a temporary copy of the index. The worker thread warning now says `failed to start`. The same failures go to the same streams with the same exit codes.
