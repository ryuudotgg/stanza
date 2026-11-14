import { expect, test } from "bun:test";
import { existsSync, readFileSync, realpathSync, symlinkSync, writeFileSync } from "node:fs";
import { join } from "node:path";
import { gitBinary, run, scratch, scratchGitRepository, tracedGit } from "./support.ts";

const before = readFileSync(join(import.meta.dir, "fixtures/braces/bodies.before.ts"), "utf8");
const after = readFileSync(join(import.meta.dir, "fixtures/braces/bodies.after.ts"), "utf8");

test("PostToolUse Edit of package.json exits without spawning git", () => {
  const cwd = scratchGitRepository({ files: { "package.json": "{}\n" } });
  const { result, calls } = tracedGit((env) =>
    writeHook(cwd, { file_path: "package.json" }, [], "PostToolUse", "Edit", {
      ...env,
      AGENT_HOOKS: "1",
    }),
  );

  expect(result).toEqual({ code: 0, stdout: "", stderr: "" });
  expect(calls).toEqual([]);
});

function writeHook(
  cwd: string,
  toolInput: Record<string, unknown>,
  args: string[] = [],
  event = "PreToolUse",
  tool = "Write",
  env: NodeJS.ProcessEnv = { ...process.env, AGENT_HOOKS: "1" },
): ReturnType<typeof run> {
  return run(
    {
      cwd,
      env,
      stdin: Buffer.from(
        JSON.stringify({ cwd, hook_event_name: event, tool_name: tool, tool_input: toolInput }),
      ),
    },
    "hook",
    ...args,
  );
}

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

test("PostToolUse reads Codex patch paths as Codex splits and trims its lines", () => {
  const cases = [
    ["p\u2028q.ts", "A", ["*** Add File: p\u2028q.ts", "+x"]],
    ["plain.ts", "A", ["*** Add File: plain.ts", "+x"]],
    ["plain.ts", "M", ["*** Update File: plain.ts ", "@@", "-x", "+y"]],
    ["plain.ts", "M", ["  *** Update File: plain.ts", "@@", "-x", "+y"]],
  ] as const;

  for (const [name, status, lines] of cases)
    for (const end of ["\n", "\r\n"]) {
      const cwd = scratchGitRepository({ files: { [name]: before } });
      const command = patch(...lines).replaceAll("\n", end);
      const response = `Success. Updated the following files:${end}${status} ${name}${end}`;
      const result = postToolUse(cwd, "apply_patch", { command }, response);

      expect(result.code).toBe(0);
      expect(result.stderr).toBe("");
      expect(context(result)).toBe(`stanza formatted ${name}, so read it again before editing.`);
      expect(readFileSync(join(cwd, name), "utf8")).toBe(after);
    }
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
  expect(fixed).toContain("  if (a) return 1;\n  return 3;");

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

test("the hook leaves a file the stanza attribute opts out alone on Edit and Stop", () => {
  const cwd = scratchGitRepository({
    files: { "pinned.ts": before, ".gitattributes": "pinned.ts -stanza\n" },
  });

  const file_path = join(cwd, "pinned.ts");
  const edit = postToolUse(cwd, "Edit", { file_path }, { filePath: file_path });
  expect(edit).toEqual({ code: 0, stdout: "", stderr: "" });

  const stop = run({ cwd, stdin: Buffer.from(JSON.stringify({ cwd })) }, "hook");
  expect(stop).toEqual({ code: 0, stdout: "", stderr: "" });
  expect(readFileSync(file_path, "utf8")).toBe(before);
});
