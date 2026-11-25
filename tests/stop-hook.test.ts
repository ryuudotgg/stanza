import { expect, test } from "bun:test";
import {
  chmodSync,
  copyFileSync,
  linkSync,
  mkdirSync,
  readFileSync,
  realpathSync,
  rmSync,
  statSync,
  symlinkSync,
  writeFileSync,
} from "node:fs";
import { basename, dirname, join, relative } from "node:path";
import {
  committedSource,
  gitIn,
  hunkFunction,
  run,
  scratch,
  scratchGitRepository,
  tracedGit,
} from "./support.ts";

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
  const result = run({ cwd: root, env, stdin: new TextEncoder().encode(input) }, "hook", ...args);

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
  expect(decision.reason).toContain("\nwall.ts\n  line 2: split these 7 statements");
  expect(decision.reason).not.toContain("braces");
  expect(decision.reason).not.toContain("\nbodies.ts\n");
  expect(decision.reason).toContain(
    "\nstanza rewrote bodies.ts, so read it again before editing.\nFix the findings above",
  );

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
  const file = join(dirname(main), basename(main, ".jsonl"), "subagents", `${agent}.jsonl`);
  const text = lines.map((line) => (typeof line === "string" ? line : JSON.stringify(line)));
  mkdirSync(dirname(file), { recursive: true });
  writeFileSync(file, text.join("\n"));
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

test("Stop with only a package.json Write exits without spawning git", () => {
  const cwd = scratchGitRepository({ files: { "package.json": "{}\n" } });
  const transcriptPath = transcript(...toolCalls(["Write", join(cwd, "package.json")]));
  const { result, calls } = tracedGit((env) =>
    hookCommand(
      JSON.stringify({ cwd, hook_event_name: "Stop", transcript_path: transcriptPath }),
      [],
      { ...env, AGENT_HOOKS: "1", FORCE_COLOR: "3" },
    ),
  );

  expect(result.exitCode).toBe(0);
  expect(output(result)).toBe("");
  expect(new TextDecoder().decode(result.stderr)).toBe("");
  expect(calls).toEqual([]);
});

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

test("stanza hook touches nothing when a Codex rollout holds only a Reasoning completion", () => {
  const cwd = agentAndHuman();
  const path = codexRollout0159(cwd, {
    type: "event_msg",
    payload: { type: "item_completed", item: { type: "Reasoning" } },
  });

  const result = hookCommand(JSON.stringify({ cwd, transcript_path: path }));

  expect(readFileSync(join(cwd, "agent.ts"))).toEqual(readFileSync(fixture));
  expect(readFileSync(join(cwd, "human.ts"))).toEqual(readFileSync(fixture));
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

test("stanza hook touches nothing when a Codex rollout has no completed item event", () => {
  const cwd = agentAndHuman();
  const path = codexRollout0159(cwd);
  const result = hookCommand(JSON.stringify({ cwd, transcript_path: path }));

  expect(readFileSync(join(cwd, "agent.ts"))).toEqual(readFileSync(fixture));
  expect(readFileSync(join(cwd, "human.ts"))).toEqual(readFileSync(fixture));
  expectSilent(result);
});

test("stanza hook reads a transcript as Claude Code when a Codex record follows its first assistant record", () => {
  const cwd = agentAndHuman();
  const path = transcript(
    ...toolCalls(["Edit", join(cwd, "agent.ts")]),
    codexFileChange({ [join(cwd, "human.ts")]: { type: "add" } }),
  );

  const result = hookCommand(JSON.stringify({ cwd, transcript_path: path }));

  expect(readFileSync(join(cwd, "agent.ts"))).toEqual(readFileSync(bodiesAfter));
  expect(readFileSync(join(cwd, "human.ts"))).toEqual(readFileSync(fixture));
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

test("Stop counts Workflow agent writes and follows no transcript directory symlink", () => {
  const before = readFileSync(fixture, "utf8");
  const cwd = repository({
    "main.ts": before,
    "second.ts": before,
    "workflow.ts": before,
    "failed.ts": before,
    "other.ts": before,
  });

  const path = transcript(
    userTurn,
    ...toolCalls(["Edit", join(cwd, "main.ts")], ["Edit", join(cwd, "second.ts")]),
  );

  subagentTranscript(
    path,
    "workflows/wf_1/agent-a",
    userTurn,
    ...toolCalls(["Write", join(cwd, "workflow.ts")], ["Edit", join(cwd, "failed.ts"), true]),
  );

  const other = transcript(userTurn);
  subagentTranscript(
    other,
    "workflows/wf_2/agent-b",
    userTurn,
    ...toolCalls(["Edit", join(cwd, "other.ts")]),
  );

  symlinkSync(
    join(dirname(other), "stop", "subagents"),
    join(dirname(path), "stop", "subagents", "linked"),
  );

  const result = hookCommand(JSON.stringify({ cwd, transcript_path: path }));

  for (const name of ["main.ts", "second.ts", "workflow.ts"])
    expect(readFileSync(join(cwd, name))).toEqual(readFileSync(bodiesAfter));

  for (const name of ["failed.ts", "other.ts"])
    expect(readFileSync(join(cwd, name))).toEqual(readFileSync(fixture));

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

test("Stop on workers fixes a hard-linked file and an ordinary one", () => {
  const cwd = repository({ "a.ts": readFileSync(fixture, "utf8") });
  linkSync(join(cwd, "a.ts"), join(cwd, "twin.ts"));
  copyFileSync(fixture, join(cwd, "b.ts"));
  const result = hookCommand(JSON.stringify({ cwd }), [], { ...hookEnv(), STANZA_WORKERS: "2" });

  for (const name of ["a.ts", "twin.ts", "b.ts"])
    expect(readFileSync(join(cwd, name))).toEqual(readFileSync(bodiesAfter));

  expect(statSync(join(cwd, "a.ts")).ino).toBe(statSync(join(cwd, "twin.ts")).ino);
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

test.skipIf(process.getuid?.() === 0)(
  "stanza hook skips changed files it cannot reach in silence",
  () => {
    const cwd = agentAndHuman();
    mkdirSync(join(cwd, "plain"));
    copyFileSync(fixture, join(cwd, "plain", "u.ts"));
    chmodSync(join(cwd, "plain"), 0o000);

    try {
      const result = hookCommand(JSON.stringify({ cwd }));
      expect(readFileSync(join(cwd, "human.ts"))).toEqual(readFileSync(bodiesAfter));
      expectSilent(result);
    } finally {
      chmodSync(join(cwd, "plain"), 0o755);
    }
  },
);

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
  expect(decision.reason).toStartWith("These need a hand fix in the files you changed:\n\n");
  expect(decision.reason).toContain("\nbroken.ts\n  line 1: Expected function name (parse)\n");
  expect(decision.reason).toContain("\nwall.ts\n  line 2: ");
  expect(readFileSync(join(cwd, "broken.ts"), "utf8")).toBe("function (\n");

  expect(result.exitCode).toBe(0);
  expect(new TextDecoder().decode(result.stderr)).toBe("");
});

test("stanza hook gives a parse failure its own line in the reason", () => {
  const cwd = repository({ "broken.ts": "let x = ;\n" });
  const decision = JSON.parse(output(hookCommand(JSON.stringify({ cwd }))));

  expect(decision.reason).toBe(
    [
      "These need a hand fix in the files you changed:",
      "",
      "broken.ts",
      "  line 1: Unexpected token (parse)",
      "",
      "Fix the findings above, then reply again. `stanza explain <file>:<line>` shows why a rule applies.",
    ].join("\n"),
  );
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

test("Stop under a removed cwd fixes the files its transcript names and skips the fallback", () => {
  const repo = agentAndHuman();
  const cwd = join(repo, "gone");
  mkdirSync(cwd);
  rmSync(cwd, { recursive: true });

  expectSilent(hookCommand(JSON.stringify({ cwd })));

  for (const name of ["agent.ts", "human.ts"])
    expect(readFileSync(join(repo, name))).toEqual(readFileSync(fixture));

  const path = transcript(userTurn, ...toolCalls(["Edit", join(repo, "agent.ts")]));
  expectSilent(hookCommand(JSON.stringify({ cwd, transcript_path: path })));
  expect(readFileSync(join(repo, "agent.ts"))).toEqual(readFileSync(bodiesAfter));
  expect(readFileSync(join(repo, "human.ts"))).toEqual(readFileSync(fixture));
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
  for (const [args, problem] of [
    [["--bogus"], "unknown flag --bogus"],
    [["--fix"], "unknown flag --fix"],
    [["--no-braces", "--no-braces"], "--no-braces given twice"],
    [["file.ts"], "unexpected argument file.ts"],
    [["--", "--hunks"], "unexpected argument --hunks"],
  ] as const) {
    const result = hookCommand("not json", [...args]);
    expect(output(result)).toBe("");
    expect(new TextDecoder().decode(result.stderr)).toBe(
      `stanza hook: ${problem}\nrun \`stanza --help\` for usage\n`,
    );

    expect(result.exitCode).toBe(1);
  }
});

test("stanza hook names the closest of its own flags for a mistyped one", () => {
  const result = hookCommand("not json", ["--hunk"]);
  expect(output(result)).toBe("");
  expect(new TextDecoder().decode(result.stderr)).toBe(
    "stanza hook: unknown flag --hunk, did you mean --hunks?\nrun `stanza --help` for usage\n",
  );

  expect(result.exitCode).toBe(1);
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
  expect(new TextDecoder().decode(result.stderr)).toBe(
    "stanza hook: git ls-files failed: fatal: .git/index: index file smaller than expected\n",
  );

  expect(result.exitCode).toBe(1);

  for (const [path, text] of Object.entries(files))
    expect(readFileSync(join(cwd, path), "utf8")).toBe(text);
});

test("a failed repository does not cancel another repository", () => {
  const files = wallAndBodies();
  const control = repository(files);
  const cwd = repository(files);
  const broken = repository();
  writeFileSync(join(broken, ".git", "index"), "junkjunkjunkjunkjunk");

  const stop = (repo: string, ...extra: string[]) => {
    const paths = [...extra, ...Object.keys(files).map((name) => join(repo, name))];
    const calls = paths.map((path): [string, string] => ["Write", path]);
    const path = transcript(userTurn, ...toolCalls(...calls));
    return hookCommand(JSON.stringify({ cwd: repo, transcript_path: path }));
  };

  const expected = stop(control);
  expect(expected.exitCode).toBe(0);
  expect(JSON.parse(output(expected)).decision).toBe("block");

  const result = stop(cwd, join(broken, "a.ts"));

  expect(output(result)).toBe(output(expected));
  expect(new TextDecoder().decode(result.stderr)).toBe(
    `stanza hook: git ls-files failed in ${relative(realpathSync(cwd), realpathSync(broken))}: fatal: .git/index: index file smaller than expected\n`,
  );

  expect(result.exitCode).toBe(1);
  expect(readFileSync(join(cwd, "bodies.ts"))).toEqual(readFileSync(bodiesAfter));
  expect(readFileSync(join(broken, "a.ts"))).toEqual(readFileSync(fixture));
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
    "stanza hook: --hunks given twice\n",
  );
});

test("stanza hook --hunks leaves unchanged code in an edited file alone", () => {
  const cwd = committedSource(`${hunkFunction("f1")}\n\n${hunkFunction("f2")}\n`);
  writeFileSync(join(cwd, "a.ts"), `${hunkFunction("f1")}\n\n${hunkFunction("f2", 3)}\n`);
  expect(hookCommand(JSON.stringify({ cwd }), ["--hunks"]).exitCode).toBe(0);

  const fixed = readFileSync(join(cwd, "a.ts"), "utf8");
  expect(fixed).toStartWith(`${hunkFunction("f1")}\n`);
  expect(fixed).toContain("  if (a) return 1;\n  return 3;");
});

test("stanza hook asks to reread a file it rewrote that still has a finding", () => {
  const wall = readFileSync(join(root, "tests", "fixtures", "wall", "wall.before.ts"), "utf8");
  const cwd = repository({ "mixed.ts": `${wall}\n${readFileSync(fixture, "utf8")}` });
  const result = hookCommand(JSON.stringify({ cwd }));
  const reason = JSON.parse(output(result)).reason;

  expect(reason).toContain("\nmixed.ts\n  line 2: ");
  expect(reason).toContain("\nstanza rewrote mixed.ts, so read it again before editing.\n");
});

test.skipIf(process.getuid?.() === 0)(
  "stanza hook reports a file it cannot write and still checks the rest",
  () => {
    const wall = readFileSync(join(root, "tests", "fixtures", "wall", "wall.before.ts"), "utf8");
    const cwd = repository({ "a.ts": readFileSync(fixture, "utf8"), "z.ts": wall });
    chmodSync(join(cwd, "a.ts"), 0o444);

    const result = hookCommand(JSON.stringify({ cwd }));
    const reason = JSON.parse(output(result)).reason;

    expect(result.exitCode).toBe(0);
    expect(reason).toMatch(/\na\.ts\n  line 1: .+ \(write\)\n/);
    expect(reason).toContain("\nz.ts\n  line 2: ");
    expect(reason).not.toContain("stanza rewrote a.ts");
    expect(readFileSync(join(cwd, "a.ts"), "utf8")).toBe(readFileSync(fixture, "utf8"));
  },
);

function committed(files: Record<string, string>): string {
  const cwd = scratchGitRepository({ files });
  gitIn(cwd, "add", "-A");
  gitIn(cwd, "commit", "-qm", "agent");
  return cwd;
}

function stopOn(cwd: string, written: string[], args: string[] = []): CommandResult {
  const path = transcript(
    userTurn,
    ...toolCalls(...written.map((name): [string, string] => ["Edit", join(cwd, name)])),
  );

  return hookCommand(JSON.stringify({ cwd, hook_event_name: "Stop", transcript_path: path }), args);
}

test("Stop fixes and blocks on files the agent wrote and committed", () => {
  const cwd = committed({ ...wallAndBodies(), "human.ts": readFileSync(fixture, "utf8") });
  const result = stopOn(cwd, ["bodies.ts", "wall.ts"]);
  const reason = JSON.parse(output(result)).reason;

  expect(reason).toContain("\nwall.ts\n  line 2: split these 7 statements");
  expect(reason).not.toContain("human.ts");
  expect(readFileSync(join(cwd, "bodies.ts"))).toEqual(readFileSync(bodiesAfter));
  expect(readFileSync(join(cwd, "human.ts"))).toEqual(readFileSync(fixture));

  expect(result.exitCode).toBe(0);
  expect(new TextDecoder().decode(result.stderr)).toBe("");
});

test("Stop --hunks leaves a committed written file with no diff alone", () => {
  const cwd = committed(wallAndBodies());
  const result = stopOn(cwd, ["bodies.ts", "wall.ts"], ["--hunks"]);

  expectSilent(result);
  expect(readFileSync(join(cwd, "bodies.ts"))).toEqual(readFileSync(fixture));
});

test("Stop --hunks fixes only the changed lines of a committed written file", () => {
  const cwd = committed({ "a.ts": `${hunkFunction("f1")}\n\n${hunkFunction("f2")}\n` });
  writeFileSync(join(cwd, "a.ts"), `${hunkFunction("f1")}\n\n${hunkFunction("f2", 3)}\n`);
  expectSilent(stopOn(cwd, ["a.ts"], ["--hunks"]));

  const fixed = readFileSync(join(cwd, "a.ts"), "utf8");
  expect(fixed).toStartWith(`${hunkFunction("f1")}\n`);
  expect(fixed).toContain("  if (a) return 1;\n  return 3;");
});

test("Stop skips written files since deleted, ignored by git or opted out", () => {
  const before = readFileSync(fixture, "utf8");
  const cwd = committed({
    ".gitattributes": "opted.ts -stanza\n",
    ".gitignore": "ignored.ts\n",
    "gone.ts": before,
    "opted.ts": before,
    "kept.ts": before,
  });

  rmSync(join(cwd, "gone.ts"));
  writeFileSync(join(cwd, "ignored.ts"), before);
  const result = stopOn(cwd, ["gone.ts", "ignored.ts", "opted.ts", "kept.ts"]);

  expectSilent(result);
  expect(readFileSync(join(cwd, "kept.ts"))).toEqual(readFileSync(bodiesAfter));
  expect(readFileSync(join(cwd, "ignored.ts"), "utf8")).toBe(before);
  expect(readFileSync(join(cwd, "opted.ts"), "utf8")).toBe(before);
});

test("Stop checks staged and untracked written files in an unborn repository", () => {
  const before = readFileSync(fixture, "utf8");
  const cwd = scratchGitRepository({
    files: { "staged.ts": before, "loose.ts": before, "human.ts": before },
    staged: ["staged.ts", "human.ts"],
  });

  for (const args of [[], ["--hunks"]]) {
    for (const name of ["staged.ts", "loose.ts"]) writeFileSync(join(cwd, name), before);

    expectSilent(stopOn(cwd, ["staged.ts", "loose.ts"], args));
    expect(readFileSync(join(cwd, "staged.ts"))).toEqual(readFileSync(bodiesAfter));
    expect(readFileSync(join(cwd, "loose.ts"))).toEqual(readFileSync(bodiesAfter));
    expect(readFileSync(join(cwd, "human.ts"), "utf8")).toBe(before);
  }
});

test("Stop reads committed written names holding * or : literally", () => {
  const before = readFileSync(fixture, "utf8");
  const cwd = committed({ "a*.ts": before, "ab.ts": before, "b:c.ts": before, ":d.ts": before });
  expectSilent(stopOn(cwd, ["a*.ts", "b:c.ts", ":d.ts"]));

  for (const name of ["a*.ts", "b:c.ts", ":d.ts"])
    expect(readFileSync(join(cwd, name))).toEqual(readFileSync(bodiesAfter));

  expect(readFileSync(join(cwd, "ab.ts"), "utf8")).toBe(before);
});

test("Stop without a usable transcript still takes only changed files", () => {
  const before = readFileSync(fixture, "utf8");
  const cwd = committed({ "old.ts": before, "edited.ts": "const a = 1;\n" });
  writeFileSync(join(cwd, "edited.ts"), before);

  for (const transcript_path of [undefined, join(cwd, "missing.jsonl")]) {
    expectSilent(hookCommand(JSON.stringify({ cwd, hook_event_name: "Stop", transcript_path })));
    expect(readFileSync(join(cwd, "edited.ts"))).toEqual(readFileSync(bodiesAfter));
    expect(readFileSync(join(cwd, "old.ts"), "utf8")).toBe(before);
    writeFileSync(join(cwd, "edited.ts"), before);
  }
});
