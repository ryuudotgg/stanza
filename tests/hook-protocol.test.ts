import { expect, test } from "bun:test";
import { readFileSync } from "node:fs";
import { join } from "node:path";
import catalog from "../docs/lib/catalog.json";
import { openingFence, page, rawTableAfter, section } from "./docs-support.ts";
import { hook } from "./hook-support.ts";
import { scratchGitRepository } from "./support.ts";

const url = "/reference/hook-protocol";

const units = [
  "zero",
  "one",
  "two",
  "three",
  "four",
  "five",
  "six",
  "seven",
  "eight",
  "nine",
  "ten",
  "eleven",
  "twelve",
  "thirteen",
  "fourteen",
  "fifteen",
  "sixteen",
  "seventeen",
  "eighteen",
  "nineteen",
];

const tens = ["twenty", "thirty", "forty", "fifty", "sixty", "seventy", "eighty", "ninety"];
function quantity(word: string): number | undefined {
  if (/^\d+$/.test(word)) return Number(word);

  const [ten = "", unit] = word.toLowerCase().split("-");
  if (unit === undefined && units.includes(ten)) return units.indexOf(ten);

  const tenIndex = tens.indexOf(ten);
  const unitIndex = unit === undefined ? 0 : units.indexOf(unit);
  if (tenIndex < 0 || unitIndex < 0 || unitIndex > 9) return undefined;

  return 20 + tenIndex * 10 + unitIndex;
}

function inputEntry(name: string): string {
  const entry = new RegExp(`\\n\\s*${name}: \\{\\n([\\s\\S]*?)\\n\\s*\\},?\\n`).exec(
    section(url, "Input"),
  );

  expect(entry, `the Input table has no ${name} entry`).not.toBeNull();
  return entry![1]!;
}

function quoted(text: string, pattern: RegExp): string[] {
  return [...text.matchAll(pattern)].map(([, name]) => name!);
}

test("the hook protocol page states the usage line the CLI prints", () => {
  const line = catalog.usage
    .split("\n")
    .map((line) => line.replace(/^Usage:/, "").trim())
    .find((line) => line.startsWith("stanza hook "));

  expect(line).toBeDefined();
  expect(openingFence(page(url).processed)).toBe(line!);
});

test("the hook protocol page lists the events the hook accepts", () => {
  const type = /type: '([^']*)'/.exec(inputEntry("hook_event_name"))?.[1] ?? "";
  const typed = type.split(" | ").map((name) => JSON.parse(name) as string);

  const rows = rawTableAfter(section(url, "Events"), ["`hook_event_name`", "what runs"]);
  const named = rows.map(([cell = ""]) => ({ cell, names: quoted(cell, /`([^`]+)`/g) }));
  const tabled = named.flatMap(({ names }) => names);
  const unnamed = named.filter(({ names }) => names.length === 0).map(({ cell }) => cell);
  expect(unnamed).toEqual(["anything else"]);

  const events = [...catalog.hook.events].sort();
  expect(typed.sort()).toEqual(events);
  expect(tabled.sort()).toEqual(events);
});

test("the hook protocol page lists the tools each tool event accepts", () => {
  const description = inputEntry("tool_name").replaceAll('{" "}', " ").replace(/\s+/g, " ");
  const lists = description.matchAll(/(\w+): (<code>(?:(?!\s\w+: )[^.])*)(?=\.|\s\w+: )/g);
  const tools = [...lists].map(([, event, list]): [string, string[]] => [
    event!,
    quoted(list!, /<code>([^<]+)<\/code>/g),
  ]);

  expect(tools).toEqual(Object.entries(catalog.hook.tools));
  expect(quoted(description, /<code>([^<]+)<\/code>/g)).toEqual(tools.flatMap(([, list]) => list));
});

test("the hook protocol page states the reason's finding limit", () => {
  const stop = section(url, "Stop").replaceAll("*", "");
  const stated = quoted(stop, /\b([\w-]+) findings\b/g).flatMap((word) => {
    const count = quantity(word);
    return count === undefined ? [] : [count];
  });

  expect(stated).toEqual([catalog.hook.findings]);
});

test("the hook protocol page's exit table names exit 0 and exit 1", () => {
  const rows = rawTableAfter(section(url, "Exit Status"), [
    "exit",
    "when",
    "effect in Claude Code",
  ]);

  expect(rows.map(([code]) => code)).toEqual(["0", "1"]);
});

test("bad input and bad flags exit 1 with the message on stderr alone", () => {
  expect(hook("{}")).toEqual({ code: 0, stdout: "", stderr: "" });

  for (const [input, args] of [
    ["not json", []],
    ["[]", []],
    ["{}", ["--bad"]],
    ["{}", ["--braces", "--no-braces"]],
    ["{}", ["--hunks", "--hunks"]],
  ] as const) {
    const result = hook(input, { args: [...args] });
    expect(result.code, `${input} ${args.join(" ")}`).toBe(1);
    expect(result.stdout).toBe("");
    expect(result.stderr).not.toBe("");
  }
});

const braced = "if (ready) {\n  act();\n}\n";

test("an event the hook does not take exits 0 and leaves a file Stop would fix alone", () => {
  const cwd = scratchGitRepository({ files: { "a.ts": braced }, staged: true });
  const path = join(cwd, "a.ts");

  expect(hook({ hook_event_name: "UserPromptSubmit", cwd })).toEqual({
    code: 0,
    stdout: "",
    stderr: "",
  });

  expect(readFileSync(path, "utf8")).toBe(braced);

  expect(hook({ hook_event_name: "Stop", cwd }).code).toBe(0);
  expect(readFileSync(path, "utf8")).toBe("if (ready) act();\n");
});

test("a clean Stop and a Stop blocked by a wall both exit 0", () => {
  const clean = scratchGitRepository({ files: { "a.ts": "act();\n" }, staged: true });
  expect(hook({ cwd: clean })).toEqual({ code: 0, stdout: "", stderr: "" });

  const walled = scratchGitRepository({ files: { "a.ts": "f();\n".repeat(7) }, staged: true });
  const blocked = hook({ cwd: walled });
  expect(blocked.code).toBe(0);
  expect(blocked.stderr).toBe("");
  expect(JSON.parse(blocked.stdout).decision).toBe("block");
});
