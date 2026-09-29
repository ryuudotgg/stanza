import { describe, expect, test } from "bun:test";
import { RULES } from "../src/engine/rules.ts";
import { pages, sidebar } from "./docs-pages.ts";

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

describe("docs site map", () => {
  for (const path of siteMap)
    test(path, () => {
      expect(pages, `${path} has no page source`).toContain(path);
      expect(sidebar, `${path} is missing from the sidebar`).toContain(path);
    });
});
