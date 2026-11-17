---
packages:
  "@ryuugg/stanza": patch
---

### 🐛 TypeScript lint configs passed into calls keep braces

`--fix` no longer removes braces when a TypeScript lint config names its settings in a parameter property default such as `private p = config`, a parameter decorator or an abstract member key inside a call Stanza cannot read. Those settings may be changed by the call, so braces stay and the unread-config notice names the config. A parameter property named like the config, such as `public config`, now hides the outer binding the way TypeScript scopes it, so it no longer keeps braces with a false notice.
