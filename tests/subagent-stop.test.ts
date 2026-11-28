import { expect, test } from "bun:test";
import { chmodSync, mkdirSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { dirname, join } from "node:path";
import {
  codexFileChange,
  codexRollout,
  hook,
  subagentTranscript,
  toolCalls,
  transcript,
  writeTranscript,
} from "./hook-support.ts";
import { git, scratch, scratchGitRepository } from "./support.ts";

const wall = "f();\n".repeat(7);
const clean = "const sub = 1;\n";
const needsFix = "if (ready) {\n  act();\n}\n";
function session(sub = clean) {
  const cwd = scratchGitRepository({
    files: { "main.ts": wall, "sub.ts": sub, "other.ts": needsFix },
    staged: true,
  });

  const transcriptPath = transcript(
    ...toolCalls(["Write", join(cwd, "main.ts")], ["Write", join(cwd, "other.ts")]),
  );

  const agentTranscriptPath = subagentTranscript(
    transcriptPath,
    "agent",
    ...toolCalls(["Write", join(cwd, "sub.ts")]),
  );

  return { cwd, transcriptPath, agentTranscriptPath };
}

function contents(fixture: ReturnType<typeof session>): Buffer[] {
  return ["main.ts", "sub.ts", "other.ts"].map((name) => readFileSync(join(fixture.cwd, name)));
}

function hookOn(fixture: ReturnType<typeof session>, fields: Record<string, unknown> = {}) {
  return hook({
    cwd: fixture.cwd,
    hook_event_name: "SubagentStop",
    transcript_path: fixture.transcriptPath,
    agent_transcript_path: fixture.agentTranscriptPath,
    ...fields,
  });
}

test("SubagentStop ignores the main session's wall when its own file is clean", () => {
  const fixture = session();
  const before = contents(fixture);
  const result = hookOn(fixture);

  expect(result).toEqual({ code: 0, stdout: "", stderr: "" });
  expect(contents(fixture)).toEqual(before);
});

test("SubagentStop with no writes exits silently", () => {
  const fixture = session();
  writeTranscript(
    fixture.agentTranscriptPath,
    ...toolCalls(["Read", join(fixture.cwd, "main.ts")]),
  );

  const before = contents(fixture);
  const result = hookOn(fixture);

  expect(result).toEqual({ code: 0, stdout: "", stderr: "" });
  expect(contents(fixture)).toEqual(before);
});

test("SubagentStop names only its own wall while Stop names both session walls", () => {
  const fixture = session(wall);
  const subagent = hookOn(fixture);
  const main = hookOn(fixture, { hook_event_name: "Stop" });

  expect(subagent.code).toBe(0);
  expect(subagent.stderr).toBe("");
  expect(JSON.parse(subagent.stdout).decision).toBe("block");
  expect(JSON.parse(subagent.stdout).reason).toContain("\nsub.ts\n");
  expect(JSON.parse(subagent.stdout).reason).not.toContain("main.ts");

  expect(main.code).toBe(0);
  expect(main.stderr).toBe("");
  expect(JSON.parse(main.stdout).decision).toBe("block");
  expect(JSON.parse(main.stdout).reason).toContain("\nmain.ts\n");
  expect(JSON.parse(main.stdout).reason).toContain("\nsub.ts\n");
});

test.each(["absent", "missing", "empty", "unrecognized"])(
  "SubagentStop handles %s agent transcripts without repository fallback",
  (kind) => {
    const fixture = session(needsFix);
    const before = contents(fixture);

    let agentTranscriptPath: string | undefined = fixture.agentTranscriptPath;
    if (kind === "absent") agentTranscriptPath = undefined;
    if (kind === "missing")
      agentTranscriptPath = join(dirname(fixture.transcriptPath), "missing.jsonl");

    if (kind === "empty") writeFileSync(fixture.agentTranscriptPath, "");
    if (kind === "unrecognized") writeFileSync(fixture.agentTranscriptPath, "not json\n");

    const result = hookOn(fixture, { agent_transcript_path: agentTranscriptPath });

    expect(result).toEqual({ code: 0, stdout: "", stderr: "" });
    expect(contents(fixture)).toEqual(before);
  },
);

test.skipIf(process.getuid?.() === 0)(
  "SubagentStop handles unreadable agent transcripts without repository fallback",
  () => {
    const fixture = session(needsFix);
    chmodSync(fixture.agentTranscriptPath, 0o000);
    const before = contents(fixture);
    const result = hookOn(fixture);

    expect(result).toEqual({ code: 0, stdout: "", stderr: "" });
    expect(contents(fixture)).toEqual(before);
  },
);

test("SubagentStop with only failed edits ignores those files", () => {
  const fixture = session(wall);
  writeTranscript(
    fixture.agentTranscriptPath,
    ...toolCalls(["Edit", join(fixture.cwd, "sub.ts"), true]),
  );

  const before = contents(fixture);
  const result = hookOn(fixture);

  expect(result).toEqual({ code: 0, stdout: "", stderr: "" });
  expect(contents(fixture)).toEqual(before);
});

test.each(["Edit", "MultiEdit"])("SubagentStop scopes successful %s calls", (name) => {
  const fixture = session(wall);
  writeTranscript(fixture.agentTranscriptPath, ...toolCalls([name, join(fixture.cwd, "sub.ts")]));
  const result = hookOn(fixture);

  expect(result.code).toBe(0);
  expect(result.stderr).toBe("");
  expect(JSON.parse(result.stdout).reason).toContain("\nsub.ts\n");
  expect(JSON.parse(result.stdout).reason).not.toContain("main.ts");
  expect(readFileSync(join(fixture.cwd, "other.ts"), "utf8")).toBe(needsFix);
});

test("SubagentStop fixes only its own file without rewriting the transcript", () => {
  const fixture = session(needsFix);
  const mainBefore = readFileSync(join(fixture.cwd, "main.ts"));
  const transcriptBefore = readFileSync(fixture.agentTranscriptPath);
  const result = hookOn(fixture);

  expect(result).toEqual({ code: 0, stdout: "", stderr: "" });

  expect(readFileSync(join(fixture.cwd, "sub.ts"), "utf8")).toBe("if (ready) act();\n");
  expect(readFileSync(join(fixture.cwd, "main.ts"))).toEqual(mainBefore);
  expect(readFileSync(join(fixture.cwd, "other.ts"), "utf8")).toBe(needsFix);
  expect(readFileSync(fixture.agentTranscriptPath)).toEqual(transcriptBefore);
});

test("SubagentStop under a removed cwd fixes its own file", () => {
  const fixture = session(needsFix);
  const cwd = join(scratch("subagent-stop"), "gone");
  mkdirSync(cwd);
  rmSync(cwd, { recursive: true });

  expect(hookOn(fixture, { cwd })).toEqual({ code: 0, stdout: "", stderr: "" });
  expect(readFileSync(join(fixture.cwd, "sub.ts"), "utf8")).toBe("if (ready) act();\n");
  expect(readFileSync(join(fixture.cwd, "other.ts"), "utf8")).toBe(needsFix);
});

test("SubagentStop does not discover sibling or descendant transcripts", () => {
  const fixture = session();
  writeTranscript(
    join(dirname(fixture.agentTranscriptPath), "sibling.jsonl"),
    ...toolCalls(["Write", join(fixture.cwd, "main.ts")]),
  );

  writeTranscript(
    join(dirname(fixture.agentTranscriptPath), "agent", "subagents", "nested.jsonl"),
    ...toolCalls(["Write", join(fixture.cwd, "main.ts")]),
  );

  const before = contents(fixture);
  const result = hookOn(fixture);

  expect(result).toEqual({ code: 0, stdout: "", stderr: "" });
  expect(contents(fixture)).toEqual(before);
});

test("SubagentStop skips its pass when stop_hook_active is true", () => {
  const fixture = session(wall);
  const before = contents(fixture);
  const result = hookOn(fixture, { stop_hook_active: true });

  expect(result).toEqual({ code: 0, stdout: "", stderr: "" });
  expect(contents(fixture)).toEqual(before);
});

const shortBody = "export function run() {\n  const value = load();\n\n  save(value);\n}\n";

const subagentMeta = {
  thread_source: "subagent",
  source: {
    subagent: {
      thread_spawn: { parent_thread_id: "thread-0", depth: 1, agent_path: null, agent_role: null },
    },
  },
};

const agentMessage = {
  type: "event_msg",
  payload: { type: "item_completed", item: { type: "AgentMessage", id: "msg-1", content: [] } },
};

function codexSession(mainWrites: string[], agentWrites?: string[]) {
  const cwd = scratchGitRepository({
    files: { "a.ts": shortBody, "b.ts": shortBody, "notes.md": "notes\n" },
    staged: true,
  });

  const rollout = (meta: Record<string, unknown>, names: string[]) =>
    codexRollout(
      meta,
      agentMessage,
      ...names.map((name) => codexFileChange({ [join(cwd, name)]: { type: "add", content: "" } })),
    );

  const transcriptPath = rollout({}, mainWrites);
  const agentTranscriptPath = agentWrites
    ? rollout(subagentMeta, agentWrites)
    : join(dirname(transcriptPath), "rollout-subagent.jsonl");

  return { cwd, transcriptPath, agentTranscriptPath };
}

function codexContents(fixture: ReturnType<typeof codexSession>): Buffer[] {
  return ["a.ts", "b.ts", "notes.md"].map((name) => readFileSync(join(fixture.cwd, name)));
}

test("Codex SubagentStop fixes its rollout's file exactly as Codex Stop does", () => {
  const subagent = codexSession([], ["a.ts"]);
  const stop = codexSession(["a.ts"]);
  const subagentResult = hookOn(subagent);
  const stopResult = hookOn(stop, { hook_event_name: "Stop" });

  expect(subagentResult).toEqual(stopResult);
  expect(subagentResult).toEqual({ code: 0, stdout: "", stderr: "" });

  expect(codexContents(subagent)).toEqual(codexContents(stop));
  expect(readFileSync(join(subagent.cwd, "a.ts"), "utf8")).not.toBe(shortBody);
  expect(readFileSync(join(subagent.cwd, "b.ts"), "utf8")).toBe(shortBody);
  expect(readFileSync(join(stop.cwd, "b.ts"), "utf8")).toBe(shortBody);
});

test("Codex SubagentStop whose rollout wrote no candidate file changes nothing", () => {
  const fixture = codexSession(["b.ts"], ["notes.md"]);
  const before = codexContents(fixture);
  const result = hookOn(fixture);

  expect(result).toEqual({ code: 0, stdout: "", stderr: "" });
  expect(codexContents(fixture)).toEqual(before);
});

test("SubagentStop fixes and blocks on files the subagent wrote and committed", () => {
  const fixture = session(needsFix);
  writeFileSync(join(fixture.cwd, "wall.ts"), wall);
  git(fixture.cwd, "add", "wall.ts");
  git(fixture.cwd, "commit", "-qm", "agent");
  writeTranscript(
    fixture.agentTranscriptPath,
    ...toolCalls(["Write", join(fixture.cwd, "sub.ts")], ["Edit", join(fixture.cwd, "wall.ts")]),
  );

  const result = hookOn(fixture);
  const reason = JSON.parse(result.stdout).reason;

  expect(reason).toContain("\nwall.ts\n  line 1: split these 7 statements");
  expect(reason).not.toContain("main.ts");
  expect(readFileSync(join(fixture.cwd, "sub.ts"), "utf8")).toBe("if (ready) act();\n");
  expect(readFileSync(join(fixture.cwd, "other.ts"), "utf8")).toBe(needsFix);

  expect(result.code).toBe(0);
  expect(result.stderr).toBe("");
});
