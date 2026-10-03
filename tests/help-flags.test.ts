import { expect, test } from "bun:test";
import { readFileSync } from "node:fs";
import { hookInput } from "../src/hook.ts";
import { columns, flags } from "../src/usage.ts";

const hookDescription = flags.find(([name]) => name === "hook")![1];

test("help hook events match the events hookInput accepts", () => {
  const source = readFileSync(new URL("../src/hook.ts", import.meta.url), "utf8");
  const eventSet = source.match(/const events = new Set\(\[([^\]]+)\]\)/);
  expect(eventSet).not.toBeNull();

  const acceptedEvents = Array.from(eventSet![1]!.matchAll(/"([^"]+)"/g), (match) => match[1]!);
  const documentedEvents = hookDescription
    .split("; ")
    .slice(0, -1)
    .flatMap((clause) => clause.split(" on ")[0]!.split(" and "));

  expect(documentedEvents.toSorted()).toEqual(acceptedEvents.toSorted());

  for (const event of acceptedEvents) {
    const input = hookInput(
      JSON.stringify({
        hook_event_name: event,
        tool_name: event === "PostToolUse" ? "Edit" : "Write",
        tool_input: { file_path: "example.ts", content: "" },
      }),
      "/repo",
    );

    expect(input).not.toHaveProperty("error");
    expect(input).not.toEqual({ event: "ignored" });
  }
});

test("help hook row names the tools and JSON input", () => {
  expect(hookDescription).toContain("PostToolUse on Edit, MultiEdit and Codex apply_patch");
  expect(hookDescription).toContain("PreToolUse on Write");
  expect(hookDescription).toEndWith("reads JSON on stdin");
});

test("CLI help prints aligned flag rows without tying --json to hooks", () => {
  const result = Bun.spawnSync(
    [process.execPath, new URL("../src/cli.ts", import.meta.url).pathname, "--help"],
    { stdout: "pipe", stderr: "pipe" },
  );

  expect(result.exitCode).toBe(0);
  expect(result.stderr.toString()).toBe("");

  const output = result.stdout.toString().split("\n");
  for (const row of columns(flags)) expect(output).toContain(row);

  expect(flags.find(([name]) => name === "--json")![1]).toBe("findings as a JSON array");
});
