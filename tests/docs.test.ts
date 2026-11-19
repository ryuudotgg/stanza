import { expect, test } from "bun:test";
import { existsSync, readFileSync, statSync } from "node:fs";
import { dirname, join } from "node:path";
import { pages, sidebar } from "./docs-pages.ts";

const docsRoot = join(import.meta.dir, "..", "docs", "content", "docs");

test("the sidebar lists every docs page once and no other", () => {
  expect(pages.length).toBeGreaterThan(0);
  expect(new Set(sidebar).size, "the sidebar repeats a page").toBe(sidebar.length);
  expect(sidebar.toSorted()).toEqual(pages.toSorted());
});

test("every sidebar entry in a meta file names a page or folder", () => {
  const missing: string[] = [];
  for (const metaPath of new Bun.Glob("**/meta.json").scanSync({ cwd: docsRoot })) {
    const meta = JSON.parse(readFileSync(join(docsRoot, metaPath), "utf8")) as { pages: string[] };
    const folder = join(docsRoot, dirname(metaPath));
    for (const entry of meta.pages) {
      if (/^---.*---$/.test(entry)) continue;

      const extracted = entry.startsWith("...");
      const name = extracted ? entry.slice(3) : entry;
      const isFolder = statSync(join(folder, name), { throwIfNoEntry: false })?.isDirectory();
      const isPage = !extracted && existsSync(join(folder, `${name}.mdx`));
      if (!isFolder && !isPage) missing.push(`${metaPath}: ${entry}`);
    }
  }

  expect(missing).toEqual([]);
});
