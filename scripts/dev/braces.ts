import { readdirSync, realpathSync } from "node:fs";
import { join, relative } from "node:path";
import { entry } from "../../src/languages/javascript/entry.ts";
import { braceDecisions } from "../../src/languages/javascript/config/index.ts";

const SKIPPED = new Set([".git", "node_modules"]);
export function inputs(dir: string): string[] {
  const found = ["."];
  const pending = [""];
  while (pending.length > 0) {
    const current = pending.pop()!;
    for (const item of readdirSync(join(dir, current), { withFileTypes: true })) {
      if (!item.isDirectory() || SKIPPED.has(item.name)) continue;

      const path = current ? `${current}/${item.name}` : item.name;
      found.push(path);
      pending.push(path);
    }
  }

  return found.sort();
}

const cwd = realpathSync(process.cwd());
function line(dir: string, extensions: (string | undefined)[]): string {
  const decisions = extensions.map((extension) => [
    extension ?? "",
    braceDecisions(dir, extension).map((decision) => ({
      family: decision.family,
      setting: decision.setting,
      files: decision.files.map((file) => relative(cwd, file)),
    })),
  ]);

  return JSON.stringify({ dir, decisions: Object.fromEntries(decisions) });
}

if (import.meta.main) {
  const args = process.argv.slice(2);
  const only = args[0] === "--extension" ? [args[1] || undefined] : undefined;
  const extensions = only ?? [undefined, ...entry.extensions];
  for (const dir of only ? args.slice(2) : args) console.log(line(dir, extensions));
}
