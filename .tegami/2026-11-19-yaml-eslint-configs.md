---
packages:
  "@ryuugg/stanza": patch
---

### 🐛 YAML ESLint configs answer like their JSON twins

`.eslintrc.yaml`, `.eslintrc.yml` and an extensionless `.eslintrc` are now parsed as a whole, so `rules`, `root`, `extends` and `overrides` decide braces exactly as they do in `.eslintrc.json`. Before, Stanza scanned YAML lines for `curly`, so an override that extends `eslint:all` for `*.ts` over a root `curly: off` let `--fix` remove braces ESLint enforces, and `{rules:{curly:"off"}}` in `.eslintrc` read as off although ESLint reads `curly:"off"` there as a key. An extensionless `.eslintrc` is read as JSON with comments first, then as YAML. A YAML file that uses anchors, tags, multiline or block scalars, more than one document, duplicate keys or other syntax outside the plain subset Stanza reads, even under an unrelated key, now keeps braces with the notice naming that config.
