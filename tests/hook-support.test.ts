import { afterEach, expect, test } from "bun:test";
import { readFileSync } from "node:fs";
import { join } from "node:path";
import { hook } from "./hook-support.ts";
import { scratchGitRepository, stanzaBinary } from "./support.ts";

const braced = "if (ready) {\n  act();\n}\n";
const files = { "a.ts": braced, ".gitignore": "!/a.ts\n" };
const configured = process.env.STANZA_BIN;

afterEach(() => {
  if (configured === undefined) delete process.env.STANZA_BIN;
  else process.env.STANZA_BIN = configured;
});

test("the hook runner never starts in the caller's directory", () => {
  const cwd = scratchGitRepository({ files });
  const caller = process.cwd();
  process.env.STANZA_BIN = stanzaBinary();

  process.chdir(cwd);
  try {
    expect(hook("{}")).toEqual({ code: 0, stdout: "", stderr: "" });
  } finally {
    process.chdir(caller);
  }

  expect(readFileSync(join(cwd, "a.ts"), "utf8")).toBe(braced);
});

test("the hook runner starts in the directory the caller names", () => {
  const cwd = scratchGitRepository({ files });
  expect(hook("{}", { cwd })).toEqual({ code: 0, stdout: "", stderr: "" });
  expect(readFileSync(join(cwd, "a.ts"), "utf8")).toBe("if (ready) act();\n");
});
