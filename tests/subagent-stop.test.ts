import { expect, test } from "bun:test";
import { mkdirSync, readFileSync, writeFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { runMain, scratch, scratchGitRepository } from "./support.ts";

const wall = "f();\n".repeat(7);
const clean = "const sub = 1;\n";
const needsFix = "if (ready) {\n  act();\n}\n";

function toolCalls(...calls: [name: string, path: string, failed?: boolean][]): unknown[] {
  const uses = calls.map(([name, file_path], index) => ({
    type: "tool_use",
    id: `tool${index}`,
    name,
    input: { file_path },
  }));

  const results = calls.map(([, , failed], index) => ({
    type: "tool_result",
    tool_use_id: `tool${index}`,
    content: failed ? "failed" : "ok",
    is_error: failed ?? false,
  }));

  return [
    { type: "assistant", message: { role: "assistant", content: uses } },
    { type: "user", message: { role: "user", content: results } },
  ];
}

function writeTranscript(path: string, records: unknown[]): void {
  mkdirSync(dirname(path), { recursive: true });
  writeFileSync(path, records.map((record) => JSON.stringify(record)).join("\n"));
}

function session(sub = clean) {
  const cwd = scratchGitRepository({
    files: { "main.ts": wall, "sub.ts": sub, "other.ts": needsFix },
    staged: true,
  });

  const transcriptPath = join(scratch("subagent-stop"), "session.jsonl");
  const agentTranscriptPath = join(dirname(transcriptPath), "session", "subagents", "agent.jsonl");
  writeTranscript(
    transcriptPath,
    toolCalls(["Write", join(cwd, "main.ts")], ["Write", join(cwd, "other.ts")]),
  );

  writeTranscript(agentTranscriptPath, toolCalls(["Write", join(cwd, "sub.ts")]));

  return { cwd, transcriptPath, agentTranscriptPath };
}

function contents(fixture: ReturnType<typeof session>): Buffer[] {
  return ["main.ts", "sub.ts", "other.ts"].map((name) => readFileSync(join(fixture.cwd, name)));
}

function hook(fixture: ReturnType<typeof session>, fields: Record<string, unknown> = {}) {
  return runMain(["hook"], {
    env: { ...process.env, AGENT_HOOKS: "1" },
    stdin: new TextEncoder().encode(
      JSON.stringify({
        cwd: fixture.cwd,
        hook_event_name: "SubagentStop",
        transcript_path: fixture.transcriptPath,
        agent_transcript_path: fixture.agentTranscriptPath,
        ...fields,
      }),
    ),
  });
}

test("SubagentStop ignores the main session's wall when its own file is clean", () => {
  const fixture = session();
  const before = contents(fixture);
  const result = hook(fixture);

  expect(result).toEqual({ code: 0, stdout: "", stderr: "" });
  expect(contents(fixture)).toEqual(before);
});

test("SubagentStop with no writes exits silently", () => {
  const fixture = session();
  writeTranscript(fixture.agentTranscriptPath, toolCalls(["Read", join(fixture.cwd, "main.ts")]));
  const before = contents(fixture);
  const result = hook(fixture);

  expect(result).toEqual({ code: 0, stdout: "", stderr: "" });
  expect(contents(fixture)).toEqual(before);
});

test("SubagentStop names only its own wall while Stop names both session walls", () => {
  const fixture = session(wall);
  const subagent = hook(fixture);
  const main = hook(fixture, { hook_event_name: "Stop" });

  expect(subagent.code).toBe(0);
  expect(subagent.stderr).toBe("");
  expect(JSON.parse(subagent.stdout).decision).toBe("block");
  expect(JSON.parse(subagent.stdout).reason).toContain("sub.ts:");
  expect(JSON.parse(subagent.stdout).reason).not.toContain("main.ts");

  expect(main.code).toBe(0);
  expect(main.stderr).toBe("");
  expect(JSON.parse(main.stdout).decision).toBe("block");
  expect(JSON.parse(main.stdout).reason).toContain("main.ts:");
  expect(JSON.parse(main.stdout).reason).toContain("sub.ts:");
});

test.each(["absent", "missing", "unreadable", "empty", "unrecognized"])(
  "SubagentStop handles %s agent transcripts without repository fallback",
  (kind) => {
    const fixture = session(needsFix);
    const before = contents(fixture);

    let agentTranscriptPath: string | undefined = fixture.agentTranscriptPath;
    if (kind === "absent") agentTranscriptPath = undefined;
    if (kind === "missing")
      agentTranscriptPath = join(dirname(fixture.transcriptPath), "missing.jsonl");

    if (kind === "unreadable") agentTranscriptPath = dirname(fixture.agentTranscriptPath);
    if (kind === "empty") writeFileSync(fixture.agentTranscriptPath, "");
    if (kind === "unrecognized") writeFileSync(fixture.agentTranscriptPath, "not json\n");

    const result = hook(fixture, { agent_transcript_path: agentTranscriptPath });

    expect(result).toEqual({ code: 0, stdout: "", stderr: "" });
    expect(contents(fixture)).toEqual(before);
  },
);

test("SubagentStop with only failed edits ignores those files", () => {
  const fixture = session(wall);
  writeTranscript(
    fixture.agentTranscriptPath,
    toolCalls(["Edit", join(fixture.cwd, "sub.ts"), true]),
  );

  const before = contents(fixture);
  const result = hook(fixture);

  expect(result).toEqual({ code: 0, stdout: "", stderr: "" });
  expect(contents(fixture)).toEqual(before);
});

test.each(["Edit", "MultiEdit"])("SubagentStop scopes successful %s calls", (name) => {
  const fixture = session(wall);
  writeTranscript(fixture.agentTranscriptPath, toolCalls([name, join(fixture.cwd, "sub.ts")]));
  const result = hook(fixture);

  expect(result.code).toBe(0);
  expect(result.stderr).toBe("");
  expect(JSON.parse(result.stdout).reason).toContain("sub.ts:");
  expect(JSON.parse(result.stdout).reason).not.toContain("main.ts");
  expect(readFileSync(join(fixture.cwd, "other.ts"), "utf8")).toBe(needsFix);
});

test("SubagentStop fixes only its own file without rewriting the transcript", () => {
  const fixture = session(needsFix);
  const mainBefore = readFileSync(join(fixture.cwd, "main.ts"));
  const transcriptBefore = readFileSync(fixture.agentTranscriptPath);
  const result = hook(fixture);

  expect(result).toEqual({ code: 0, stdout: "", stderr: "" });

  expect(readFileSync(join(fixture.cwd, "sub.ts"), "utf8")).toBe("if (ready)\n  act();\n");
  expect(readFileSync(join(fixture.cwd, "main.ts"))).toEqual(mainBefore);
  expect(readFileSync(join(fixture.cwd, "other.ts"), "utf8")).toBe(needsFix);
  expect(readFileSync(fixture.agentTranscriptPath)).toEqual(transcriptBefore);
});

test("SubagentStop does not discover sibling or descendant transcripts", () => {
  const fixture = session();
  writeTranscript(
    join(dirname(fixture.agentTranscriptPath), "sibling.jsonl"),
    toolCalls(["Write", join(fixture.cwd, "main.ts")]),
  );

  writeTranscript(
    join(dirname(fixture.agentTranscriptPath), "agent", "subagents", "nested.jsonl"),
    toolCalls(["Write", join(fixture.cwd, "main.ts")]),
  );

  const before = contents(fixture);
  const result = hook(fixture);

  expect(result).toEqual({ code: 0, stdout: "", stderr: "" });
  expect(contents(fixture)).toEqual(before);
});

test("SubagentStop skips its pass when stop_hook_active is true", () => {
  const fixture = session(wall);
  const before = contents(fixture);
  const result = hook(fixture, { stop_hook_active: true });

  expect(result).toEqual({ code: 0, stdout: "", stderr: "" });
  expect(contents(fixture)).toEqual(before);
});
