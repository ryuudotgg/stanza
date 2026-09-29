import { describe, expect, test } from "bun:test";
import { RULES } from "../src/engine/rules.ts";

const siteMap = [
  "/",
  "/getting-started",
  "/guides/release-binaries",
  "/guides/pre-commit",
  "/guides/editors",
  "/guides/existing-codebases",
  "/agents/codex",
  "/agents/claude-code",
  "/rules",
  "/rules/directives",
  "/rules/braces-and-lint-config",
  ...Object.keys(RULES).map((id) => `/rules/${id}`),
  "/reference/cli",
  "/reference/file-selection",
  "/reference/output",
  "/reference/explain",
  "/reference/hook-protocol",
];

const docsDirectory = new URL("../docs/", import.meta.url).pathname;
const result = Bun.spawnSync(["bun", "--preload", "./scripts/preload.ts", "scripts/pages.ts"], {
  cwd: docsDirectory,
});

if (result.exitCode !== 0)
  throw new Error(`Could not load docs pages: ${result.stderr.toString()}`);

const { pages, sidebar } = JSON.parse(result.stdout.toString()) as {
  pages: string[];
  sidebar: string[];
};

describe("docs site map", () => {
  for (const path of siteMap)
    test(path, () => {
      expect(pages, `${path} has no page source`).toContain(path);
      expect(sidebar, `${path} is missing from the sidebar`).toContain(path);
    });
});
