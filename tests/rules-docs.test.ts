import { expect, test } from "bun:test";
import { existsSync, readFileSync } from "node:fs";
import { basename, dirname, extname, isAbsolute, join, relative, resolve, sep } from "node:path";
import { diffExampleToken, fixturePaths } from "../docs/lib/diff.ts";
import catalog from "../docs/lib/catalog.json";
import { ruleSentence, type RuleId } from "../docs/lib/rules.ts";
import { pages } from "./docs-pages.ts";
import { page, ruleRows, tableAfter } from "./docs-support.ts";
import { batch } from "./support.ts";

const docsRoot = join(import.meta.dir, "..", "docs", "content", "docs");
const fixturesRoot = join(import.meta.dir, "fixtures");
const ids = Object.keys(catalog.rules);

const fixtureRules = new Map<string, RuleId>([["guard-chain/too-long.ts", "after-multiline"]]);

function checked(path: string) {
  const [answer] = batch(fixturesRoot, [
    {
      path,
      text: readFileSync(path, "utf8"),
      mode: "check",
      braces: null,
      changedLines: null,
      explain: [],
    },
  ]);

  expect(answer!.error, path).toBeNull();
  return answer!.findings;
}

function includes(raw: string): string[] {
  return [...raw.matchAll(/<include\b[^>]*>\s*([^<]*?)\s*<\/include>/gs)].map((match) =>
    match[1]!.trim(),
  );
}

function diffFixtures(raw: string): string[] {
  return [...raw.matchAll(/<DiffExample\s+fixture="([^"]+)"\s*\/>/g)].map((match) => match[1]!);
}

function fixtureBefore(directory: string, fixture: string): string {
  const { before: beforePath, after: afterPath } = fixturePaths(fixturesRoot, fixture);
  const location = relative(join(fixturesRoot, directory), beforePath);
  expect(
    location !== ".." && !location.startsWith(`..${sep}`) && !isAbsolute(location),
    `${beforePath} is outside ${directory}`,
  ).toBe(true);

  for (const path of [beforePath, afterPath]) {
    expect(existsSync(path), `${path} is missing`).toBe(true);
    expect(catalog.extensions, `${path} has no language`).toContain(extname(path));
    expect(
      readFileSync(path, "utf8").replace(/\n$/, "").split("\n").length,
      `${path} exceeds 15 lines`,
    ).toBeLessThanOrEqual(15);
  }

  return beforePath;
}

test("rule pages cover the catalog", () => {
  const rulePages = pages
    .filter((url) => url.startsWith("/rules/"))
    .filter((url) => url !== "/rules/directives" && url !== "/rules/braces-and-lint-config")
    .map((url) => url.slice("/rules/".length));

  expect(rulePages.toSorted()).toEqual(ids.toSorted());
});

test("the rules overview renders the catalog's rule rows", () => {
  const text = page("/rules").processed;
  expect(tableAfter(text, ["rule", "what it does"])).toEqual(ruleRows(true));
  expect(tableAfter(text, ["rule", "what it reports"])).toEqual(ruleRows(false));

  for (const id of ids) expect(text).toContain(`[\`${id}\`](/rules/${id})`);
});

for (const [id, rule] of Object.entries(catalog.rules))
  test(`/rules/${id}`, () => {
    const url = `/rules/${id}`;
    const rulePage = page(url);
    expect(rulePage.processed.trimStart().startsWith(ruleSentence(id as RuleId))).toBe(true);

    if (rule.fixable) {
      const fixtures = diffFixtures(rulePage.raw);
      expect(fixtures.length, `${url} has no diff example`).toBeGreaterThan(0);
      expect(rulePage.processed.match(/```[^\n]*/)?.[0], `${url} first code block`).toBe(
        `\`\`\`${extname(fixtures[0]!).slice(1)} ${diffExampleToken} noCopy`,
      );

      for (const fixture of fixtures) {
        const beforePath = fixtureBefore(id, fixture);
        const expected = fixtureRules.get(fixture) ?? id;

        expect(
          checked(beforePath).some((finding) => finding.rule === expected),
          `${beforePath} has no ${expected} finding`,
        ).toBe(true);
      }

      return;
    }

    const accordion = rulePage.raw.indexOf("<Accordion");
    const example = rulePage.raw.match(/<include\b[^>]*title="Example"[^>]*>/)?.index;
    const findings = rulePage.raw.match(/<include\b[^>]*title="Findings"[^>]*>/)?.index;

    expect(example).toBeDefined();
    expect(findings).toBeDefined();
    expect(findings!).toBeGreaterThan(example!);
    if (accordion >= 0) expect(findings!).toBeLessThan(accordion);

    const files = includes(rulePage.raw).map((name) =>
      resolve(docsRoot, dirname(rulePage.path), name),
    );

    const beforeFiles = files.filter((path) => /\.before\.[^.]+$/.test(path));
    expect(beforeFiles.length, `${url} has no example`).toBeGreaterThan(0);

    for (const beforePath of beforeFiles) {
      const location = relative(join(fixturesRoot, id), beforePath);
      expect(
        location !== ".." && !location.startsWith(`..${sep}`) && !isAbsolute(location),
        `${beforePath} is outside ${id}`,
      ).toBe(true);

      const afterPath = beforePath.replace(/\.before(\.[^.]+)$/, ".after$1");
      expect(existsSync(afterPath), `${afterPath} is missing`).toBe(true);
      expect(readFileSync(afterPath, "utf8")).toBe(readFileSync(beforePath, "utf8"));

      for (const path of [beforePath, afterPath]) {
        expect(catalog.extensions, `${path} has no language`).toContain(extname(path));
        expect(
          readFileSync(path, "utf8").replace(/\n$/, "").split("\n").length,
          `${path} exceeds 15 lines`,
        ).toBeLessThanOrEqual(15);
      }

      const findingsPath = join(
        dirname(beforePath),
        `${basename(beforePath).split(".before.")[0]}.findings`,
      );

      expect(files, `${findingsPath} is not included`).toContain(findingsPath);

      for (const path of [beforePath, findingsPath])
        expect(rulePage.processed).toContain(readFileSync(path, "utf8").replace(/\n$/, ""));

      for (const finding of readFileSync(findingsPath, "utf8").trim().split("\n"))
        expect(finding).toMatch(new RegExp(`^\\d+:\\d+ ${id}$`));
    }
  });

for (const [url, directory] of [
  ["/rules/directives", "ignore"],
  ["/rules/braces-and-lint-config", "braces-enforced"],
] as const)
  test(`${url} has diff examples`, () => {
    const fixtures = diffFixtures(page(url).raw);
    expect(fixtures.length).toBeGreaterThan(0);
    for (const fixture of fixtures) fixtureBefore(directory, fixture);
  });
