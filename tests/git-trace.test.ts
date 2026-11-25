import { expect, test } from "bun:test";
import { mkdirSync, writeFileSync } from "node:fs";
import { join } from "node:path";
import { type ShimOptions, shownCall, takeTrace, traceShim } from "../scripts/git-tools.ts";
import { scratch } from "./support.ts";

const succeed = Bun.which("true");
if (!succeed) throw new Error("true not found on PATH");

function shimIn(name: string, options?: ShimOptions): { shim: string; trace: string } {
  const dir = join(scratch("trace"), name);
  mkdirSync(dir);
  return { shim: join(dir, "git"), trace: traceShim(dir, succeed!, options) };
}

async function traced(calls: string[][]): Promise<string[][]> {
  const { shim, trace } = shimIn("shim");
  const exits = await Promise.all(calls.map((args) => Bun.spawn([shim, ...args]).exited));
  expect(exits.every((code) => code === 0)).toBe(true);
  return takeTrace(trace).map(({ args }) => args);
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

  const records = takeTrace(trace).map(({ args }) => args);
  expect(shown(records)).toEqual(shown(calls));
  expect(records.find((args) => args[0]?.startsWith("\\"))?.[0]).toHaveLength(2);
  expect(takeTrace(trace)).toEqual([]);
});

test("returns sequential calls in the order they ran", () => {
  const { shim, trace } = shimIn("order");
  const calls = Array.from({ length: 12 }, (_, n) => [`call-${n}`]);
  for (const args of calls) expect(Bun.spawnSync([shim, ...args]).exitCode).toBe(0);

  expect(takeTrace(trace).map(({ args }) => args)).toEqual(calls);
});

test("hides attribute metadata from var -l alone, keeping git's bytes and exit status", () => {
  const dir = join(scratch("trace"), "metadata");
  mkdirSync(dir);
  const fake = join(scratch("fake-git"), "git");
  writeFileSync(
    fake,
    `#!/bin/sh\nprintf 'GIT_ATTR_SYSTEM=/etc\\nuser.name=GIT_ATTR_kept\\nGIT_ATTR_GLOBAL=x\\n'\nexit 3\n`,
    { mode: 0o755 },
  );

  traceShim(dir, fake, { hideAttributeMetadata: true });
  const listed = (...args: string[]) => {
    const result = Bun.spawnSync([join(dir, "git"), ...args]);
    return { code: result.exitCode, stdout: result.stdout.toString() };
  };

  const unfiltered = "GIT_ATTR_SYSTEM=/etc\nuser.name=GIT_ATTR_kept\nGIT_ATTR_GLOBAL=x\n";
  expect(listed("-C", "/tmp", "var", "-l")).toEqual({
    code: 3,
    stdout: "user.name=GIT_ATTR_kept\n",
  });

  expect(listed("--attr-source", "HEAD", "var", "-l")).toEqual({
    code: 3,
    stdout: "user.name=GIT_ATTR_kept\n",
  });

  expect(listed("-C", "var", "-l")).toEqual({ code: 3, stdout: unfiltered });
  expect(listed("show", "var", "-l")).toEqual({ code: 3, stdout: unfiltered });
});

test("refuses a flag only where git reads it as a global option", () => {
  const { shim } = shimIn("refuse", { refuse: "--attr-source" });
  const code = (...args: string[]) => Bun.spawnSync([shim, ...args]).exitCode;

  expect(code("--attr-source", "HEAD", "version")).toBe(129);
  expect(code("-C", "dir", "--attr-source=HEAD", "version")).toBe(129);

  expect(code("--shallow-file", "--attr-source=HEAD", "version")).toBe(0);
  expect(code("-c", "a.b=--attr-source=x", "version")).toBe(0);
  expect(code("ls-files", "--", "--attr-source=HEAD")).toBe(0);
  expect(code("--exec-path", "--attr-source=HEAD")).toBe(0);
});

test("fails every call of a round that never fills, then lets a full round through", async () => {
  const { shim } = shimIn("meeting", { meet: { commands: ["status"], count: 3 } });
  const first = Bun.spawn([shim, "status"], { stderr: "pipe" });
  await Bun.sleep(500);
  const second = Bun.spawn([shim, "status"], { stderr: "pipe" });

  expect(await Promise.all([first.exited, second.exited])).toEqual([125, 125]);

  const late = [0, 300, 600].map(async (delay) => {
    await Bun.sleep(delay);
    return Bun.spawn([shim, "status"]).exited;
  });

  expect(await Promise.all(late)).toEqual([0, 0, 0]);
}, 10_000);

test("refuses a meeting with no command or a count outside two to 64", () => {
  expect(() => shimIn("empty", { meet: { commands: [], count: 2 } })).toThrow("a command");
  expect(() => shimIn("huge", { meet: { commands: ["status"], count: 100_000 } })).toThrow(
    "two to 64 calls",
  );
});

test("refuses a torn record", () => {
  for (const text of [
    "",
    "4294967296\0a\0-\0",
    "2\0a\0-\0",
    "1\0a\0",
    "1\0a\0-\0" + "2",
    "x\0",
    "1\0a\x003\0bc",
  ]) {
    const { trace } = shimIn("torn");
    writeFileSync(join(trace, "1"), text);
    writeFileSync(join(trace, "order"), "1\n");
    expect(() => takeTrace(trace)).toThrow("torn git record");
  }

  const { trace } = shimIn("unordered");
  writeFileSync(join(trace, "1"), "1\0a\0-\0");
  expect(() => takeTrace(trace)).toThrow("torn git record with no order line");
});

test("shows a call with every argument boundary", () => {
  expect(shownCall(["-C", "<cwd>", "var", "-l"])).toBe("git -C <cwd> var -l");
  expect(shownCall(["a b", "", "\\n", 'say "hi"'])).toBe('git "a b" "" "\\\\n" "say \\"hi\\""');
});
