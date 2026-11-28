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
import { dirname, join, relative } from "node:path";
import {
  codexExec,
  codexFileChange,
  codexRollout,
  hook,
  subagentTranscript,
  toolCalls,
  transcript,
} from "./hook-support.ts";
import {
  committedSource,
  git,
  gitRefusesOwnership,
  hunkFunction,
  type RunResult,
  scratch,
  scratchGitRepository,
  superproject,
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

function wallAndBodies(): Record<string, string> {
  return {
    "wall.ts": readFileSync(join(root, "tests", "fixtures", "wall", "wall.before.ts"), "utf8"),
    "bodies.ts": readFileSync(fixture, "utf8"),
  };
}

test("stanza hook fixes the input repository and blocks only on remaining findings", () => {
  const cwd = repository(wallAndBodies());
  const result = hook({ cwd });
  const lines = result.stdout.split("\n");
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

  expect(result.code).toBe(0);
  expect(result.stderr).toBe("");
});

const bodiesAfter = join(root, "tests", "fixtures", "braces", "bodies.after.ts");

const userTurn = { type: "user", message: { role: "user", content: "hi" } };

test("Stop with only a package.json Write exits without spawning git", () => {
  const cwd = scratchGitRepository({ files: { "package.json": "{}\n" } });
  const transcriptPath = transcript(...toolCalls(["Write", join(cwd, "package.json")]));
  const { result, calls } = tracedGit((env) =>
    hook({ cwd, hook_event_name: "Stop", transcript_path: transcriptPath }, { env }),
  );

  expect(result.code).toBe(0);
  expect(result.stdout).toBe("");
  expect(result.stderr).toBe("");
  expect(calls).toEqual([]);
});

function agentAndHuman(): string {
  const before = readFileSync(fixture, "utf8");
  return repository({ "human.ts": before, "agent.ts": before });
}

function expectSilent(result: RunResult): void {
  expect(result.stdout).toBe("");
  expect(result.stderr).toBe("");
  expect(result.code).toBe(0);
}

test("stanza hook reads a rollout captured from Codex 0.159.0", () => {
  const before = readFileSync(fixture, "utf8");
  const cwd = repository({ "src/agent.ts": before, "human.ts": before });
  const captured = readFileSync(join(root, "tests", "fixtures", "codex", "rollout-0.159.0.jsonl"));
  const path = transcript(...captured.toString().trimEnd().replaceAll("{{cwd}}", cwd).split("\n"));

  const result = hook({ cwd, transcript_path: path });

  expect(readFileSync(join(cwd, "src", "agent.ts"))).toEqual(readFileSync(bodiesAfter));
  expect(readFileSync(join(cwd, "human.ts"))).toEqual(readFileSync(fixture));
  expectSilent(result);
});

test("stanza hook scopes Codex 0.159.0 Stop to completed file changes", () => {
  const cwd = agentAndHuman();
  const agent = join(cwd, "agent.ts");
  const path = codexRollout(
    { cwd },
    ...codexExec(agent),
    codexFileChange({ [agent]: { type: "update", unified_diff: "@@", move_path: null } }),
  );

  const result = hook({ cwd, transcript_path: path });

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
  const path = codexRollout(
    { cwd },
    ...codexExec(agent),
    codexFileChange({ [agent]: { type: "update", unified_diff: "@@", move_path: null } }),
    ...codexExec(human, false),
    codexFileChange(
      { [pending]: { type: "update", unified_diff: "@@", move_path: null } },
      "in_progress",
    ),
  );

  const result = hook({ cwd, transcript_path: path });

  expect(readFileSync(agent)).toEqual(readFileSync(bodiesAfter));
  expect(readFileSync(human)).toEqual(readFileSync(fixture));
  expect(readFileSync(pending)).toEqual(readFileSync(fixture));
  expectSilent(result);
});

test("stanza hook fixes the destination of a Codex move", () => {
  const cwd = agentAndHuman();
  const agent = join(cwd, "agent.ts");
  const old = join(cwd, "old.ts");
  const path = codexRollout(
    { cwd },
    ...codexExec(agent),
    codexFileChange({ [old]: { type: "update", unified_diff: "@@", move_path: agent } }),
  );

  const result = hook({ cwd, transcript_path: path });

  expect(readFileSync(agent)).toEqual(readFileSync(bodiesAfter));
  expect(readFileSync(join(cwd, "human.ts"))).toEqual(readFileSync(fixture));
  expectSilent(result);
});

test("stanza hook touches nothing when a Codex rollout holds only a Reasoning completion", () => {
  const cwd = agentAndHuman();
  const path = codexRollout(
    { cwd },
    {
      type: "event_msg",
      payload: { type: "item_completed", item: { type: "Reasoning" } },
    },
  );

  const result = hook({ cwd, transcript_path: path });

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
  const path = codexRollout(
    { cwd },
    ...codexExec(agent),
    codexFileChange({ [agent]: { type: "update", unified_diff: "@@", move_path: null } }),
  );

  const result = hook(capturedStop(cwd, path), { args: ["--hunks"] });

  expect(readFileSync(agent)).toEqual(readFileSync(bodiesAfter));
  expect(readFileSync(join(cwd, "human.ts"))).toEqual(readFileSync(fixture));
  expectSilent(result);
});

test("stanza hook matches a rollout path through a symlink to a real cwd", () => {
  const cwd = agentAndHuman();
  const linkedCwd = join(scratch("hook-codex-tmp"), "repo");
  symlinkSync(cwd, linkedCwd, "dir");

  const agent = join(linkedCwd, "agent.ts");
  const path = codexRollout(
    { cwd: linkedCwd },
    ...codexExec(agent),
    codexFileChange({ [agent]: { type: "add", content: "" } }),
  );

  const result = hook(capturedStop(realpathSync(cwd), path));

  expect(readFileSync(join(cwd, "agent.ts"))).toEqual(readFileSync(bodiesAfter));
  expect(readFileSync(join(cwd, "human.ts"))).toEqual(readFileSync(fixture));
  expectSilent(result);
});

test("stanza hook fixes a file the agent wrote in a repository other than cwd", () => {
  const cwd = agentAndHuman();
  const other = agentAndHuman();
  const path = transcript(userTurn, ...toolCalls(["Write", join(other, "agent.ts")]));

  const result = hook({ cwd, transcript_path: path });

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
  const path = codexRollout(
    { cwd },
    ...codexExec(agent),
    codexFileChange({
      [gone]: { type: "add", content: "" },
      [agent]: { type: "update", unified_diff: "@@", move_path: null },
    }),
  );

  const result = hook({ cwd, transcript_path: path });

  expect(readFileSync(agent)).toEqual(readFileSync(bodiesAfter));
  expect(readFileSync(join(cwd, "human.ts"))).toEqual(readFileSync(fixture));
  expectSilent(result);
});

test("stanza hook does not follow a written symlink into another repository", () => {
  const cwd = agentAndHuman();
  const other = agentAndHuman();
  const link = join(cwd, "link.ts");
  symlinkSync(join(other, "agent.ts"), link);

  const rollout = codexRollout(
    { cwd },
    ...codexExec(link),
    codexFileChange({ [link]: { type: "update", unified_diff: "@@", move_path: null } }),
  );

  const claude = transcript(userTurn, ...toolCalls(["Edit", link]));
  for (const path of [rollout, claude]) expectSilent(hook({ cwd, transcript_path: path }));

  for (const repository of [cwd, other])
    for (const name of ["agent.ts", "human.ts"])
      expect(readFileSync(join(repository, name))).toEqual(readFileSync(fixture));
});

test("stanza hook touches nothing when a Codex rollout has no completed item event", () => {
  const cwd = agentAndHuman();
  const path = codexRollout({ cwd });
  const result = hook({ cwd, transcript_path: path });

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

  const result = hook({ cwd, transcript_path: path });

  expect(readFileSync(join(cwd, "agent.ts"))).toEqual(readFileSync(bodiesAfter));
  expect(readFileSync(join(cwd, "human.ts"))).toEqual(readFileSync(fixture));
  expectSilent(result);
});

test("stanza hook maps Codex paths through a symlinked cwd", () => {
  const cwd = agentAndHuman();
  const linkedCwd = join(scratch("hook-codex-link"), "repo");
  symlinkSync(cwd, linkedCwd, "dir");

  const agent = join(linkedCwd, "agent.ts");
  const path = codexRollout(
    { cwd: linkedCwd },
    ...codexExec(agent),
    codexFileChange({ [agent]: { type: "update", unified_diff: "@@", move_path: null } }),
  );

  const result = hook({ cwd: linkedCwd, transcript_path: path });

  expect(readFileSync(join(cwd, "agent.ts"))).toEqual(readFileSync(bodiesAfter));
  expect(readFileSync(join(cwd, "human.ts"))).toEqual(readFileSync(fixture));
  expectSilent(result);
});

test("stanza hook fixes a file Codex patched through a symlinked directory", () => {
  const before = readFileSync(fixture, "utf8");
  const cwd = repository({ "packages/x/agent.ts": before, "human.ts": before });
  symlinkSync(join(cwd, "packages", "x"), join(cwd, "lib"), "dir");

  const agent = join(cwd, "lib", "agent.ts");
  const path = codexRollout(
    { cwd },
    ...codexExec(agent),
    codexFileChange({ [agent]: { type: "update", unified_diff: "@@", move_path: null } }),
  );

  const result = hook({ cwd, transcript_path: path });

  expect(readFileSync(join(cwd, "packages", "x", "agent.ts"))).toEqual(readFileSync(bodiesAfter));
  expect(readFileSync(join(cwd, "human.ts"))).toEqual(readFileSync(fixture));
  expectSilent(result);
});

test("stanza hook fixes only the files the transcript says the agent wrote", () => {
  const cwd = agentAndHuman();
  const path = transcript(userTurn, ...toolCalls(["Edit", join(cwd, "agent.ts")]));
  const result = hook({ cwd, transcript_path: path });

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

  const result = hook({ cwd, transcript_path: path });

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

  const result = hook({ cwd, transcript_path: path });

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

  const result = hook({ cwd, transcript_path: path });

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

    const result = hook({ cwd, transcript_path: path });

    expect(readFileSync(join(cwd, "main.ts"))).toEqual(readFileSync(bodiesAfter));
    expect(readFileSync(join(cwd, "human.ts"))).toEqual(readFileSync(fixture));
    expectSilent(result);
  }
});

test("stanza hook fixes a file whose edit result the transcript does not hold yet", () => {
  const cwd = agentAndHuman();
  const [edit] = toolCalls(["Edit", join(cwd, "agent.ts")]);
  const result = hook({ cwd, transcript_path: transcript(userTurn, edit) });

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
  const result = hook({ cwd, transcript_path: transcript(...spaced) });

  expect(readFileSync(join(cwd, "human.ts"))).toEqual(readFileSync(fixture));
  expect(readFileSync(join(cwd, "agent.ts"))).toEqual(readFileSync(bodiesAfter));
  expectSilent(result);
});

test("stanza hook falls back to changed files without transcript_path", () => {
  const cwd = agentAndHuman();
  const result = hook({ cwd });

  expect(readFileSync(join(cwd, "human.ts"))).toEqual(readFileSync(bodiesAfter));
  expect(readFileSync(join(cwd, "agent.ts"))).toEqual(readFileSync(bodiesAfter));
  expectSilent(result);
});

const braced = "if (ready) {\n  act();\n}\n";
const unbraced = "if (ready) act();\n";

test("Stop below the root fixes changed files across the repository", () => {
  const repo = scratchGitRepository({ files: { "kept.ts": braced }, staged: true });
  git(repo, "commit", "-qm", "init");

  mkdirSync(join(repo, "pkg"));
  writeFileSync(join(repo, "a.ts"), braced);
  writeFileSync(join(repo, "pkg", "b.ts"), braced);
  const result = hook({ hook_event_name: "Stop", cwd: join(repo, "pkg") });

  expect(readFileSync(join(repo, "a.ts"), "utf8")).toBe(unbraced);
  expect(readFileSync(join(repo, "pkg", "b.ts"), "utf8")).toBe(unbraced);
  expect(readFileSync(join(repo, "kept.ts"), "utf8")).toBe(braced);
  expectSilent(result);
});

test("Stop leaves a submodule's changes alone", () => {
  const cwd = superproject({});
  const sub = join(cwd, "vendor", "sub", "s.ts");
  writeFileSync(sub, braced);

  expectSilent(hook({ hook_event_name: "Stop", cwd }));
  expect(readFileSync(sub, "utf8")).toBe(braced);

  writeFileSync(join(cwd, "a.ts"), braced);
  const result = hook({ hook_event_name: "Stop", cwd });

  expect(readFileSync(join(cwd, "a.ts"), "utf8")).toBe(unbraced);
  expect(readFileSync(sub, "utf8")).toBe(braced);
  expectSilent(result);
});

test("Stop on workers fixes a hard-linked file and an ordinary one", () => {
  const cwd = repository({ "a.ts": readFileSync(fixture, "utf8") });
  linkSync(join(cwd, "a.ts"), join(cwd, "twin.ts"));
  copyFileSync(fixture, join(cwd, "b.ts"));
  const result = hook({ cwd }, { env: { STANZA_WORKERS: "2" } });

  for (const name of ["a.ts", "twin.ts", "b.ts"])
    expect(readFileSync(join(cwd, name))).toEqual(readFileSync(bodiesAfter));

  expect(statSync(join(cwd, "a.ts")).ino).toBe(statSync(join(cwd, "twin.ts")).ino);
  expectSilent(result);
});

test("stanza hook falls back to changed files when it cannot read the transcript", () => {
  for (const path of [transcript('{"type":"response_item"}'), "/nonexistent/stop.jsonl"]) {
    const cwd = agentAndHuman();
    const result = hook({ cwd, transcript_path: path });

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
      const result = hook({ cwd });
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
  const result = hook({ cwd, transcript_path: path });

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

  const result = hook({ cwd, transcript_path: path });

  expect(readFileSync(join(cwd, "agent.ts"))).toEqual(readFileSync(bodiesAfter));
  expect(readFileSync(join(cwd, "human.ts"))).toEqual(readFileSync(fixture));
  expect(readFileSync(outside)).toEqual(readFileSync(fixture));
  expectSilent(result);
});

test("stanza hook leaves files untouched when stop_hook_active is true", () => {
  const files = wallAndBodies();
  const cwd = repository(files);
  const result = hook({ cwd, stop_hook_active: true });

  expect(result.stdout).toBe("");
  expect(result.stderr).toBe("");
  expect(result.code).toBe(0);

  for (const [path, text] of Object.entries(files))
    expect(readFileSync(join(cwd, path))).toEqual(Buffer.from(text));
});

test("stanza hook reports parse failures together with wall findings", () => {
  const cwd = repository({ ...wallAndBodies(), "broken.ts": "function (\n" });
  const result = hook({ cwd });
  const decision = JSON.parse(result.stdout);

  expect(decision.decision).toBe("block");
  expect(decision.reason).toStartWith("These need a hand fix in the files you changed:\n\n");
  expect(decision.reason).toContain("\nbroken.ts\n  line 1: Expected function name (parse)\n");
  expect(decision.reason).toContain("\nwall.ts\n  line 2: ");
  expect(readFileSync(join(cwd, "broken.ts"), "utf8")).toBe("function (\n");

  expect(result.code).toBe(0);
  expect(result.stderr).toBe("");
});

test("stanza hook gives a parse failure its own line in the reason", () => {
  const cwd = repository({ "broken.ts": "let x = ;\n" });
  const decision = JSON.parse(hook({ cwd }).stdout);

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

test("stanza hook silently skips outside and missing directories", () => {
  const outside = scratch("hook-outside");
  for (const cwd of [outside, join(outside, "missing")]) {
    const result = hook({ cwd });
    expect(result.stdout).toBe("");
    expect(result.stderr).toBe("");
    expect(result.code).toBe(0);
  }
});

test("Stop under a removed cwd fixes the files its transcript names and skips the fallback", () => {
  const repo = agentAndHuman();
  const cwd = join(repo, "gone");
  mkdirSync(cwd);
  rmSync(cwd, { recursive: true });

  expectSilent(hook({ cwd }, { cwd: repo }));

  for (const name of ["agent.ts", "human.ts"])
    expect(readFileSync(join(repo, name))).toEqual(readFileSync(fixture));

  const path = transcript(userTurn, ...toolCalls(["Edit", join(repo, "agent.ts")]));
  expectSilent(hook({ cwd, transcript_path: path }));
  expect(readFileSync(join(repo, "agent.ts"))).toEqual(readFileSync(bodiesAfter));
  expect(readFileSync(join(repo, "human.ts"))).toEqual(readFileSync(fixture));
});

test("stanza hook rejects malformed stdin without a block decision", () => {
  const result = hook("not json");
  expect(result.stdout).toBe("");
  expect(result.stderr).toContain("stanza hook: input must be valid JSON");

  expect(result.code).toBe(1);
});

test("stanza hook names the closest of its own flags for a mistyped one", () => {
  const result = hook("not json", { args: ["--hunk"] });
  expect(result.stdout).toBe("");
  expect(result.stderr).toBe(
    "stanza hook: unknown flag --hunk, did you mean --hunks?\nrun `stanza --help` for usage\n",
  );

  expect(result.code).toBe(1);
});

test("stanza hook stays silent after fixing every finding", () => {
  const cwd = repository();
  const result = hook({ cwd });
  expect(result.stdout).toBe("");
  expect(result.stderr).toBe("");
  expect(result.code).toBe(0);

  expect(readFileSync(join(cwd, "a.ts"))).toEqual(
    readFileSync(join(root, "tests", "fixtures", "braces", "bodies.after.ts")),
  );
});

test("stanza hook reports a git selection failure as a non-blocking error", () => {
  const files = wallAndBodies();
  const cwd = repository(files);
  writeFileSync(join(cwd, ".git", "index"), "junkjunkjunkjunkjunk");

  const result = hook({ cwd });
  expect(result.stdout).toBe("");
  expect(result.stderr).toBe(
    "stanza hook: git ls-files failed: fatal: .git/index: index file smaller than expected\n",
  );

  expect(result.code).toBe(1);

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
    return hook({ cwd: repo, transcript_path: path });
  };

  const expected = stop(control);
  expect(expected.code).toBe(0);
  expect(JSON.parse(expected.stdout).decision).toBe("block");

  const result = stop(cwd, join(broken, "a.ts"));

  expect(result.stdout).toBe(expected.stdout);
  expect(result.stderr).toBe(
    `stanza hook: git ls-files failed in ${relative(realpathSync(cwd), realpathSync(broken))}: fatal: .git/index: index file smaller than expected\n`,
  );

  expect(result.code).toBe(1);
  expect(readFileSync(join(cwd, "bodies.ts"))).toEqual(readFileSync(bodiesAfter));
  expect(readFileSync(join(broken, "a.ts"))).toEqual(readFileSync(fixture));
});

test("stanza hook reports a repository git refuses as a non-blocking error", () => {
  const cwd = repository();
  const result = hook({ cwd }, { env: gitRefusesOwnership() });

  expect(result.stdout).toBe("");
  expect(result.stderr).toContain("dubious ownership");
  expect(result.code).toBe(1);
});

test("stanza hook --no-braces keeps braces and still applies blank line fixes", () => {
  const original = readFileSync(fixture, "utf8");
  const cwd = repository();
  const result = hook({ cwd }, { args: ["--no-braces"] });
  const fixed = readFileSync(join(cwd, "a.ts"), "utf8");

  expect(result.code).toBe(0);
  expect(braces(fixed)).toBe(braces(original));
  expect(fixed).not.toBe(original);
});

test("stanza hook --braces overrides a braces enforcing config", () => {
  const source = readFileSync(fixture, "utf8");
  const cwd = repository({ ".oxlintrc.json": '{ "rules": { "curly": "error" } }', "a.ts": source });
  const result = hook({ cwd }, { args: ["--braces"] });

  expect(result.code).toBe(0);
  expect(braces(readFileSync(join(cwd, "a.ts"), "utf8"))).toBeLessThan(braces(source));
  expect(result.stderr).toBe("");
});

test("stanza hook accepts --hunks in either flag order and rejects repeats", () => {
  for (const args of [["--hunks"], ["--hunks", "--no-braces"], ["--no-braces", "--hunks"]]) {
    const cwd = repository();
    const result = hook({ cwd }, { args });
    expect(result.code).toBe(0);
    expect(result.stderr).toBe("");
    expect(readFileSync(join(cwd, "a.ts"), "utf8")).not.toBe(readFileSync(fixture, "utf8"));
  }

  const repeated = hook("not json", { args: ["--hunks", "--hunks"] });
  expect(repeated.code).toBe(1);
  expect(repeated.stderr).toStartWith("stanza hook: --hunks given twice\n");
});

test("stanza hook --hunks leaves unchanged code in an edited file alone", () => {
  const cwd = committedSource(`${hunkFunction("f1")}\n\n${hunkFunction("f2")}\n`);
  writeFileSync(join(cwd, "a.ts"), `${hunkFunction("f1")}\n\n${hunkFunction("f2", 3)}\n`);
  expect(hook({ cwd }, { args: ["--hunks"] }).code).toBe(0);

  const fixed = readFileSync(join(cwd, "a.ts"), "utf8");
  expect(fixed).toStartWith(`${hunkFunction("f1")}\n`);
  expect(fixed).toContain("  if (a) return 1;\n  return 3;");
});

test("stanza hook asks to reread a file it rewrote that still has a finding", () => {
  const wall = readFileSync(join(root, "tests", "fixtures", "wall", "wall.before.ts"), "utf8");
  const cwd = repository({ "mixed.ts": `${wall}\n${readFileSync(fixture, "utf8")}` });
  const result = hook({ cwd });
  const reason = JSON.parse(result.stdout).reason;

  expect(reason).toContain("\nmixed.ts\n  line 2: ");
  expect(reason).toContain("\nstanza rewrote mixed.ts, so read it again before editing.\n");
});

test.skipIf(process.getuid?.() === 0)(
  "stanza hook reports a file it cannot write and still checks the rest",
  () => {
    const wall = readFileSync(join(root, "tests", "fixtures", "wall", "wall.before.ts"), "utf8");
    const cwd = repository({ "a.ts": readFileSync(fixture, "utf8"), "z.ts": wall });
    chmodSync(join(cwd, "a.ts"), 0o444);

    const result = hook({ cwd });
    const reason = JSON.parse(result.stdout).reason;

    expect(result.code).toBe(0);
    expect(reason).toMatch(/\na\.ts\n  line 1: .+ \(write\)\n/);
    expect(reason).toContain("\nz.ts\n  line 2: ");
    expect(reason).not.toContain("stanza rewrote a.ts");
    expect(readFileSync(join(cwd, "a.ts"), "utf8")).toBe(readFileSync(fixture, "utf8"));
  },
);

function committed(files: Record<string, string>): string {
  const cwd = scratchGitRepository({ files });
  git(cwd, "add", "-A");
  git(cwd, "commit", "-qm", "agent");
  return cwd;
}

function stopOn(cwd: string, written: string[], args: string[] = []): RunResult {
  const path = transcript(
    userTurn,
    ...toolCalls(...written.map((name): [string, string] => ["Edit", join(cwd, name)])),
  );

  return hook({ cwd, hook_event_name: "Stop", transcript_path: path }, { args });
}

test("Stop fixes and blocks on files the agent wrote and committed", () => {
  const cwd = committed({ ...wallAndBodies(), "human.ts": readFileSync(fixture, "utf8") });
  const result = stopOn(cwd, ["bodies.ts", "wall.ts"]);
  const reason = JSON.parse(result.stdout).reason;

  expect(reason).toContain("\nwall.ts\n  line 2: split these 7 statements");
  expect(reason).not.toContain("human.ts");
  expect(readFileSync(join(cwd, "bodies.ts"))).toEqual(readFileSync(bodiesAfter));
  expect(readFileSync(join(cwd, "human.ts"))).toEqual(readFileSync(fixture));

  expect(result.code).toBe(0);
  expect(result.stderr).toBe("");
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
    expectSilent(hook({ cwd, hook_event_name: "Stop", transcript_path }));
    expect(readFileSync(join(cwd, "edited.ts"))).toEqual(readFileSync(bodiesAfter));
    expect(readFileSync(join(cwd, "old.ts"), "utf8")).toBe(before);
    writeFileSync(join(cwd, "edited.ts"), before);
  }
});
