import { readFileSync, readdirSync, realpathSync } from "node:fs";
import { extname, join, relative } from "node:path";
import { entry } from "../../src/languages/javascript/entry.ts";
import { formatterWidth } from "../../src/languages/javascript/config/width.ts";

const SKIPPED = new Set([".git", "node_modules"]);
const PROBES = ["x.ts", "x.test.ts"];
export function inputs(dir: string): string[] {
  const found: string[] = [];
  const pending = [""];
  while (pending.length > 0) {
    const current = pending.pop()!;
    for (const probe of PROBES) found.push(current ? `${current}/${probe}` : probe);

    for (const item of readdirSync(join(dir, current), { withFileTypes: true })) {
      const path = current ? `${current}/${item.name}` : item.name;
      if (item.isDirectory() && !SKIPPED.has(item.name)) pending.push(path);
      else if (item.isFile() && entry.extensions.includes(extname(item.name))) found.push(path);
    }
  }

  return [...new Set(found)].sort();
}

const BUN = readFileSync(join(import.meta.dir, "..", "..", ".bun-version"), "utf8").trim();
if (import.meta.main && Bun.version !== BUN) {
  console.error(
    `width: Bun.Glob changes between releases; run this with Bun ${BUN}, not ${Bun.version}`,
  );

  process.exit(2);
}

const cwd = realpathSync(process.cwd());
function line(path: string): string {
  const width = formatterWidth(path);
  const source =
    width.source.kind === "config"
      ? { ...width.source, file: relative(cwd, width.source.file) }
      : width.source;

  return JSON.stringify({
    path,
    columns: width.columns,
    tab: width.tab,
    source,
    unread: width.unread.map((file) => relative(cwd, file)),
  });
}

if (import.meta.main) for (const path of process.argv.slice(2)) console.log(line(path));
