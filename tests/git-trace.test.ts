import { expect, test } from "bun:test";
import { mkdirSync, writeFileSync } from "node:fs";
import { join } from "node:path";
import { shownCall, takeTrace, traceShim } from "../scripts/git-tools.ts";
import { scratch } from "./support.ts";

const succeed = Bun.which("true");
if (!succeed) throw new Error("true not found on PATH");

function shimIn(name: string): { shim: string; trace: string } {
  const dir = join(scratch("trace"), name);
  mkdirSync(dir);
  return { shim: join(dir, "git"), trace: traceShim(dir, succeed!) };
}

async function traced(calls: string[][]): Promise<string[][]> {
  const { shim, trace } = shimIn("shim");
  const exits = await Promise.all(calls.map((args) => Bun.spawn([shim, ...args]).exited));
  expect(exits.every((code) => code === 0)).toBe(true);
  return takeTrace(trace);
}

const shown = (records: string[][]) => records.map((args) => JSON.stringify(args)).toSorted();

test("records every concurrent call whole", async () => {
  const calls = Array.from({ length: 120 }, (_, n) => [
    "-C",
    "/some/dir",
    "check-attr",
    "--stdin",
    "-z",
    "eol",
    "text",
    `filter-${n}`,
  ]);

  expect(shown(await traced(calls))).toEqual(shown(calls));
});

test("records concurrent calls larger than a shell's output buffer whole", async () => {
  const calls = Array.from({ length: 20 }, (_, n) => [
    "--",
    ...Array.from({ length: 2000 }, (_, index) => `src/file-${n}-${index}.ts`),
  ]);

  expect(shown(await traced(calls))).toEqual(shown(calls));
});

test("keeps argument boundaries and backslashes", () => {
  const { shim, trace } = shimIn("it's a dir");
  const calls = [["a b"], ["a", "b"], ["\\n"], ["", "line\nbreak"]];
  for (const args of calls) expect(Bun.spawnSync([shim, ...args]).exitCode).toBe(0);

  const records = takeTrace(trace);
  expect(shown(records)).toEqual(shown(calls));
  expect(records.find((args) => args[0]?.startsWith("\\"))?.[0]).toHaveLength(2);
  expect(takeTrace(trace)).toEqual([]);
});

test("refuses a torn record", () => {
  for (const text of ["2\0a\0", "1\0a\0" + "2", "x\0"]) {
    const { trace } = shimIn("torn");
    writeFileSync(join(trace, "1"), text);
    expect(() => takeTrace(trace)).toThrow("torn git record");
  }
});

test("shows a call with every argument boundary", () => {
  expect(shownCall(["-C", "<cwd>", "var", "-l"])).toBe("git -C <cwd> var -l");
  expect(shownCall(["a b", "", "\\n", 'say "hi"'])).toBe('git "a b" "" "\\\\n" "say \\"hi\\""');
});
