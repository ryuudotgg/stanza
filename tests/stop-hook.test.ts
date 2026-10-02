import { expect, test } from "bun:test";
import {
  chmodSync,
  copyFileSync,
  mkdirSync,
  readFileSync,
  realpathSync,
  symlinkSync,
  writeFileSync,
} from "node:fs";
import { basename, dirname, join } from "node:path";
import { writtenFiles } from "../src/hook.ts";
import { runMain, scratch, scratchGitRepository } from "./support.ts";

const root = join(import.meta.dir, "..");
const fixture = join(root, "tests", "fixtures", "braces", "bodies.before.ts");
function repository(
  files: Record<string, string> = { "a.ts": readFileSync(fixture, "utf8") },
): string {
  return scratchGitRepository({ files, staged: true });
}

function braces(text: string): number {
  return text.split("{").length - 1;
}

function hookEnv(): Record<string, string | undefined> {
  return { ...process.env, AGENT_HOOKS: "1" };
}

interface CommandResult {
  exitCode: number;
  stdout?: Uint8Array;
  stderr?: Uint8Array;
}

function output(result: CommandResult): string {
  return new TextDecoder().decode(result.stdout);
}

function hookCommand(
  input: string,
  args: string[] = [],
  env: Record<string, string | undefined> = { ...hookEnv(), FORCE_COLOR: "3" },
): CommandResult {
  const result = runMain(["hook", ...args], {
    cwd: root,
    env,
    stdin: new TextEncoder().encode(input),
  });

  const encoder = new TextEncoder();
  return {
    exitCode: result.code,
    stdout: encoder.encode(result.stdout),
    stderr: encoder.encode(result.stderr),
  };
}

function wallAndBodies(): Record<string, string> {
  return {
    "wall.ts": readFileSync(join(root, "tests", "fixtures", "wall", "wall.before.ts"), "utf8"),
    "bodies.ts": readFileSync(fixture, "utf8"),
  };
}

test("stanza hook fixes the input repository and blocks only on remaining findings", () => {
  const cwd = repository(wallAndBodies());
  const result = hookCommand(JSON.stringify({ cwd }));
  const lines = output(result).split("\n");
  expect(lines).toHaveLength(2);
  expect(lines[1]).toBe("");

  const decision = JSON.parse(lines[0]!);
  expect(decision.decision).toBe("block");
  expect(decision.reason).toContain("wall.ts:2:3 wall ");
  expect(decision.reason).not.toContain("braces");
  expect(decision.reason).not.toContain("bodies.ts");

  expect(readFileSync(join(cwd, "bodies.ts"))).toEqual(
    readFileSync(join(root, "tests", "fixtures", "braces", "bodies.after.ts")),
  );

  expect(result.exitCode).toBe(0);
  expect(new TextDecoder().decode(result.stderr)).toBe("");
});

const bodiesAfter = join(root, "tests", "fixtures", "braces", "bodies.after.ts");

function transcript(...lines: unknown[]): string {
  const path = join(scratch("hook-transcript"), "stop.jsonl");
  const text = lines.map((line) => (typeof line === "string" ? line : JSON.stringify(line)));
  writeFileSync(path, text.join("\n"));

  return path;
}

function subagentTranscript(main: string, agent: string, ...lines: unknown[]): void {
  const dir = join(dirname(main), basename(main, ".jsonl"), "subagents");
  const text = lines.map((line) => (typeof line === "string" ? line : JSON.stringify(line)));
  mkdirSync(dir, { recursive: true });
  writeFileSync(join(dir, `${agent}.jsonl`), text.join("\n"));
}

function toolCalls(...calls: [name: string, path: string, failed?: boolean][]): unknown[] {
  const uses = calls.map(([name, file_path], index) => ({
    type: "tool_use",
    id: `t${index}`,
    name,
    input: { file_path },
  }));

  const results = calls.map(([, , failed], index) => ({
    type: "tool_result",
    tool_use_id: `t${index}`,
    content: failed ? "String to replace not found in file." : "ok",
    is_error: failed ?? false,
  }));

  return [
    { type: "assistant", message: { role: "assistant", content: uses } },
    { type: "user", message: { role: "user", content: results } },
  ];
}

const userTurn = { type: "user", message: { role: "user", content: "hi" } };

function agentAndHuman(): string {
  const before = readFileSync(fixture, "utf8");
  return repository({ "human.ts": before, "agent.ts": before });
}

function expectSilent(result: CommandResult): void {
  expect(output(result)).toBe("");
  expect(new TextDecoder().decode(result.stderr)).toBe("");
  expect(result.exitCode).toBe(0);
}

function codexRollout0159(cwd: string, ...records: unknown[]): string {
  return transcript(
    {
      timestamp: "2026-09-30T00:00:00Z",
      type: "session_meta",
      payload: { id: "session-1", cwd, cli_version: "0.159.0", source: "exec" },
    },
    { type: "event_msg", payload: { type: "task_started", turn_id: "turn-1" } },
    ...records,
  );
}

function codexExec(path: string, applied = true): unknown[] {
  const patch = `*** Begin Patch\n*** Update File: ${path}\n@@\n-old\n+new\n*** End Patch`;
  return [
    {
      type: "response_item",
      payload: {
        type: "custom_tool_call",
        status: "completed",
        call_id: `call_${basename(path)}`,
        name: "exec",
        input: `const patch = ${JSON.stringify(patch)};\nconst result = await tools.apply_patch(patch);\ntext(result);\n`,
      },
    },
    {
      type: "response_item",
      payload: {
        type: "custom_tool_call_output",
        call_id: `call_${basename(path)}`,
        output: [
          { type: "input_text", text: applied ? "Script completed\n" : "Script failed\n" },
          {
            type: "input_text",
            text: applied
              ? `Success. Updated the following files:\nM ${path}\n`
              : "Script error:\npatch rejected: writing is blocked by read-only sandbox; rejected by user approval settings",
          },
        ],
      },
    },
  ];
}

function codexFileChange(changes: Record<string, unknown>, status = "completed"): unknown {
  return {
    type: "event_msg",
    payload: {
      type: "item_completed",
      thread_id: "thread-1",
      turn_id: "turn-1",
      item: {
        type: "FileChange",
        id: "exec-1",
        changes,
        status,
        stdout: "Success. Updated the following files:\n",
        stderr: "",
      },
      started_at_ms: 1,
      completed_at_ms: 2,
    },
  };
}

test("stanza hook reads a rollout captured from Codex 0.159.0", () => {
  const before = readFileSync(fixture, "utf8");
  const cwd = repository({ "src/agent.ts": before, "human.ts": before });
  const captured = readFileSync(join(root, "tests", "fixtures", "codex", "rollout-0.159.0.jsonl"));
  const path = transcript(...captured.toString().trimEnd().replaceAll("{{cwd}}", cwd).split("\n"));

  expect([...(writtenFiles(path) ?? [])]).toEqual([realpathSync(join(cwd, "src", "agent.ts"))]);

  const result = hookCommand(JSON.stringify({ cwd, transcript_path: path }));

  expect(readFileSync(join(cwd, "src", "agent.ts"))).toEqual(readFileSync(bodiesAfter));
  expect(readFileSync(join(cwd, "human.ts"))).toEqual(readFileSync(fixture));
  expectSilent(result);
});

test("stanza hook scopes Codex 0.159.0 Stop to completed file changes", () => {
  const cwd = agentAndHuman();
  const agent = join(cwd, "agent.ts");
  const path = codexRollout0159(
    cwd,
    ...codexExec(agent),
    codexFileChange({ [agent]: { type: "update", unified_diff: "@@", move_path: null } }),
  );

  const result = hookCommand(JSON.stringify({ cwd, transcript_path: path }));

  expect(readFileSync(agent)).toEqual(readFileSync(bodiesAfter));
  expect(readFileSync(join(cwd, "human.ts"))).toEqual(readFileSync(fixture));
  expectSilent(result);
});

test("stanza hook ignores rejected and unfinished Codex patches", () => {
  const before = readFileSync(fixture, "utf8");
  const cwd = repository({ "agent.ts": before, "human.ts": before, "pending.ts": before });
  const agent = join(cwd, "agent.ts");
  const human = join(cwd, "human.ts");
  const pending = join(cwd, "pending.ts");
  const path = codexRollout0159(
    cwd,
    ...codexExec(agent),
    codexFileChange({ [agent]: { type: "update", unified_diff: "@@", move_path: null } }),
    ...codexExec(human, false),
    codexFileChange(
      { [pending]: { type: "update", unified_diff: "@@", move_path: null } },
      "in_progress",
    ),
  );

  const result = hookCommand(JSON.stringify({ cwd, transcript_path: path }));

  expect(readFileSync(agent)).toEqual(readFileSync(bodiesAfter));
  expect(readFileSync(human)).toEqual(readFileSync(fixture));
  expect(readFileSync(pending)).toEqual(readFileSync(fixture));
  expectSilent(result);
});

test("stanza hook fixes the destination of a Codex move", () => {
  const cwd = agentAndHuman();
  const agent = join(cwd, "agent.ts");
  const old = join(cwd, "old.ts");
  const path = codexRollout0159(
    cwd,
    ...codexExec(agent),
    codexFileChange({ [old]: { type: "update", unified_diff: "@@", move_path: agent } }),
  );

  const result = hookCommand(JSON.stringify({ cwd, transcript_path: path }));

  expect(readFileSync(agent)).toEqual(readFileSync(bodiesAfter));
  expect(readFileSync(join(cwd, "human.ts"))).toEqual(readFileSync(fixture));
  expectSilent(result);
});

test("stanza hook falls back to changed files when a Codex rollout names none yet", () => {
  const cwd = agentAndHuman();
  const path = codexRollout0159(cwd, {
    type: "event_msg",
    payload: { type: "item_completed", item: { type: "AgentMessage", status: "completed" } },
  });

  const result = hookCommand(JSON.stringify({ cwd, transcript_path: path }));

  expect(readFileSync(join(cwd, "agent.ts"))).toEqual(readFileSync(bodiesAfter));
  expect(readFileSync(join(cwd, "human.ts"))).toEqual(readFileSync(bodiesAfter));
  expectSilent(result);
});

function capturedStop(cwd: string, transcriptPath: string): string {
  return readFileSync(join(root, "tests", "fixtures", "codex", "stop-0.159.0.json"), "utf8")
    .replace("{{cwd}}", cwd)
    .replace("{{transcript}}", transcriptPath);
}

test("stanza hook reads a captured Codex 0.159.0 Stop payload with --hunks", () => {
  const cwd = agentAndHuman();
  const agent = join(cwd, "agent.ts");
  const path = codexRollout0159(
    cwd,
    ...codexExec(agent),
    codexFileChange({ [agent]: { type: "update", unified_diff: "@@", move_path: null } }),
  );

  const result = hookCommand(capturedStop(cwd, path), ["--hunks"]);

  expect(readFileSync(agent)).toEqual(readFileSync(bodiesAfter));
  expect(readFileSync(join(cwd, "human.ts"))).toEqual(readFileSync(fixture));
  expectSilent(result);
});

test("stanza hook matches a rollout path through a symlink to a real cwd", () => {
  const cwd = agentAndHuman();
  const linkedCwd = join(scratch("hook-codex-tmp"), "repo");
  symlinkSync(cwd, linkedCwd, "dir");

  const agent = join(linkedCwd, "agent.ts");
  const path = codexRollout0159(
    linkedCwd,
    ...codexExec(agent),
    codexFileChange({ [agent]: { type: "add", content: "" } }),
  );

  const result = hookCommand(capturedStop(realpathSync(cwd), path));

  expect(readFileSync(join(cwd, "agent.ts"))).toEqual(readFileSync(bodiesAfter));
  expect(readFileSync(join(cwd, "human.ts"))).toEqual(readFileSync(fixture));
  expectSilent(result);
});

test("stanza hook fixes a file the agent wrote in a repository other than cwd", () => {
  const cwd = agentAndHuman();
  const other = agentAndHuman();
  const path = transcript(userTurn, ...toolCalls(["Write", join(other, "agent.ts")]));

  const result = hookCommand(JSON.stringify({ cwd, transcript_path: path }));

  expect(readFileSync(join(other, "agent.ts"))).toEqual(readFileSync(bodiesAfter));
  expect(readFileSync(join(other, "human.ts"))).toEqual(readFileSync(fixture));

  for (const name of ["agent.ts", "human.ts"])
    expect(readFileSync(join(cwd, name))).toEqual(readFileSync(fixture));

  expectSilent(result);
});

test("stanza hook keeps fixing when a patched file's directory was deleted", () => {
  const cwd = agentAndHuman();
  const agent = join(cwd, "agent.ts");
  const gone = join(cwd, "gone", "a.ts");
  const path = codexRollout0159(
    cwd,
    ...codexExec(agent),
    codexFileChange({
      [gone]: { type: "add", content: "" },
      [agent]: { type: "update", unified_diff: "@@", move_path: null },
    }),
  );

  const result = hookCommand(JSON.stringify({ cwd, transcript_path: path }));

  expect(readFileSync(agent)).toEqual(readFileSync(bodiesAfter));
  expect(readFileSync(join(cwd, "human.ts"))).toEqual(readFileSync(fixture));
  expectSilent(result);
});

test("stanza hook does not follow a written symlink into another repository", () => {
  const cwd = agentAndHuman();
  const other = agentAndHuman();
  const link = join(cwd, "link.ts");
  symlinkSync(join(other, "agent.ts"), link);

  const rollout = codexRollout0159(
    cwd,
    ...codexExec(link),
    codexFileChange({ [link]: { type: "update", unified_diff: "@@", move_path: null } }),
  );

  const claude = transcript(userTurn, ...toolCalls(["Edit", link]));
  for (const path of [rollout, claude])
    expectSilent(hookCommand(JSON.stringify({ cwd, transcript_path: path })));

  for (const repository of [cwd, other])
    for (const name of ["agent.ts", "human.ts"])
      expect(readFileSync(join(repository, name))).toEqual(readFileSync(fixture));
});

test("stanza hook falls back when a Codex rollout has no completed item event", () => {
  const cwd = agentAndHuman();
  const path = codexRollout0159(cwd);
  const result = hookCommand(JSON.stringify({ cwd, transcript_path: path }));

  expect(readFileSync(join(cwd, "agent.ts"))).toEqual(readFileSync(bodiesAfter));
  expect(readFileSync(join(cwd, "human.ts"))).toEqual(readFileSync(bodiesAfter));
  expectSilent(result);
});

test("stanza hook maps Codex paths through a symlinked cwd", () => {
  const cwd = agentAndHuman();
  const linkedCwd = join(scratch("hook-codex-link"), "repo");
  symlinkSync(cwd, linkedCwd, "dir");

  const agent = join(linkedCwd, "agent.ts");
  const path = codexRollout0159(
    linkedCwd,
    ...codexExec(agent),
    codexFileChange({ [agent]: { type: "update", unified_diff: "@@", move_path: null } }),
  );

  const result = hookCommand(JSON.stringify({ cwd: linkedCwd, transcript_path: path }));

  expect(readFileSync(join(cwd, "agent.ts"))).toEqual(readFileSync(bodiesAfter));
  expect(readFileSync(join(cwd, "human.ts"))).toEqual(readFileSync(fixture));
  expectSilent(result);
});

test("stanza hook fixes a file Codex patched through a symlinked directory", () => {
  const before = readFileSync(fixture, "utf8");
  const cwd = repository({ "packages/x/agent.ts": before, "human.ts": before });
  symlinkSync(join(cwd, "packages", "x"), join(cwd, "lib"), "dir");

  const agent = join(cwd, "lib", "agent.ts");
  const path = codexRollout0159(
    cwd,
    ...codexExec(agent),
    codexFileChange({ [agent]: { type: "update", unified_diff: "@@", move_path: null } }),
  );

  const result = hookCommand(JSON.stringify({ cwd, transcript_path: path }));

  expect(readFileSync(join(cwd, "packages", "x", "agent.ts"))).toEqual(readFileSync(bodiesAfter));
  expect(readFileSync(join(cwd, "human.ts"))).toEqual(readFileSync(fixture));
  expectSilent(result);
});

test("stanza hook fixes only the files the transcript says the agent wrote", () => {
  const cwd = agentAndHuman();
  const path = transcript(userTurn, ...toolCalls(["Edit", join(cwd, "agent.ts")]));
  const result = hookCommand(JSON.stringify({ cwd, transcript_path: path }));

  expect(readFileSync(join(cwd, "human.ts"))).toEqual(readFileSync(fixture));
  expect(readFileSync(join(cwd, "agent.ts"))).toEqual(readFileSync(bodiesAfter));
  expectSilent(result);
});

test("stanza hook fixes files written by subagents", () => {
  const before = readFileSync(fixture, "utf8");
  const cwd = repository({
    "main.ts": before,
    "sub.ts": before,
    "other.ts": before,
    "human.ts": before,
  });

  const path = transcript(userTurn, ...toolCalls(["Edit", join(cwd, "main.ts")]));
  subagentTranscript(path, "agent-a", userTurn, ...toolCalls(["Write", join(cwd, "sub.ts")]));
  subagentTranscript(path, "agent-b", userTurn, ...toolCalls(["Edit", join(cwd, "other.ts")]));

  const result = hookCommand(JSON.stringify({ cwd, transcript_path: path }));

  expect(readFileSync(join(cwd, "main.ts"))).toEqual(readFileSync(bodiesAfter));
  expect(readFileSync(join(cwd, "sub.ts"))).toEqual(readFileSync(bodiesAfter));
  expect(readFileSync(join(cwd, "other.ts"))).toEqual(readFileSync(bodiesAfter));
  expect(readFileSync(join(cwd, "human.ts"))).toEqual(readFileSync(fixture));
  expectSilent(result);
});

test("stanza hook skips failed subagent writes", () => {
  const before = readFileSync(fixture, "utf8");
  const cwd = repository({ "main.ts": before, "sub.ts": before });
  const path = transcript(userTurn, ...toolCalls(["Edit", join(cwd, "main.ts")]));
  subagentTranscript(path, "agent-a", userTurn, ...toolCalls(["Write", join(cwd, "sub.ts"), true]));

  const result = hookCommand(JSON.stringify({ cwd, transcript_path: path }));

  expect(readFileSync(join(cwd, "main.ts"))).toEqual(readFileSync(bodiesAfter));
  expect(readFileSync(join(cwd, "sub.ts"))).toEqual(readFileSync(fixture));
  expectSilent(result);
});

test("stanza hook tolerates missing and malformed subagent transcripts", () => {
  for (const subagent of [undefined, [userTurn, "not json"]]) {
    const before = readFileSync(fixture, "utf8");
    const cwd = repository({ "main.ts": before, "human.ts": before });
    const path = transcript(userTurn, ...toolCalls(["Edit", join(cwd, "main.ts")]));
    if (subagent !== undefined)
      subagentTranscript(
        path,
        "agent-a",
        ...subagent,
        ...toolCalls(["Write", join(cwd, "main.ts")]),
      );

    const result = hookCommand(JSON.stringify({ cwd, transcript_path: path }));

    expect(readFileSync(join(cwd, "main.ts"))).toEqual(readFileSync(bodiesAfter));
    expect(readFileSync(join(cwd, "human.ts"))).toEqual(readFileSync(fixture));
    expectSilent(result);
  }
});

test("stanza hook fixes a file whose edit result the transcript does not hold yet", () => {
  const cwd = agentAndHuman();
  const [edit] = toolCalls(["Edit", join(cwd, "agent.ts")]);
  const result = hookCommand(JSON.stringify({ cwd, transcript_path: transcript(userTurn, edit) }));

  expect(readFileSync(join(cwd, "human.ts"))).toEqual(readFileSync(fixture));
  expect(readFileSync(join(cwd, "agent.ts"))).toEqual(readFileSync(bodiesAfter));
  expectSilent(result);
});

test("stanza hook reads a transcript written with spaced JSON", () => {
  const cwd = agentAndHuman();
  const reply = { type: "assistant", message: { role: "assistant", content: [{ type: "text" }] } };
  const records = [
    userTurn,
    reply,
    ...toolCalls(["Edit", join(cwd, "agent.ts")], ["Edit", join(cwd, "human.ts"), true]),
  ];

  const spaced = records.map((record) => JSON.stringify(record).replaceAll('":', '": '));
  const result = hookCommand(JSON.stringify({ cwd, transcript_path: transcript(...spaced) }));

  expect(readFileSync(join(cwd, "human.ts"))).toEqual(readFileSync(fixture));
  expect(readFileSync(join(cwd, "agent.ts"))).toEqual(readFileSync(bodiesAfter));
  expectSilent(result);
});

test("stanza hook falls back to changed files without transcript_path", () => {
  const cwd = agentAndHuman();
  const result = hookCommand(JSON.stringify({ cwd }));

  expect(readFileSync(join(cwd, "human.ts"))).toEqual(readFileSync(bodiesAfter));
  expect(readFileSync(join(cwd, "agent.ts"))).toEqual(readFileSync(bodiesAfter));
  expectSilent(result);
});

test("stanza hook falls back to changed files when it cannot read the transcript", () => {
  for (const path of [transcript('{"type":"response_item"}'), "/nonexistent/stop.jsonl"]) {
    const cwd = agentAndHuman();
    const result = hookCommand(JSON.stringify({ cwd, transcript_path: path }));

    expect(readFileSync(join(cwd, "human.ts"))).toEqual(readFileSync(bodiesAfter));
    expectSilent(result);
  }
});

test("stanza hook fixes nothing when the transcript holds no file writes", () => {
  const cwd = agentAndHuman();
  const path = transcript(userTurn, ...toolCalls(["Read", join(cwd, "agent.ts")]));
  const result = hookCommand(JSON.stringify({ cwd, transcript_path: path }));

  expect(readFileSync(join(cwd, "human.ts"))).toEqual(readFileSync(fixture));
  expect(readFileSync(join(cwd, "agent.ts"))).toEqual(readFileSync(fixture));
  expectSilent(result);
});

test("stanza hook skips transcript lines and paths it cannot use", () => {
  const cwd = agentAndHuman();
  const outside = join(scratch("hook-outside-file"), "outside.ts");
  copyFileSync(fixture, outside);

  const path = transcript(
    userTurn,
    ...toolCalls(
      ["Edit", join(cwd, "agent.ts")],
      ["Edit", join(cwd, "human.ts"), true],
      ["Write", join(cwd, "missing.ts")],
      ["Write", outside],
    ),
    "not json",
  );

  const result = hookCommand(JSON.stringify({ cwd, transcript_path: path }));

  expect(readFileSync(join(cwd, "agent.ts"))).toEqual(readFileSync(bodiesAfter));
  expect(readFileSync(join(cwd, "human.ts"))).toEqual(readFileSync(fixture));
  expect(readFileSync(outside)).toEqual(readFileSync(fixture));
  expectSilent(result);
});

test("stanza hook leaves files untouched when stop_hook_active is true", () => {
  const files = wallAndBodies();
  const cwd = repository(files);
  const result = hookCommand(JSON.stringify({ cwd, stop_hook_active: true }));

  expect(output(result)).toBe("");
  expect(new TextDecoder().decode(result.stderr)).toBe("");
  expect(result.exitCode).toBe(0);

  for (const [path, text] of Object.entries(files))
    expect(readFileSync(join(cwd, path))).toEqual(Buffer.from(text));
});

test("stanza hook reports parse failures together with wall findings", () => {
  const cwd = repository({ ...wallAndBodies(), "broken.ts": "function (\n" });
  const result = hookCommand(JSON.stringify({ cwd }));
  const decision = JSON.parse(output(result));

  expect(decision.decision).toBe("block");
  expect(decision.reason).toStartWith("stanza could not fix these in the files you changed:");
  expect(decision.reason).toContain("broken.ts:1:10 could not read or parse this file: ");
  expect(decision.reason).toContain("wall.ts:2:3 wall ");
  expect(readFileSync(join(cwd, "broken.ts"), "utf8")).toBe("function (\n");

  expect(result.exitCode).toBe(0);
  expect(new TextDecoder().decode(result.stderr)).toBe("");
});

test("stanza hook with AGENT_HOOKS=0 ignores invalid input and flags", () => {
  const result = hookCommand("not json", ["--bad"], { ...hookEnv(), AGENT_HOOKS: "0" });
  expect(output(result)).toBe("");
  expect(new TextDecoder().decode(result.stderr)).toBe("");
  expect(result.exitCode).toBe(0);
});

test("stanza hook silently skips outside and missing directories", () => {
  const outside = scratch("hook-outside");
  for (const cwd of [outside, join(outside, "missing")]) {
    const result = hookCommand(JSON.stringify({ cwd }));
    expect(output(result)).toBe("");
    expect(new TextDecoder().decode(result.stderr)).toBe("");
    expect(result.exitCode).toBe(0);
  }
});

test("stanza hook rejects malformed stdin without a block decision", () => {
  const result = hookCommand("not json");
  expect(output(result)).toBe("");
  expect(new TextDecoder().decode(result.stderr)).toContain(
    "stanza hook: input must be valid JSON",
  );

  expect(result.exitCode).toBe(1);
});

test("stanza hook rejects unknown and repeated flags before reading input", () => {
  for (const args of [["--fix"], ["--no-braces", "--no-braces"], ["file.ts"]]) {
    const result = hookCommand("not json", args);
    expect(output(result)).toBe("");
    expect(new TextDecoder().decode(result.stderr)).toContain(
      "stanza hook [--braces | --no-braces] [--hunks]",
    );

    expect(new TextDecoder().decode(result.stderr)).toStartWith(
      `stanza hook: unexpected argument ${args.at(-1)}\n`,
    );

    expect(result.exitCode).toBe(1);
  }
});

test("stanza hook rejects --braces with --no-braces", () => {
  const result = hookCommand("not json", ["--braces", "--no-braces"]);
  expect(output(result)).toBe("");
  expect(new TextDecoder().decode(result.stderr)).toStartWith(
    "stanza hook: use one of --braces or --no-braces\n",
  );

  expect(result.exitCode).toBe(1);
});

test("stanza hook stays silent after fixing every finding", () => {
  const cwd = repository();
  const result = hookCommand(JSON.stringify({ cwd }));
  expect(output(result)).toBe("");
  expect(new TextDecoder().decode(result.stderr)).toBe("");
  expect(result.exitCode).toBe(0);

  expect(readFileSync(join(cwd, "a.ts"))).toEqual(
    readFileSync(join(root, "tests", "fixtures", "braces", "bodies.after.ts")),
  );
});

test("stanza hook reports a git selection failure as a non-blocking error", () => {
  const files = wallAndBodies();
  const cwd = repository(files);
  writeFileSync(join(cwd, ".git", "index"), "junkjunkjunkjunkjunk");

  const result = hookCommand(JSON.stringify({ cwd }));
  expect(output(result)).toBe("");
  expect(new TextDecoder().decode(result.stderr)).toContain("git ls-files failed");
  expect(result.exitCode).toBe(1);

  for (const [path, text] of Object.entries(files))
    expect(readFileSync(join(cwd, path), "utf8")).toBe(text);
});

test("stanza hook reports a repository git refuses as a non-blocking error", () => {
  const cwd = repository();
  const result = hookCommand(JSON.stringify({ cwd }), [], {
    ...hookEnv(),
    GIT_CONFIG_GLOBAL: "/dev/null",
    GIT_CONFIG_NOSYSTEM: "1",
    GIT_TEST_ASSUME_DIFFERENT_OWNER: "1",
  });

  expect(output(result)).toBe("");
  expect(new TextDecoder().decode(result.stderr)).toContain("dubious ownership");
  expect(result.exitCode).toBe(1);
});

test("stanza hook --no-braces keeps braces and still applies blank line fixes", () => {
  const original = readFileSync(fixture, "utf8");
  const cwd = repository();
  const result = hookCommand(JSON.stringify({ cwd }), ["--no-braces"]);
  const fixed = readFileSync(join(cwd, "a.ts"), "utf8");

  expect(result.exitCode).toBe(0);
  expect(braces(fixed)).toBe(braces(original));
  expect(fixed).not.toBe(original);
});

test("stanza hook --braces overrides a braces enforcing config", () => {
  const source = readFileSync(fixture, "utf8");
  const cwd = repository({ ".oxlintrc.json": '{ "rules": { "curly": "error" } }', "a.ts": source });
  const result = hookCommand(JSON.stringify({ cwd }), ["--braces"]);

  expect(result.exitCode).toBe(0);
  expect(braces(readFileSync(join(cwd, "a.ts"), "utf8"))).toBeLessThan(braces(source));
  expect(new TextDecoder().decode(result.stderr)).toBe("");
});

test("stanza hook accepts --hunks in either flag order and rejects repeats", () => {
  for (const args of [["--hunks"], ["--hunks", "--no-braces"], ["--no-braces", "--hunks"]]) {
    const cwd = repository();
    const result = hookCommand(JSON.stringify({ cwd }), args);
    expect(result.exitCode).toBe(0);
    expect(new TextDecoder().decode(result.stderr)).toBe("");
    expect(readFileSync(join(cwd, "a.ts"), "utf8")).not.toBe(readFileSync(fixture, "utf8"));
  }

  const repeated = hookCommand("not json", ["--hunks", "--hunks"]);
  expect(repeated.exitCode).toBe(1);
  expect(new TextDecoder().decode(repeated.stderr)).toStartWith(
    "stanza hook: unexpected argument --hunks\n",
  );
});

test("stanza hook --hunks leaves unchanged code in an edited file alone", () => {
  const block = (name: string, value: number) =>
    `function ${name}(a: boolean) {\n  if (a) {\n    return 1;\n  }\n  return ${value};\n}\n`;

  const cwd = repository({ "a.ts": `${block("f1", 2)}\n${block("f2", 2)}` });
  for (const args of [
    ["config", "commit.gpgsign", "false"],
    ["-c", "user.email=t@t", "-c", "user.name=t", "commit", "-qm", "init"],
  ])
    expect(Bun.spawnSync(["git", ...args], { cwd }).exitCode).toBe(0);

  writeFileSync(join(cwd, "a.ts"), `${block("f1", 2)}\n${block("f2", 3)}`);
  expect(hookCommand(JSON.stringify({ cwd }), ["--hunks"]).exitCode).toBe(0);

  const fixed = readFileSync(join(cwd, "a.ts"), "utf8");
  expect(fixed).toStartWith(block("f1", 2));
  expect(fixed).toContain("  if (a)\n    return 1;\n  return 3;");
});

test("stanza hook asks to reread a file it rewrote that still has a finding", () => {
  const wall = readFileSync(join(root, "tests", "fixtures", "wall", "wall.before.ts"), "utf8");
  const cwd = repository({ "mixed.ts": `${wall}\n${readFileSync(fixture, "utf8")}` });
  const result = hookCommand(JSON.stringify({ cwd }));
  const reason = JSON.parse(output(result)).reason;

  expect(reason).toContain("mixed.ts:2:3 wall ");
  expect(reason).toEndWith("stanza rewrote mixed.ts, so read it again before editing.");
});

test.skipIf(process.getuid?.() === 0)(
  "stanza hook reports a file it could not write and still checks the rest",
  () => {
    const wall = readFileSync(join(root, "tests", "fixtures", "wall", "wall.before.ts"), "utf8");
    const cwd = repository({ "a.ts": readFileSync(fixture, "utf8"), "z.ts": wall });
    chmodSync(join(cwd, "a.ts"), 0o444);

    const result = hookCommand(JSON.stringify({ cwd }));
    const reason = JSON.parse(output(result)).reason;

    expect(result.exitCode).toBe(0);
    expect(reason).toContain("a.ts:1:1 could not write the fixes to this file: ");
    expect(reason).toContain("z.ts:2:3 wall ");
    expect(readFileSync(join(cwd, "a.ts"), "utf8")).toBe(readFileSync(fixture, "utf8"));
  },
);
