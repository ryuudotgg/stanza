import { expect, test } from "bun:test";
import { existsSync, readFileSync } from "node:fs";
import { basename, dirname, extname, isAbsolute, join, relative, resolve, sep } from "node:path";
import { diffExampleToken } from "../docs/lib/diff.ts";
import { ruleSentence } from "../docs/lib/rules.ts";
import { RULES, type RuleId } from "../src/engine/rules.ts";
import { entryFor } from "../src/languages/index.ts";
import { formatText } from "../src/step.ts";
import { pages, sidebar, texts } from "./docs-pages.ts";
import { catalog, tableAfter } from "./rule-tables.ts";

const docsRoot = join(import.meta.dir, "..", "docs", "content", "docs");
const fixturesRoot = join(import.meta.dir, "fixtures");
const ids = Object.keys(RULES);
const rulesPages = [
  "index",
  "directives",
  "---Blank Lines Added---",
  "after-multiline",
  "after-guard",
  "let-step",
  "switch-clauses",
  "---Blank Lines Removed---",
  "guard-join",
  "consume-join",
  "use-join",
  "guard-chain",
  "short-body",
  "edge-blank",
  "---Braces---",
  "braces",
  "braces-and-lint-config",
  "---Reported Only---",
  "block-spacing",
  "wall",
];

function includes(raw: string): string[] {
  return [...raw.matchAll(/<include\b[^>]*>\s*([^<]*?)\s*<\/include>/gs)].map((match) =>
    match[1]!.trim(),
  );
}

function diffFixtures(raw: string): string[] {
  return [...raw.matchAll(/<DiffExample\s+fixture="([^"]+)"\s*\/>/g)].map((match) => match[1]!);
}

function fixtureBefore(directory: string, fixture: string): string {
  const beforePath = resolve(fixturesRoot, fixture.replace(/(\.[^.]+)$/, ".before$1"));
  const afterPath = resolve(fixturesRoot, fixture.replace(/(\.[^.]+)$/, ".after$1"));
  const location = relative(join(fixturesRoot, directory), beforePath);
  expect(
    location !== ".." && !location.startsWith(`..${sep}`) && !isAbsolute(location),
    `${beforePath} is outside ${directory}`,
  ).toBe(true);

  for (const path of [beforePath, afterPath]) {
    expect(existsSync(path), `${path} is missing`).toBe(true);
    expect(entryFor(path), `${path} has no language`).toBeDefined();
    expect(
      readFileSync(path, "utf8").replace(/\n$/, "").split("\n").length,
      `${path} exceeds 15 lines`,
    ).toBeLessThanOrEqual(15);
  }

  return beforePath;
}

test("rule pages cover the catalog and the sidebar shows the rule groups", () => {
  const rulePages = pages
    .filter((url) => url.startsWith("/rules/"))
    .filter((url) => url !== "/rules/directives" && url !== "/rules/braces-and-lint-config")
    .map((url) => url.slice("/rules/".length));

  expect(rulePages.toSorted()).toEqual(ids.toSorted());

  const meta = JSON.parse(readFileSync(join(docsRoot, "rules", "meta.json"), "utf8")) as {
    pages: string[];
  };

  expect(meta.pages).toEqual(rulesPages);
  expect(sidebar.filter((url) => url.startsWith("/rules"))).toEqual(
    rulesPages
      .filter((page) => !page.startsWith("---"))
      .map((page) => (page === "index" ? "/rules" : `/rules/${page}`)),
  );
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
    expect(page!.processed.trimStart().startsWith(ruleSentence(id as RuleId))).toBe(true);

    if (rule.fixable) {
      const fixtures = diffFixtures(page!.raw);
      expect(fixtures.length, `${url} has no diff example`).toBeGreaterThan(0);
      expect(page!.processed.match(/```[^\n]*/)?.[0], `${url} first code block`).toBe(
        `\`\`\`${extname(fixtures[0]!).slice(1)} ${diffExampleToken} noCopy`,
      );

      for (const fixture of fixtures) {
        const beforePath = fixtureBefore(id, fixture);
        expect(
          formatText(beforePath, readFileSync(beforePath, "utf8"), { mode: "check" }).findings.some(
            (finding) => finding.rule === id,
          ),
          `${beforePath} has no ${id} finding`,
        ).toBe(true);
      }

      return;
    }

    const accordion = page!.raw.indexOf("<Accordion");
    const example = page!.raw.match(/<include\b[^>]*title="Example"[^>]*>/)?.index;
    const findings = page!.raw.match(/<include\b[^>]*title="Findings"[^>]*>/)?.index;

    expect(example).toBeDefined();
    expect(findings).toBeDefined();
    expect(findings!).toBeGreaterThan(example!);
    if (accordion >= 0) expect(findings!).toBeLessThan(accordion);

    const files = includes(page!.raw).map((name) => resolve(docsRoot, dirname(page!.path), name));
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
        expect(entryFor(path), `${path} has no language`).toBeDefined();
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
        expect(page!.processed).toContain(readFileSync(path, "utf8").replace(/\n$/, ""));

      for (const finding of readFileSync(findingsPath, "utf8").trim().split("\n"))
        expect(finding).toMatch(new RegExp(`^\\d+:\\d+ ${id}$`));
    }
  });

for (const [url, directory] of [
  ["/rules/directives", "ignore"],
  ["/rules/braces-and-lint-config", "braces-enforced"],
] as const)
  test(`${url} has diff examples`, () => {
    const page = texts[url];
    expect(page).toBeDefined();
    const fixtures = diffFixtures(page!.raw);
    expect(fixtures.length).toBeGreaterThan(0);

    for (const fixture of fixtures) fixtureBefore(directory, fixture);
  });
