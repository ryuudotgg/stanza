import { expect, spyOn, test } from "bun:test";
import { existsSync, readFileSync, realpathSync, symlinkSync, writeFileSync } from "node:fs";
import { join } from "node:path";
import { blockReason, hookInput } from "../src/hook.ts";
import { RULES } from "../src/engine/rules.ts";
import type { Finding } from "../src/engine/types.ts";
import { gitBinary, run, scratch, scratchGitRepository } from "./support.ts";

const before = readFileSync(join(import.meta.dir, "fixtures/braces/bodies.before.ts"), "utf8");
const after = readFileSync(join(import.meta.dir, "fixtures/braces/bodies.after.ts"), "utf8");

test("PostToolUse Edit of package.json exits without spawning git", () => {
  const cwd = scratchGitRepository({ files: { "package.json": "{}\n" } });
  const spawning = spyOn(Bun, "spawnSync");
  try {
    const result = writeHook(cwd, { file_path: "package.json" }, [], "PostToolUse", "Edit");
    expect(result).toEqual({ code: 0, stdout: "", stderr: "" });
    expect(spawning).not.toHaveBeenCalled();
  } finally {
    spawning.mockRestore();
  }
});

test("a fresh hook process with nothing to format never loads the formatter", () => {
  const cwd = scratchGitRepository({ files: { "package.json": "{}\n" } });
  const start = join(import.meta.dir, "../src/start.ts");
  const script = `const { start } = await import(${JSON.stringify(start)});
const code = await start(["hook"]);
console.log(JSON.stringify({ code, loaded: Object.keys(require.cache) }));`;

  const inputs = [
    { hook_event_name: "Notification" },
    {
      hook_event_name: "PostToolUse",
      cwd,
      tool_name: "Edit",
      tool_input: { file_path: join(cwd, "package.json") },
    },
  ];

  for (const input of inputs) {
    const result = Bun.spawnSync(["bun", "-e", script], {
      cwd,
      stdin: Buffer.from(JSON.stringify(input)),
    });

    const { code, loaded } = JSON.parse(result.stdout.toString()) as {
      code: number;
      loaded: string[];
    };

    const formatter = loaded.filter((path) =>
      /[\\/]src[\\/](cli|step|files)\.ts$|[\\/]src[\\/]engine[\\/]/.test(path),
    );

    expect(code).toBe(0);
    expect(loaded.some((path) => path.endsWith("hook.ts"))).toBe(true);
    expect(formatter).toEqual([]);
  }
});

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

test("hookInput allowlists Stop, SubagentStop, PreToolUse and PostToolUse", () => {
  for (const event of ["Stop", null])
    expect(hookInput(JSON.stringify({ hook_event_name: event }), "/repo")).toEqual({
      event: "stop",
      cwd: "/repo",
      stopHookActive: false,
      transcriptPath: undefined,
    });

  expect(hookInput('{"hook_event_name":"SubagentStop"}', "/repo")).toStrictEqual({
    event: "stop",
    cwd: "/repo",
    stopHookActive: false,
    agentTranscriptPath: undefined,
  });

  for (const event of ["UserPromptSubmit", "PostToolUseFailure"])
    expect(hookInput(JSON.stringify({ hook_event_name: event, cwd: 42 }), "/repo")).toEqual({
      event: "ignored",
    });

  expect(hookInput('{"hook_event_name":"PostToolUse","cwd":42}', "/repo")).toEqual({
    error: "cwd must be a string",
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

const applied =
  "Exit code: 0\nWall time: 0.2 seconds\nOutput:\nSuccess. Updated the following files:\nA a.ts\nM b.ts\nM c.ts\nD gone.ts\n";

function patch(...lines: string[]): string {
  return ["*** Begin Patch", ...lines, "*** End Patch"].join("\n");
}

const codexPatch = patch(
  "*** Add File: a.ts",
  "+const a = 1;",
  "*** Update File: b.ts",
  "@@",
  "-const b = 1;",
  "+const b = 2;",
  "*** Update File: old.ts",
  "*** Move to: sub/../c.ts",
  "*** Delete File: gone.ts",
);

function postToolUse(
  cwd: string,
  tool: string,
  toolInput: Record<string, unknown>,
  toolResponse: unknown,
  args: string[] = [],
): ReturnType<typeof run> {
  const input = {
    cwd,
    hook_event_name: "PostToolUse",
    tool_name: tool,
    tool_input: toolInput,
    tool_response: toolResponse,
    transcript_path: join(cwd, "transcript.jsonl"),
  };

  return run({ cwd, stdin: Buffer.from(JSON.stringify(input)) }, "hook", ...args);
}

function context(result: ReturnType<typeof run>): string {
  const output = JSON.parse(result.stdout);
  expect(Object.keys(output)).toEqual(["hookSpecificOutput"]);
  expect(output.hookSpecificOutput.hookEventName).toBe("PostToolUse");
  return output.hookSpecificOutput.additionalContext;
}

test("hookInput reads the paths a PostToolUse patch or edit wrote", () => {
  const codex = {
    hook_event_name: "PostToolUse",
    cwd: "sub",
    tool_name: "apply_patch",
    tool_input: { command: codexPatch },
    tool_response: applied,
  };

  expect(hookInput(JSON.stringify(codex), "/repo")).toEqual({
    event: "edit",
    cwd: "/repo/sub",
    paths: ["/repo/sub/a.ts", "/repo/sub/b.ts", "/repo/sub/c.ts"],
  });

  const absolute = { ...codex, tool_input: { command: patch("*** Update File: /abs/x.ts") } };
  expect(hookInput(JSON.stringify(absolute), "/repo")).toMatchObject({ paths: ["/abs/x.ts"] });

  for (const tool_name of ["Edit", "MultiEdit"])
    expect(
      hookInput(
        JSON.stringify({
          hook_event_name: "PostToolUse",
          tool_name,
          tool_input: { file_path: "src/a.ts" },
          tool_response: { filePath: "src/a.ts" },
        }),
        "/repo",
      ),
    ).toEqual({ event: "edit", cwd: "/repo", paths: ["/repo/src/a.ts"] });
});

test("hookInput ignores a failed, empty or foreign PostToolUse", () => {
  const cases = [
    {
      tool_name: "apply_patch",
      tool_input: { command: codexPatch },
      tool_response: "apply_patch verification failed: Failed to find expected lines",
    },
    {
      tool_name: "apply_patch",
      tool_input: { command: codexPatch },
      tool_response: "verification failed, expected: Success. Updated the following files:",
    },
    { tool_name: "apply_patch", tool_input: { command: codexPatch }, tool_response: { applied } },
    { tool_name: "apply_patch", tool_input: { command: codexPatch } },
    {
      tool_name: "apply_patch",
      tool_input: { command: patch("*** Delete File: a.ts") },
      tool_response: applied,
    },
    { tool_name: "apply_patch", tool_input: { command: 42 }, tool_response: applied },
    { tool_name: "Write", tool_input: { file_path: "a.ts", content: before } },
    { tool_name: "Edit", tool_input: { file_path: 42 } },
    { tool_name: "Bash", tool_input: { command: codexPatch }, tool_response: applied },
  ];

  for (const input of cases)
    expect(
      hookInput(JSON.stringify({ hook_event_name: "PostToolUse", ...input }), "/repo"),
    ).toEqual({
      event: "ignored",
    });
});

test("PostToolUse after a Codex patch fixes each file it wrote and nothing else", () => {
  const cwd = scratchGitRepository({
    files: {
      "a.ts": before,
      "b.ts": before,
      "c.ts": before,
      "human.ts": before,
      "notes.md": before,
      "ignored.ts": before,
      "node_modules/dep/x.ts": before,
      "gen.ts": `// @generated\n${before}`,
      "attr.ts": before,
      ".gitignore": "ignored.ts\n",
      ".gitattributes": "attr.ts linguist-generated\n",
    },
  });

  const outside = scratch("outside");
  writeFileSync(join(outside, "x.ts"), before);

  const command = [
    codexPatch,
    patch(
      "*** Add File: notes.md",
      "*** Add File: ignored.ts",
      "*** Add File: node_modules/dep/x.ts",
      "*** Add File: gen.ts",
      "*** Add File: attr.ts",
      `*** Add File: ${join(outside, "x.ts")}`,
    ),
  ].join("\n");

  const result = postToolUse(cwd, "apply_patch", { command }, applied);

  expect(result.code).toBe(0);
  expect(result.stderr).toBe("");
  expect(context(result)).toBe(
    "stanza formatted a.ts, b.ts and c.ts, so read them again before editing.",
  );

  for (const name of ["a.ts", "b.ts", "c.ts"])
    expect(readFileSync(join(cwd, name), "utf8")).toBe(after);

  for (const name of ["human.ts", "notes.md", "ignored.ts", "node_modules/dep/x.ts", "attr.ts"])
    expect(readFileSync(join(cwd, name), "utf8")).toBe(before);

  expect(readFileSync(join(cwd, "gen.ts"), "utf8")).toBe(`// @generated\n${before}`);

  const stop = run({ cwd, stdin: Buffer.from(JSON.stringify({ cwd })) }, "hook");
  expect(stop.code).toBe(0);
  expect(readFileSync(join(cwd, "a.ts"), "utf8")).toBe(after);

  expect(readFileSync(join(outside, "x.ts"), "utf8")).toBe(before);
});

test("PostToolUse after a failed Codex patch changes nothing", () => {
  const cwd = scratchGitRepository({ files: { "a.ts": before, "b.ts": before, "c.ts": before } });
  const failed = "apply_patch verification failed: Failed to find expected lines in b.ts";

  expect(postToolUse(cwd, "apply_patch", { command: codexPatch }, failed)).toEqual({
    code: 0,
    stderr: "",
    stdout: "",
  });

  for (const name of ["a.ts", "b.ts", "c.ts"])
    expect(readFileSync(join(cwd, name), "utf8")).toBe(before);
});

test("PostToolUse after a Claude Code Edit or MultiEdit fixes the edited file", () => {
  for (const tool of ["Edit", "MultiEdit"]) {
    const cwd = scratchGitRepository({ files: { "src/a.ts": before, "human.ts": before } });
    const file_path = join(cwd, "src/a.ts");
    const result = postToolUse(cwd, tool, { file_path }, { filePath: file_path });

    expect(result.code).toBe(0);
    expect(result.stderr).toBe("");
    expect(context(result)).toBe(
      `stanza formatted ${join("src", "a.ts")}, so read it again before editing.`,
    );

    expect(readFileSync(file_path, "utf8")).toBe(after);
    expect(readFileSync(join(cwd, "human.ts"), "utf8")).toBe(before);
  }
});

test("PostToolUse reads a captured Codex 0.159.0 apply_patch payload", () => {
  const cwd = scratchGitRepository({ files: { "sample.ts": before, "human.ts": before } });
  const input = readFileSync(
    join(import.meta.dir, "fixtures/codex/post-tool-use-0.159.0.json"),
    "utf8",
  )
    .replace("{{cwd}}", cwd)
    .replace("{{transcript}}", join(cwd, "rollout.jsonl"));

  const result = run({ cwd, stdin: Buffer.from(input) }, "hook");

  expect(result.code).toBe(0);
  expect(result.stderr).toBe("");
  expect(context(result)).toBe("stanza formatted sample.ts, so read it again before editing.");
  expect(readFileSync(join(cwd, "sample.ts"), "utf8")).toBe(after);
  expect(readFileSync(join(cwd, "human.ts"), "utf8")).toBe(before);
});

test("PostToolUse fixes an edited file in a repository other than cwd", () => {
  const cwd = scratchGitRepository({ files: { "a.ts": before } });
  const other = scratchGitRepository({ files: { "b.ts": before, "human.ts": before } });
  const file_path = join(other, "b.ts");
  const result = postToolUse(cwd, "Edit", { file_path }, { filePath: file_path });

  expect(result.code).toBe(0);
  expect(result.stderr).toBe("");
  expect(context(result)).toBe(
    `stanza formatted ${realpathSync(file_path)}, so read it again before editing.`,
  );

  expect(readFileSync(file_path, "utf8")).toBe(after);
  expect(readFileSync(join(other, "human.ts"), "utf8")).toBe(before);
  expect(readFileSync(join(cwd, "a.ts"), "utf8")).toBe(before);
});

test("PostToolUse resolves a relative patch path against a cwd below the root", () => {
  const cwd = scratchGitRepository({ files: { "pkg/src/a.ts": before, "src/a.ts": before } });
  const result = postToolUse(
    join(cwd, "pkg"),
    "apply_patch",
    { command: patch("*** Update File: src/a.ts") },
    applied,
    ["--braces"],
  );

  expect(context(result)).toBe(
    `stanza formatted ${join("src", "a.ts")}, so read it again before editing.`,
  );

  expect(readFileSync(join(cwd, "pkg/src/a.ts"), "utf8")).toBe(after);
  expect(readFileSync(join(cwd, "src/a.ts"), "utf8")).toBe(before);
});

test("PostToolUse prints nothing for a clean file, a file outside git or a missing file", () => {
  const cwd = scratchGitRepository({ files: { "a.ts": after } });
  const outside = scratch("outside");
  writeFileSync(join(outside, "a.ts"), before);

  for (const [dir, name] of [
    [cwd, "a.ts"],
    [cwd, "missing.ts"],
    [outside, "a.ts"],
  ])
    expect(postToolUse(dir!, "Edit", { file_path: join(dir!, name!) }, {})).toEqual({
      code: 0,
      stderr: "",
      stdout: "",
    });

  expect(readFileSync(join(outside, "a.ts"), "utf8")).toBe(before);
});

test("PostToolUse takes --hunks and --no-braces as the Stop pass does", () => {
  const block = (name: string, value: number) =>
    `function ${name}(a: boolean) {\n  if (a) {\n    return 1;\n  }\n  return ${value};\n}\n`;

  const cwd = scratchGitRepository({ files: { "a.ts": `${block("f1", 2)}\n${block("f2", 2)}` } });
  for (const args of [
    ["add", "a.ts"],
    [
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
  ])
    expect(Bun.spawnSync([gitBinary, ...args], { cwd }).exitCode).toBe(0);

  const path = join(cwd, "a.ts");
  writeFileSync(path, `${block("f1", 2)}\n${block("f2", 3)}`);

  expect(postToolUse(cwd, "Edit", { file_path: path }, {}, ["--hunks"]).code).toBe(0);

  const fixed = readFileSync(path, "utf8");
  expect(fixed).toStartWith(block("f1", 2));
  expect(fixed).toContain("  if (a)\n    return 1;\n  return 3;");

  writeFileSync(path, before);
  expect(postToolUse(cwd, "Edit", { file_path: path }, {}, ["--no-braces"]).code).toBe(0);
  expect(readFileSync(path, "utf8")).toContain("if (owner) { mark(items[0]); }");
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

test("PreToolUse formats a Write into a repository other than cwd", () => {
  const cwd = scratchGitRepository();
  const file_path = join(scratchGitRepository(), "a.ts");
  const result = writeHook(cwd, { file_path, content: before });

  expect(result.code).toBe(0);
  expect(result.stderr).toBe("");
  expect(JSON.parse(result.stdout).hookSpecificOutput.updatedInput).toEqual({
    file_path,
    content: after,
  });
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
      gitBinary,
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
