import { expect, test } from "bun:test";
import { existsSync, readFileSync } from "node:fs";
import { basename, dirname, isAbsolute, join, relative, resolve, sep } from "node:path";
import { RULES } from "../src/engine/rules.ts";
import { entryFor } from "../src/languages/index.ts";
import { formatText } from "../src/step.ts";
import { pages, texts } from "./docs-pages.ts";
import { catalog, row } from "./rule-tables.ts";

const docsRoot = join(import.meta.dir, "..", "docs", "content", "docs");
const fixturesRoot = join(import.meta.dir, "fixtures");
const ids = Object.keys(RULES);

function includes(raw: string): string[] {
  return [...raw.matchAll(/<include\b[^>]*>\s*([^<]*?)\s*<\/include>/gs)].map((match) =>
    match[1]!.trim(),
  );
}

function includedFiles(url: string): string[] {
  const page = texts[url];
  expect(page, `${url} has no page text`).toBeDefined();
  return includes(page!.raw).map((name) => resolve(docsRoot, dirname(page!.path), name));
}

function expectIncluded(path: string, processed: string): string {
  expect(existsSync(path), `${path} is missing`).toBe(true);
  const content = readFileSync(path, "utf8");
  expect(processed, `${path} is missing from processed markdown`).toContain(
    content.replace(/\n$/, ""),
  );

  return content;
}

function tableAfter(text: string, header: [string, string]): [string, string][] {
  const lines = text.split("\n");
  const start = lines.findIndex((line) => row(line).join("|") === header.join("|"));
  expect(start, `${header.join(" | ")} table is missing`).toBeGreaterThanOrEqual(0);
  expect(lines[start + 1]).toMatch(/^\|[\s|:-]+\|$/);

  const end = lines.findIndex((line, index) => index > start + 1 && !line.startsWith("|"));
  return lines.slice(start + 2, end < 0 ? undefined : end).map(row);
}

test("rule page list and sidebar order match the catalog", () => {
  const rulePages = pages
    .filter((url) => url.startsWith("/rules/"))
    .filter((url) => url !== "/rules/directives" && url !== "/rules/braces-and-lint-config")
    .map((url) => url.slice("/rules/".length));

  expect(rulePages.toSorted()).toEqual(ids.toSorted());

  const meta = JSON.parse(readFileSync(join(docsRoot, "rules", "meta.json"), "utf8")) as {
    pages: string[];
  };

  expect(meta.pages.filter((page) => ids.includes(page))).toEqual(ids);
});

test("overview tables match the rule catalog", () => {
  const text = texts["/rules"]?.processed;
  expect(text).toBeDefined();
  expect(tableAfter(text!, ["rule", "what it does"])).toEqual(catalog(true));
  expect(tableAfter(text!, ["rule", "what it reports"])).toEqual(catalog(false));

  for (const id of ids) expect(text).toContain(`[\`${id}\`](/rules/${id})`);
});

for (const [id, rule] of Object.entries(RULES))
  test(`/rules/${id}`, () => {
    const url = `/rules/${id}`;
    const page = texts[url];
    expect(page, `${url} has no page text`).toBeDefined();
    expect(page!.processed.trimStart().startsWith(rule.summary)).toBe(true);

    const files = includedFiles(url);
    expect(files.length, `${url} has no example`).toBeGreaterThan(0);

    const ruleRoot = join(fixturesRoot, id);
    for (const path of files) {
      const location = relative(ruleRoot, path);
      expect(
        location !== ".." && !location.startsWith(`..${sep}`) && !isAbsolute(location),
        `${path} is outside ${ruleRoot}`,
      ).toBe(true);

      const content = expectIncluded(path, page!.processed);
      if (entryFor(path))
        expect(
          content.replace(/\n$/, "").split("\n").length,
          `${path} exceeds 15 lines`,
        ).toBeLessThanOrEqual(15);
    }

    const beforeFiles = files.filter((path) => /\.before\.[^.]+$/.test(path));
    expect(beforeFiles.length, `${url} has no before source`).toBeGreaterThan(0);

    for (const beforePath of beforeFiles) {
      expect(entryFor(beforePath), `${beforePath} has no language`).toBeDefined();

      const afterPath = beforePath.replace(/\.before(\.[^.]+)$/, ".after$1");
      expect(existsSync(afterPath), `${afterPath} is missing`).toBe(true);
      const before = readFileSync(beforePath, "utf8");
      const after = readFileSync(afterPath, "utf8");
      expect(entryFor(afterPath), `${afterPath} has no language`).toBeDefined();
      expect(
        after.replace(/\n$/, "").split("\n").length,
        `${afterPath} exceeds 15 lines`,
      ).toBeLessThanOrEqual(15);

      if (rule.fixable) {
        expect(files, `${afterPath} is not included`).toContain(afterPath);
        expect(
          formatText(beforePath, before, { mode: "check" }).findings.some(
            (finding) => finding.rule === id,
          ),
          `${beforePath} has no ${id} finding`,
        ).toBe(true);
      } else {
        expect(after).toBe(before);

        const findingsPath = join(
          dirname(beforePath),
          `${basename(beforePath).split(".before.")[0]}.findings`,
        );

        expect(files, `${findingsPath} is not included`).toContain(findingsPath);
        const findings = readFileSync(findingsPath, "utf8").trim().split("\n");
        expect(findings.length).toBeGreaterThan(0);
        for (const finding of findings) expect(finding).toMatch(new RegExp(`^\\d+:\\d+ ${id}$`));
      }
    }
  });

for (const [url, directory] of [
  ["/rules/directives", "ignore"],
  ["/rules/braces-and-lint-config", "braces-enforced"],
] as const)
  test(`${url} includes its example`, () => {
    const page = texts[url];
    expect(page).toBeDefined();
    const files = includedFiles(url);
    expect(files.length).toBeGreaterThan(0);

    for (const path of files) {
      expect(dirname(path)).toBe(join(fixturesRoot, directory));
      expectIncluded(path, page!.processed);
    }
  });
