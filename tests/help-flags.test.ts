import { expect, test } from "bun:test";
import { hookEvents, hookInput, hookTools } from "../src/hook.ts";
import { columns, flags } from "../src/usage.ts";

const hookDescription = flags.find(([name]) => name === "hook")![1];

const toolsByEvent: Partial<Record<string, ReadonlySet<string>>> = hookTools;

const documentedTools = new Map(
  hookDescription
    .split("; ")
    .slice(0, -1)
    .flatMap((clause) => {
      const [events, tools] = clause.split(" on ");
      const names =
        tools?.split(/, | and /).map((tool) => tool.split(" ").at(-1)!.replaceAll("`", "")) ?? [];

      return events!.split(" and ").map((event) => [event, names.toSorted()] as const);
    }),
);

function accepts(event: string, tool: string): boolean {
  const input = hookInput(
    JSON.stringify({
      hook_event_name: event,
      tool_name: tool,
      tool_input: {
        file_path: "example.ts",
        notebook_path: "example.ipynb",
        content: "",
        command: "*** Update File: example.ts",
      },
      tool_response: "Success. Updated the following files:\nM example.ts",
    }),
    "/repo",
  );

  expect(input).not.toHaveProperty("error");
  return !("event" in input && input.event === "ignored");
}

test("help hook row names every event the hook accepts", () => {
  expect(
    [...documentedTools.keys()].toSorted(),
    "the hook help row and hookEvents name different events",
  ).toEqual([...hookEvents].toSorted());

  for (const event of hookEvents)
    expect(
      accepts(event, [...(toolsByEvent[event] ?? [""])][0]!),
      `hookInput ignores ${event}`,
    ).toBe(true);

  expect(hookDescription).toEndWith("reads JSON on stdin");
});

test("help hook row names every tool the hook accepts on each event", () => {
  for (const event of hookEvents)
    expect(
      documentedTools.get(event),
      `the hook help row and hookTools name different ${event} tools`,
    ).toEqual([...(toolsByEvent[event] ?? [])].toSorted());
});

test("hookInput accepts exactly the tools hookTools lists", () => {
  const candidates = new Set([
    ...Object.values(hookTools).flatMap((tools) => [...tools]),
    "NotebookEdit",
    "Bash",
    "Read",
  ]);

  for (const [event, tools] of Object.entries(hookTools))
    for (const tool of candidates)
      expect(
        accepts(event, tool),
        `hookInput ${tools.has(tool) ? "ignores" : "accepts"} ${tool} on ${event}, which hookTools ${tools.has(tool) ? "lists" : "does not list"}`,
      ).toBe(tools.has(tool));
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
