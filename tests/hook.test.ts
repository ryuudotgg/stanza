import { expect, test } from "bun:test";
import { existsSync, readFileSync, symlinkSync, writeFileSync } from "node:fs";
import { join } from "node:path";
import { blockReason, hookInput } from "../src/hook.ts";
import { RULES } from "../src/rules.ts";
import type { Finding } from "../src/types.ts";
import { run, scratch, scratchGitRepository } from "./support.ts";

const before = readFileSync(join(import.meta.dir, "fixtures/braces/bodies.before.ts"), "utf8");
const after = readFileSync(join(import.meta.dir, "fixtures/braces/bodies.after.ts"), "utf8");

function writeHook(
  cwd: string,
  toolInput: Record<string, unknown>,
  args: string[] = [],
  event = "PreToolUse",
  tool = "Write",
): ReturnType<typeof run> {
  return run(
    {
      cwd,
      stdin: Buffer.from(
        JSON.stringify({ cwd, hook_event_name: event, tool_name: tool, tool_input: toolInput }),
      ),
    },
    "hook",
    ...args,
  );
}

function finding(path = "wall.ts", rule: "wall" | "block-spacing" | "parse" = "wall"): Finding {
  return {
    path,
    line: 2,
    col: 3,
    rule,
    message: rule === "parse" ? "Unexpected token" : RULES[rule].message(),
    fixable: false,
  };
}

test("hookInput defaults absent and null fields and ignores other fields", () => {
  for (const text of ["{}", '{"cwd":null,"stop_hook_active":null}', '{"event":"stop"}'])
    expect(hookInput(text, "/repo")).toEqual({
      event: "stop",
      cwd: "/repo",
      stopHookActive: false,
      transcriptPath: undefined,
    });
});

test("hookInput resolves a relative cwd and accepts boolean stop_hook_active", () => {
  expect(hookInput('{"cwd":"../other","stop_hook_active":true}', "/repo/sub")).toEqual({
    event: "stop",
    cwd: "/repo/other",
    stopHookActive: true,
    transcriptPath: undefined,
  });

  expect(hookInput('{"cwd":"/other","stop_hook_active":false}', "/repo")).toEqual({
    event: "stop",
    cwd: "/other",
    stopHookActive: false,
    transcriptPath: undefined,
  });
});

test("hookInput resolves a relative transcript_path", () => {
  expect(hookInput('{"transcript_path":"transcripts/stop.jsonl"}', "/repo")).toEqual({
    event: "stop",
    cwd: "/repo",
    stopHookActive: false,
    transcriptPath: "/repo/transcripts/stop.jsonl",
  });
});

test("hookInput rejects wrong input types and invalid JSON", () => {
  for (const text of ["null", "[]", '"input"', "true", "1"])
    expect(hookInput(text, "/repo")).toEqual({ error: "input must be a JSON object" });

  expect(hookInput('{"cwd":42}', "/repo")).toEqual({ error: "cwd must be a string" });
  expect(hookInput('{"transcript_path":42}', "/repo")).toEqual({
    error: "transcript_path must be a string",
  });

  expect(hookInput('{"stop_hook_active":"true"}', "/repo")).toEqual({
    error: "stop_hook_active must be a boolean",
  });

  expect(hookInput("not json", "/repo")).toEqual({ error: "input must be valid JSON" });
});

test("hookInput allowlists Stop, SubagentStop and PreToolUse", () => {
  for (const event of ["Stop", "SubagentStop", null])
    expect(hookInput(JSON.stringify({ hook_event_name: event }), "/repo")).toEqual({
      event: "stop",
      cwd: "/repo",
      stopHookActive: false,
      transcriptPath: undefined,
    });

  for (const event of ["PostToolUse", "UserPromptSubmit"])
    expect(hookInput(JSON.stringify({ hook_event_name: event, cwd: 42 }), "/repo")).toEqual({
      event: "ignored",
    });

  for (const event of [42, false, {}, []])
    expect(hookInput(JSON.stringify({ hook_event_name: event }), "/repo")).toEqual({
      error: "hook_event_name must be a string",
    });
});

test("hookInput accepts only complete Write input for PreToolUse", () => {
  const tool_input = { file_path: "a.ts", content: before, extra: true };
  expect(
    hookInput(
      JSON.stringify({ hook_event_name: "PreToolUse", cwd: "sub", tool_name: "Write", tool_input }),
      "/repo",
    ),
  ).toEqual({ event: "write", cwd: "/repo/sub", toolInput: tool_input });

  for (const tool_name of ["Edit", "MultiEdit", "Other"])
    expect(
      hookInput(JSON.stringify({ hook_event_name: "PreToolUse", tool_name, tool_input }), "/repo"),
    ).toEqual({ event: "ignored" });

  for (const tool_input of [
    null,
    {},
    { file_path: "a.ts" },
    { file_path: 42, content: before },
    { file_path: "a.ts", content: 42 },
  ])
    expect(
      hookInput(
        JSON.stringify({ hook_event_name: "PreToolUse", tool_name: "Write", tool_input }),
        "/repo",
      ),
    ).toEqual({ event: "ignored" });

  expect(hookInput('{"hook_event_name":"PreToolUse","cwd":42}', "/repo")).toEqual({
    error: "cwd must be a string",
  });
});

test("PreToolUse Write returns fixed input without writing the file", () => {
  const cwd = scratchGitRepository();
  const file_path = join(cwd, "a.ts");
  const result = writeHook(cwd, { file_path, content: before, extra: 7 });

  expect(result.code).toBe(0);
  expect(result.stderr).toBe("");
  expect(result.stdout.endsWith("\n")).toBe(true);

  const output = JSON.parse(result.stdout);
  expect(output.hookSpecificOutput.hookEventName).toBe("PreToolUse");
  expect(output.hookSpecificOutput.updatedInput).toEqual({ file_path, content: after, extra: 7 });
  expect(output.hookSpecificOutput.additionalContext).toBe(
    `stanza formatted ${file_path} before writing it, so read it again before editing.`,
  );

  expect(output.hookSpecificOutput).not.toHaveProperty("permissionDecision");
  expect(existsSync(file_path)).toBe(false);
});

test("PreToolUse leaves unsupported, generated, invalid and clean input alone", () => {
  const cwd = scratchGitRepository();
  const cases = [
    ["a.ts", before, "Edit"],
    ["a.ts", before, "MultiEdit"],
    ["notes.md", before, "Write"],
    ["a.ts", `// @generated\n${before}`, "Write"],
    ["a.ts", "function (", "Write"],
    ["a.ts", after, "Write"],
  ];

  for (const [name, content, tool] of cases) {
    const result = writeHook(cwd, { file_path: join(cwd, name!), content }, [], "PreToolUse", tool);
    expect(result).toEqual({ code: 0, stderr: "", stdout: "" });
  }

  expect(writeHook(cwd, { file_path: join(cwd, "a.ts") })).toEqual({
    code: 0,
    stderr: "",
    stdout: "",
  });
});

test("PreToolUse skips paths outside the repository, symlinks out and ignored files", () => {
  const cwd = scratchGitRepository();
  const outside = scratch("outside");

  const file = join(outside, "a.ts");
  writeFileSync(file, before);

  symlinkSync(file, join(cwd, "link.ts"));
  symlinkSync(join(outside, "missing.ts"), join(cwd, "dangling.ts"));
  symlinkSync(join(outside, "missing"), join(cwd, "gone"));
  writeFileSync(join(cwd, ".gitignore"), "ignored.ts\n");

  const paths = ["link.ts", "dangling.ts", "gone/a.ts", "ignored.ts"].map((name) =>
    join(cwd, name),
  );

  for (const path of [file, ...paths])
    expect(writeHook(cwd, { file_path: path, content: before })).toEqual({
      code: 0,
      stderr: "",
      stdout: "",
    });

  expect(readFileSync(file, "utf8")).toBe(before);
});

test("PreToolUse with --hunks defers HEAD files and formats new files", () => {
  const cwd = scratchGitRepository({ files: { "a.ts": before }, staged: true });
  const commit = Bun.spawnSync(
    [
      "git",
      "-c",
      "commit.gpgsign=false",
      "-c",
      "user.email=t@t",
      "-c",
      "user.name=t",
      "commit",
      "-qm",
      "init",
    ],
    { cwd },
  );

  expect(commit.exitCode).toBe(0);

  expect(writeHook(cwd, { file_path: join(cwd, "a.ts"), content: before }, ["--hunks"])).toEqual({
    code: 0,
    stderr: "",
    stdout: "",
  });

  const result = writeHook(cwd, { file_path: join(cwd, "new.ts"), content: before }, ["--hunks"]);
  expect(result.code).toBe(0);
  expect(JSON.parse(result.stdout).hookSpecificOutput.updatedInput.content).toBe(after);
});

test("PreToolUse applies --no-braces and ignores other hook events", () => {
  const cwd = scratchGitRepository();
  const path = join(cwd, "a.ts");
  const kept = writeHook(cwd, { file_path: path, content: before }, ["--no-braces"]);
  expect(kept.code).toBe(0);
  expect(JSON.parse(kept.stdout).hookSpecificOutput.updatedInput.content).toContain(
    "if (owner) { mark(items[0]); }",
  );

  writeFileSync(path, before);

  const ignored = writeHook(cwd, { file_path: path, content: before }, [], "PostToolUse");
  expect(ignored).toEqual({ code: 0, stderr: "", stdout: "" });
  expect(readFileSync(path, "utf8")).toBe(before);
});

test("blockReason is absent without findings even when files were rewritten", () => {
  expect(blockReason([], ["bodies.ts"])).toBeUndefined();
});

test("blockReason prints the exact finding, summary and instruction", () => {
  expect(blockReason([finding()], [])).toBe(
    "stanza could not fix these in the files you changed:\n" +
      "  wall.ts:2:3 wall 6 or more statements with no blank line between them; separate the steps\n\n" +
      "wall: six or more consecutive single-line statements with no blank line\n\n" +
      "Fix those findings, then reply again.",
  );
});

test("blockReason caps finding lines at twelve and reports the remaining count", () => {
  const findings = Array.from({ length: 14 }, (_, index) => finding(`file${index}.ts`));
  const reason = blockReason(findings, ["file12.ts"]);

  expect(reason?.split("\n").filter((line) => line.startsWith("  file"))).toHaveLength(12);
  expect(reason).toContain("  file11.ts:2:3 wall ");
  expect(reason).not.toContain("  file12.ts");
  expect(reason).toEndWith("stanza rewrote file12.ts, so read it again before editing.");
  expect(reason).toContain("\n  ... and 2 more\n\n");

  expect(blockReason(findings.slice(0, 12), [])).not.toContain("... and");
  expect(blockReason(findings, ["other.ts"])).not.toContain("stanza rewrote");
});

test("blockReason gives parse findings their own message without a rule summary", () => {
  const parse = { ...finding("broken.ts", "parse"), line: 1, col: 10 };
  expect(blockReason([parse], [])).toBe(
    "stanza could not fix these in the files you changed:\n" +
      "  broken.ts:1:10 could not read or parse this file: Unexpected token\n\n" +
      "Fix those findings, then reply again.",
  );
});

test("blockReason tells an engine failure apart from a parse failure", () => {
  const failed = { ...finding("deep.ts", "parse"), rule: "error" as const, message: "RangeError" };
  expect(blockReason([failed], [])).toContain("deep.ts:2:3 stanza failed on this file: RangeError");
});

test("blockReason lists each rule once in catalog order", () => {
  const findings = [finding(), finding("steps.ts", "block-spacing"), finding("other.ts")];
  const reason = blockReason(findings, []);
  expect(reason?.split("\n\n")[1]).toBe(
    `block-spacing: ${RULES["block-spacing"].summary}\nwall: ${RULES.wall.summary}`,
  );
});

test("blockReason names only rewritten files with shown findings", () => {
  expect(blockReason([finding()], ["bodies.ts"])).not.toContain("stanza rewrote");
  expect(blockReason([finding()], ["bodies.ts", "wall.ts"])).toEndWith(
    "\n\nstanza rewrote wall.ts, so read it again before editing.",
  );

  const findings = [finding("a.ts"), finding("b.ts"), finding("c.ts")];
  expect(blockReason(findings, ["a.ts", "b.ts"])).toEndWith(
    "\n\nstanza rewrote a.ts and b.ts, so read them again before editing.",
  );

  expect(blockReason(findings, ["a.ts", "b.ts", "c.ts", "clean.ts"])).toEndWith(
    "\n\nstanza rewrote a.ts, b.ts and c.ts, so read them again before editing.",
  );
});

test("PreToolUse reports a git failure without denying the Write", () => {
  const cwd = scratchGitRepository();
  const env = {
    ...process.env,
    GIT_CONFIG_GLOBAL: "/dev/null",
    GIT_CONFIG_NOSYSTEM: "1",
    GIT_TEST_ASSUME_DIFFERENT_OWNER: "1",
  };

  const input = {
    cwd,
    hook_event_name: "PreToolUse",
    tool_name: "Write",
    tool_input: { file_path: join(cwd, "a.ts"), content: before },
  };

  const result = run({ cwd, env, stdin: Buffer.from(JSON.stringify(input)) }, "hook");
  expect(result.code).toBe(1);
  expect(result.stdout).toBe("");
  expect(result.stderr).toContain("dubious ownership");
});
