import { expect, test } from "bun:test";
import { mkdirSync, readFileSync, realpathSync, rmSync, symlinkSync, writeFileSync } from "node:fs";
import { join, relative } from "node:path";
import {
  committedSource,
  git,
  hunkFunction,
  run,
  scratch,
  scratchGitRepository,
  tracedGit,
} from "./support.ts";

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
  event = "PostToolUse",
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

test("PostToolUse names the files a Codex patch formatted in byte order", () => {
  const names = ["Z.ts", "a.ts", "_b.ts"];
  const cwd = scratchGitRepository({
    files: Object.fromEntries(names.map((name) => [name, before])),
  });

  const command = patch(...names.map((name) => `*** Add File: ${name}`));
  const response = `Exit code: 0\nWall time: 0.2 seconds\nOutput:\nSuccess. Updated the following files:\n${names.map((name) => `A ${name}`).join("\n")}\n`;

  expect(context(postToolUse(cwd, "apply_patch", { command }, response))).toBe(
    "stanza formatted Z.ts, _b.ts and a.ts, so read them again before editing.",
  );
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

test("PostToolUse after a Claude Code Write, Edit or MultiEdit fixes the file it wrote", () => {
  for (const tool of ["Write", "Edit", "MultiEdit"]) {
    const cwd = scratchGitRepository({ files: { "src/a.ts": before, "human.ts": before } });
    const file_path = join(cwd, "src/a.ts");
    const result = postToolUse(cwd, tool, { file_path, content: before }, { filePath: file_path });

    expect(result.code).toBe(0);
    expect(result.stderr).toBe("");
    expect(context(result)).toBe(
      `stanza formatted ${join("src", "a.ts")}, so read it again before editing.`,
    );

    expect(readFileSync(file_path, "utf8")).toBe(after);
    expect(readFileSync(join(cwd, "human.ts"), "utf8")).toBe(before);
  }
});

test("PostToolUse fixes an edited file that matches HEAD, and --hunks leaves it alone", () => {
  const cwd = scratchGitRepository({ files: { "a.ts": before }, staged: true });
  git(cwd, "commit", "-qm", "init");
  const file_path = join(cwd, "a.ts");

  const hunks = postToolUse(cwd, "Edit", { file_path }, { filePath: file_path }, ["--hunks"]);
  expect(hunks).toEqual({ code: 0, stdout: "", stderr: "" });
  expect(readFileSync(file_path, "utf8")).toBe(before);

  const result = postToolUse(cwd, "Edit", { file_path }, { filePath: file_path });
  expect(context(result)).toBe("stanza formatted a.ts, so read it again before editing.");
  expect(readFileSync(file_path, "utf8")).toBe(after);
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

test("PostToolUse fixes a written file in a repository other than cwd", () => {
  for (const tool of ["Write", "Edit"]) {
    const cwd = scratchGitRepository({ files: { "a.ts": before } });
    const other = scratchGitRepository({ files: { "b.ts": before, "human.ts": before } });
    const file_path = join(other, "b.ts");
    const result = postToolUse(cwd, tool, { file_path, content: before }, { filePath: file_path });

    expect(result.code).toBe(0);
    expect(result.stderr).toBe("");
    expect(context(result)).toBe(
      `stanza formatted ${realpathSync(file_path)}, so read it again before editing.`,
    );

    expect(readFileSync(file_path, "utf8")).toBe(after);
    expect(readFileSync(join(other, "human.ts"), "utf8")).toBe(before);
    expect(readFileSync(join(cwd, "a.ts"), "utf8")).toBe(before);
  }
});

function removedDirectory(parent: string): string {
  const path = join(parent, "gone");
  mkdirSync(path);
  rmSync(path, { recursive: true });

  return path;
}

function hookAt(processCwd: string, input: Record<string, unknown>): ReturnType<typeof run> {
  return run({ cwd: processCwd, stdin: Buffer.from(JSON.stringify(input)) }, "hook");
}

test("PostToolUse under a removed cwd fixes an absolute path and skips a relative one", () => {
  for (const tool of ["Write", "Edit"]) {
    const repo = scratchGitRepository({ files: { "a.ts": before, "b.ts": before } });
    const cwd = removedDirectory(repo);
    const edit = (file_path: string) =>
      hookAt(repo, {
        cwd,
        hook_event_name: "PostToolUse",
        tool_name: tool,
        tool_input: { file_path, content: before },
      });

    expect(edit("../b.ts")).toEqual({ code: 0, stdout: "", stderr: "" });
    expect(readFileSync(join(repo, "b.ts"), "utf8")).toBe(before);

    const result = edit(join(repo, "a.ts"));
    expect(result.code).toBe(0);
    expect(result.stderr).toBe("");
    expect(context(result)).toBe(
      `stanza formatted ${join(repo, "a.ts")}, so read it again before editing.`,
    );

    expect(readFileSync(join(repo, "a.ts"), "utf8")).toBe(after);
  }
});

test("a failed repository does not cancel another repository", () => {
  const control = scratchGitRepository({ files: { "a.ts": before } });
  const cwd = scratchGitRepository({ files: { "a.ts": before } });
  const broken = scratchGitRepository({ files: { "b.ts": before } });
  writeFileSync(join(broken, ".git", "index"), "junkjunkjunkjunkjunk");

  const applyPatch = (repo: string, ...extra: string[]) => {
    const names = [...extra, join(repo, "a.ts")];
    const command = patch(...names.map((name) => `*** Update File: ${name}`));
    const response = `Exit code: 0\nWall time: 0.2 seconds\nOutput:\nSuccess. Updated the following files:\n${names.map((name) => `M ${name}`).join("\n")}\n`;
    return postToolUse(repo, "apply_patch", { command }, response);
  };

  const expected = applyPatch(control);
  expect(expected.code).toBe(0);
  expect(context(expected)).toBe("stanza formatted a.ts, so read it again before editing.");

  const result = applyPatch(cwd, join(broken, "b.ts"));
  expect(result.stdout).toBe(expected.stdout);
  expect(result.stderr).toBe(
    `stanza hook: git ls-files failed in ${relative(realpathSync(cwd), realpathSync(broken))}: fatal: .git/index: index file smaller than expected\n`,
  );

  expect(result.code).toBe(1);
  expect(readFileSync(join(cwd, "a.ts"), "utf8")).toBe(after);
  expect(readFileSync(join(broken, "b.ts"), "utf8")).toBe(before);
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

function linkedRepository(files: Record<string, string> = {}): string {
  const root = scratchGitRepository({ files: { "a.ts": before, "real/a.ts": before, ...files } });
  mkdirSync(join(root, "real/sub"));
  symlinkSync(join(root, "real/sub"), join(root, "link"));
  return root;
}

const linkedPaths = [
  { cwd: "link", path: "../a.ts" },
  { cwd: ".", path: "link/../a.ts" },
];

test("PostToolUse joins a relative path to cwd as the tool does, without following symlinks", () => {
  for (const tool of ["Write", "Edit", "MultiEdit", "apply_patch"])
    for (const linked of linkedPaths) {
      const root = linkedRepository({ ".gitattributes": "/real/a.ts -stanza\n" });
      const cwd = join(root, linked.cwd);
      const result =
        tool === "apply_patch"
          ? postToolUse(cwd, tool, { command: patch(`*** Update File: ${linked.path}`) }, applied)
          : postToolUse(cwd, tool, { file_path: linked.path, content: before }, {});

      const shown = linked.cwd === "link" ? join(root, "a.ts") : "a.ts";
      expect(result.code).toBe(0);
      expect(result.stderr).toBe("");
      expect(context(result)).toBe(`stanza formatted ${shown}, so read it again before editing.`);

      expect(readFileSync(join(root, "a.ts"), "utf8")).toBe(after);
      expect(readFileSync(join(root, "real/a.ts"), "utf8")).toBe(before);
    }
});

test("a relative path through a symlink still skips a file linked out of the repository", () => {
  const outside = join(scratch("outside"), "a.ts");
  writeFileSync(outside, before);
  const root = linkedRepository({ "real/out.ts": before });
  symlinkSync(outside, join(root, "out.ts"));

  const edit = postToolUse(root, "Edit", { file_path: "link/../out.ts" }, {});
  const write = postToolUse(root, "Write", { file_path: "link/../out.ts", content: before }, {});

  expect(edit).toEqual({ code: 0, stdout: "", stderr: "" });
  expect(write).toEqual({ code: 0, stdout: "", stderr: "" });
  expect(readFileSync(outside, "utf8")).toBe(before);
  expect(readFileSync(join(root, "real/out.ts"), "utf8")).toBe(before);
});

test("PostToolUse prints nothing for a clean file, a file outside git or a missing file", () => {
  const cwd = scratchGitRepository({ files: { "a.ts": after } });
  const outside = scratch("outside");
  writeFileSync(join(outside, "a.ts"), before);

  for (const tool of ["Write", "Edit"])
    for (const [dir, name] of [
      [cwd, "a.ts"],
      [cwd, "missing.ts"],
      [outside, "a.ts"],
    ])
      expect(
        postToolUse(dir!, tool, { file_path: join(dir!, name!), content: before }, {}),
      ).toEqual({ code: 0, stderr: "", stdout: "" });

  expect(readFileSync(join(outside, "a.ts"), "utf8")).toBe(before);
});

test("PostToolUse takes --hunks and --no-braces as the Stop pass does", () => {
  const cwd = committedSource(`${hunkFunction("f1")}\n\n${hunkFunction("f2")}\n`);
  const path = join(cwd, "a.ts");
  writeFileSync(path, `${hunkFunction("f1")}\n\n${hunkFunction("f2", 3)}\n`);

  expect(postToolUse(cwd, "Edit", { file_path: path }, {}, ["--hunks"]).code).toBe(0);

  const fixed = readFileSync(path, "utf8");
  expect(fixed).toStartWith(`${hunkFunction("f1")}\n`);
  expect(fixed).toContain("  if (a) return 1;\n  return 3;");

  writeFileSync(path, before);
  expect(postToolUse(cwd, "Edit", { file_path: path }, {}, ["--no-braces"]).code).toBe(0);
  expect(readFileSync(path, "utf8")).toContain("if (owner) { mark(items[0]); }");
});

test("PreToolUse prints nothing and leaves the written file alone", () => {
  const cwd = scratchGitRepository({ files: { "a.ts": before, "b.ts": before } });
  const file_path = join(cwd, "a.ts");
  const transcript_path = join(cwd, "session.jsonl");
  writeFileSync(
    transcript_path,
    `${JSON.stringify({
      type: "assistant",
      message: {
        content: [
          { type: "tool_use", id: "t", name: "Edit", input: { file_path: join(cwd, "b.ts") } },
        ],
      },
    })}\n`,
  );

  for (const extra of [{}, { cwd: 42 }, { transcript_path }]) {
    const input = {
      cwd,
      hook_event_name: "PreToolUse",
      tool_name: "Write",
      tool_input: { file_path, content: before },
      ...extra,
    };

    const env = { ...process.env, AGENT_HOOKS: "1" };
    expect(run({ cwd, env, stdin: Buffer.from(JSON.stringify(input)) }, "hook")).toEqual({
      code: 0,
      stdout: "",
      stderr: "",
    });
  }

  expect(readFileSync(file_path, "utf8")).toBe(before);
  expect(readFileSync(join(cwd, "b.ts"), "utf8")).toBe(before);
});

test("PostToolUse Write leaves unsupported, generated, invalid and clean files alone", () => {
  const cwd = scratchGitRepository({
    files: {
      "notes.md": before,
      "gen.ts": `// @generated\n${before}`,
      "bad.ts": "function (",
      "clean.ts": after,
      "braced.ts": before,
    },
  });

  for (const name of ["notes.md", "gen.ts", "bad.ts", "clean.ts"]) {
    const file_path = join(cwd, name);
    const content = readFileSync(file_path, "utf8");

    expect(writeHook(cwd, { file_path, content })).toEqual({ code: 0, stderr: "", stdout: "" });
    expect(readFileSync(file_path, "utf8")).toBe(content);
  }

  const braced = join(cwd, "braced.ts");
  expect(context(writeHook(cwd, { file_path: braced, content: before }))).toBe(
    "stanza formatted braced.ts, so read it again before editing.",
  );

  expect(readFileSync(braced, "utf8")).toBe(after);

  expect(writeHook(cwd, { content: before })).toEqual({ code: 0, stderr: "", stdout: "" });
});

test("malformed text in hook input names no file", () => {
  const cwd = scratchGitRepository({ files: { "�.ts": before } });
  const sent = JSON.stringify({
    cwd,
    hook_event_name: "PostToolUse",
    tool_name: "Edit",
    tool_input: { file_path: "\ud800.ts" },
  });

  const hook = (stdin: Buffer) =>
    run({ cwd, env: { ...process.env, AGENT_HOOKS: "1" }, stdin }, "hook");

  expect(sent).toContain("\\ud800");
  expect(hook(Buffer.from(sent))).toEqual({ code: 0, stdout: "", stderr: "" });

  const bytes = Buffer.from(sent.replace("\\ud800", "BAD"));
  bytes.fill(0xff, bytes.indexOf("BAD"), bytes.indexOf("BAD") + 3);
  expect(hook(bytes)).toEqual({
    code: 1,
    stdout: "",
    stderr: "stanza hook: input must be valid UTF-8\n",
  });

  const change = {
    type: "FileChange",
    status: "completed",
    changes: { [join(cwd, "BAD.ts")]: {} },
  };

  const record = JSON.stringify({
    type: "event_msg",
    payload: { type: "item_completed", item: change },
  });

  const rollout = Buffer.from(`${record}\n`);
  rollout.fill(0xff, rollout.indexOf("BAD"), rollout.indexOf("BAD") + 3);
  const transcript_path = join(scratch("rollout"), "rollout.jsonl");
  writeFileSync(transcript_path, rollout);

  const stop = JSON.stringify({ cwd, hook_event_name: "Stop", transcript_path });
  expect(hook(Buffer.from(stop))).toEqual({ code: 0, stdout: "", stderr: "" });

  const session = join(scratch("session"), "session.jsonl");
  writeFileSync(
    session,
    `${JSON.stringify({ type: "assistant", "\ud800": 1, message: { content: [] } })}\n`,
  );

  const keyed = JSON.stringify({ cwd, hook_event_name: "Stop", transcript_path: session });
  expect(hook(Buffer.from(keyed))).toEqual({ code: 0, stdout: "", stderr: "" });

  expect(readFileSync(join(cwd, "�.ts"), "utf8")).toBe(before);
});

test("PostToolUse Write skips paths outside the repository, symlinks out and ignored files", () => {
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

  writeFileSync(join(cwd, "ignored.ts"), before);

  for (const path of [file, ...paths])
    expect(writeHook(cwd, { file_path: path, content: before })).toEqual({
      code: 0,
      stderr: "",
      stdout: "",
    });

  expect(readFileSync(file, "utf8")).toBe(before);
  expect(readFileSync(join(cwd, "ignored.ts"), "utf8")).toBe(before);

  const kept = join(cwd, "kept.ts");
  writeFileSync(kept, before);
  expect(context(writeHook(cwd, { file_path: kept, content: before }))).toBe(
    "stanza formatted kept.ts, so read it again before editing.",
  );
});

test("PostToolUse Write with --hunks fixes the HEAD diff of a tracked file and all of a new one", () => {
  const kept = `${hunkFunction("f1")}\n`;
  const cwd = committedSource(`${kept}\n${hunkFunction("f2")}\n`);
  const file_path = join(cwd, "a.ts");
  writeFileSync(file_path, `${kept}\n${hunkFunction("f2", 3)}\n`);
  const tracked = writeHook(cwd, { file_path, content: readFileSync(file_path, "utf8") }, [
    "--hunks",
  ]);

  expect(tracked.code).toBe(0);
  expect(tracked.stderr).toBe("");
  expect(context(tracked)).toBe("stanza formatted a.ts, so read it again before editing.");
  expect(readFileSync(file_path, "utf8")).toBe(
    `${kept}\nfunction f2(a: boolean) {\n  if (a) return 1;\n  return 3;\n}\n`,
  );

  const fresh = join(cwd, "new.ts");
  writeFileSync(fresh, before);
  const created = writeHook(cwd, { file_path: fresh, content: before }, ["--hunks"]);
  expect(created.code).toBe(0);
  expect(readFileSync(fresh, "utf8")).toBe(after);
});

test("PostToolUse Write applies --no-braces", () => {
  const cwd = scratchGitRepository({ files: { "a.ts": before } });
  const path = join(cwd, "a.ts");
  const result = writeHook(cwd, { file_path: path, content: before }, ["--no-braces"]);
  expect(context(result)).toBe("stanza formatted a.ts, so read it again before editing.");

  const fixed = readFileSync(path, "utf8");
  expect(fixed).not.toBe(before);
  expect(fixed).toContain("if (owner) { mark(items[0]); }");
});

test("PostToolUse Write reports a git failure without a block", () => {
  const cwd = scratchGitRepository({ files: { "a.ts": before } });
  const env = {
    ...process.env,
    GIT_CONFIG_GLOBAL: "/dev/null",
    GIT_CONFIG_NOSYSTEM: "1",
    GIT_TEST_ASSUME_DIFFERENT_OWNER: "1",
  };

  const input = {
    cwd,
    hook_event_name: "PostToolUse",
    tool_name: "Write",
    tool_input: { file_path: join(cwd, "a.ts"), content: before },
  };

  const result = run({ cwd, env, stdin: Buffer.from(JSON.stringify(input)) }, "hook");
  expect(result.code).toBe(1);
  expect(result.stdout).toBe("");
  expect(result.stderr).toContain("dubious ownership");
  expect(readFileSync(join(cwd, "a.ts"), "utf8")).toBe(before);
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

test("config notices read the same from a run and from the hook, each under its own name", () => {
  const notices = [
    "cannot tell whether eslint.config.js enforces braces, so braces stay; pass --braces or --no-braces to settle it",
    "cannot read the line width from .prettierrc.json, so the next source sets it",
  ];

  const cwd = scratchGitRepository({
    files: {
      "eslint.config.js": "export default makeConfig();\n",
      ".prettierrc.json": "{",
      "a.ts": "export const a = 1;\n",
    },
  });

  const runs: [string, ReturnType<typeof run>][] = [
    ["stanza: ", run({ cwd }, "--check", "a.ts")],
    ["stanza hook: ", writeHook(cwd, { file_path: join(cwd, "a.ts") }, [], "PostToolUse", "Edit")],
  ];

  for (const [prefix, result] of runs) {
    const expected = notices.map((notice) => `${prefix}${notice}`);
    expect([prefix, result.code, result.stdout, named(result.stderr)]).toEqual([
      prefix,
      0,
      "",
      expected,
    ]);
  }
});

function named(stderr: string): string[] {
  return stderr.split("\n").filter((line) => line.startsWith("stanza"));
}

function sharedConfig(levels: number, next: (previous: string) => string): string {
  const lines = ['const c0 = { rules: { curly: "off" } };'];
  for (let level = 1; level <= levels; level++)
    lines.push(`const c${level} = ${next(`c${level - 1}`)};`);

  return `${lines.join("\n")}\nexport default c${levels};\n`;
}

test("--check reads braces through shared lint configs and names an uncertain one", () => {
  const unread =
    "stanza: cannot tell whether eslint.config.js enforces braces, so braces stay; pass --braces or --no-braces to settle it";

  const shapes: [string, (previous: string) => string, number, string[], string[]][] = [
    ["eslint.config.js", (previous) => `[${previous}, ${previous}]`, 1, ["braces"], []],
    [
      "oxlint.config.ts",
      (previous) => `{ extends: [${previous}, ${previous}] }`,
      1,
      ["braces"],
      [],
    ],
    [
      "eslint.config.js",
      (previous) =>
        `[{ files: ["**/*.ts"], extends: [${previous}] }, { files: ["**/*.js"], extends: [${previous}] }]`,
      0,
      [],
      [unread],
    ],
  ];

  for (const [config, next, code, rules, notices] of shapes) {
    const cwd = scratch("config");
    writeFileSync(join(cwd, config), sharedConfig(30, next));
    writeFileSync(join(cwd, "x.ts"), "if (ok) {\n  run();\n}\n");

    const json = run({ cwd }, "--check", "--json", "x.ts");
    const found = JSON.parse(json.stdout).map((finding: { rule: string }) => finding.rule);
    expect([config, json.code, found, named(json.stderr)]).toEqual([config, code, rules, notices]);

    const plain = run({ cwd }, "--check", "x.ts");
    const rows = plain.stdout.split("\n").filter((line) => line.includes(": braces: "));
    expect([config, plain.code, rows.length, named(plain.stderr)]).toEqual([
      config,
      code,
      rules.length,
      notices,
    ]);
  }
});
