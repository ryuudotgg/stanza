import { expect, test } from "bun:test";
import {
  copyFileSync,
  existsSync,
  mkdirSync,
  mkdtempSync,
  chmodSync,
  readFileSync,
  realpathSync,
  rmSync,
  symlinkSync,
  writeFileSync,
} from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";

const root = join(import.meta.dir, "..");
const fixture = join(root, "tests", "fixtures", "braces", "bodies.before.ts");

function rootWithoutBinary(): string {
  const dir = mkdtempSync(join(tmpdir(), "stanza-hooks-"));
  mkdirSync(join(dir, "git-hooks"));

  copyFileSync(join(root, "hook.sh"), join(dir, "hook.sh"));
  copyFileSync(join(root, "git-hooks", "pre-commit"), join(dir, "git-hooks", "pre-commit"));
  symlinkSync(join(root, "src"), join(dir, "src"));
  symlinkSync(join(root, "node_modules"), join(dir, "node_modules"));

  return dir;
}

function rootWithBinary(script: string): string {
  const dir = rootWithoutBinary();
  mkdirSync(join(dir, "bin"));
  writeFileSync(join(dir, "bin", "stanza"), `#!/bin/sh\n${script}\n`);
  chmodSync(join(dir, "bin", "stanza"), 0o755);

  return dir;
}

function pathWithoutBun(): string {
  const dir = mkdtempSync(join(tmpdir(), "stanza-hooks-path-"));
  const tools = ["basename", "dirname", "git"];
  for (const tool of tools) symlinkSync(Bun.which(tool)!, join(dir, tool));

  return dir;
}

function repository(
  files: Record<string, string> = { "a.ts": readFileSync(fixture, "utf8") },
): string {
  const dir = mkdtempSync(join(tmpdir(), "stanza-hooks-repo-"));
  Bun.spawnSync(["git", "init", "-q"], { cwd: dir });

  for (const [path, text] of Object.entries(files)) {
    mkdirSync(join(dir, path, ".."), { recursive: true });
    writeFileSync(join(dir, path), text);
  }

  const staged = Object.keys(files).filter((path) => !path.startsWith("node_modules/"));
  Bun.spawnSync(["git", "add", "--", ...staged], { cwd: dir });
  return dir;
}

function braces(text: string): number {
  return text.split("{").length - 1;
}

function hookEnv(flags = ""): Record<string, string | undefined> {
  return { ...process.env, AGENT_HOOKS: "1", STANZA_FLAGS: flags };
}

function preCommit(
  files?: Record<string, string>,
  flags?: string,
  hookRoot = rootWithoutBinary(),
  path = process.env.PATH,
): ReturnType<typeof Bun.spawnSync> {
  const cwd = repository(files);
  const result = Bun.spawnSync([join(hookRoot, "git-hooks", "pre-commit")], {
    cwd,
    env: { ...hookEnv(flags), PATH: path },
  });

  return result;
}

function output(result: ReturnType<typeof Bun.spawnSync>): string {
  return new TextDecoder().decode(result.stdout);
}

function rules(findings: string, path: string): string[] {
  return findings
    .split("\n")
    .filter((line) => line.startsWith(`${path}:`))
    .map((line) => line.split(" ")[1] ?? "");
}

function stopHookOutput(files: Record<string, string>, hookRoot = rootWithoutBinary()): string {
  const cwd = repository(files);
  const result = Bun.spawnSync([join(hookRoot, "hook.sh")], {
    cwd,
    env: hookEnv(),
    stdin: new TextEncoder().encode(JSON.stringify({ cwd })),
  });

  return output(result).replaceAll(cwd, "<repo>");
}

function stopHook(flags?: string, hookRoot = rootWithoutBinary()): string {
  const cwd = repository();
  Bun.spawnSync([join(hookRoot, "hook.sh")], {
    cwd,
    env: hookEnv(flags),
    stdin: new TextEncoder().encode(JSON.stringify({ cwd })),
  });

  return readFileSync(join(cwd, "a.ts"), "utf8");
}

test("pre-commit forwards STANZA_FLAGS to stanza", () => {
  expect(output(preCommit())).toContain(" braces ");

  const findings = output(preCommit(undefined, "--no-braces"));
  expect(findings).not.toContain(" braces ");
  expect(findings).toContain(" after-multiline ");
});

test("pre-commit resolves shared ESLint packages from the repository", () => {
  const findings = output(
    preCommit({
      ".eslintrc.json": '{ "extends": ["@acme/eslint-config"] }',
      "node_modules/@acme/eslint-config/package.json":
        '{"name":"@acme/eslint-config","main":"index.json"}',
      "node_modules/@acme/eslint-config/index.json": '{ "rules": { "curly": "off" } }',
      "a.ts": readFileSync(fixture, "utf8"),
    }),
  );

  expect(findings).toContain(" braces ");
});

test("pre-commit resolves nested ESLint config from the repository", () => {
  const findings = output(
    preCommit({
      ".eslintrc.json": '{ "rules": { "curly": "error" } }',
      "nested/.eslintrc.json": '{ "rules": { "curly": "off" } }',
      "a.ts": readFileSync(fixture, "utf8"),
      "nested/b.ts": readFileSync(fixture, "utf8"),
    }),
  );

  expect(rules(findings, "nested/b.ts")).toContain("braces");
  expect(rules(findings, "a.ts")).not.toContain("braces");
  expect(rules(findings, "a.ts")).not.toHaveLength(0);
});

test("pre-commit remaps directories whose names start with two dots", () => {
  const findings = output(
    preCommit({
      ".eslintrc.json": '{ "rules": { "curly": "error" } }',
      "..sources/a.ts": readFileSync(fixture, "utf8"),
    }),
  );

  expect(rules(findings, "..sources/a.ts")).not.toContain("braces");
  expect(rules(findings, "..sources/a.ts")).not.toHaveLength(0);
});

test("pre-commit checks staged blobs instead of working tree files", () => {
  const cwd = repository();
  writeFileSync(join(cwd, "a.ts"), "export const clean = 1;\n");

  const result = Bun.spawnSync([join(rootWithoutBinary(), "git-hooks", "pre-commit")], {
    cwd,
    env: hookEnv(),
  });

  expect(rules(output(result), "a.ts")).toContain("braces");

  expect(result.exitCode).not.toBe(0);
});

test("pre-commit skips staged files marked linguist-generated", () => {
  const result = preCommit({
    ".gitattributes": "gen.ts linguist-generated\n",
    "gen.ts": readFileSync(fixture, "utf8"),
  });

  expect(output(result)).toBe("");
  expect(result.exitCode).toBe(0);
});

test("pre-commit ends a failing run with the command that fixes and restages", () => {
  const result = preCommit({ "a.ts": readFileSync(fixture, "utf8"), "b c.ts": "export {};\n" });
  const lines = new TextDecoder().decode(result.stderr).trimEnd().split("\n");

  expect(lines.at(-1)).toEndWith(" && stanza --fix -- a.ts && git --literal-pathspecs add -- a.ts");
  expect(result.exitCode).toBe(1);
});

test("--check --staged reports names starting with a dash or holding a newline", () => {
  const text = readFileSync(fixture, "utf8");
  const cwd = repository({ "-x.ts": text, "a\nb.ts": text });
  const result = Bun.spawnSync(
    [process.execPath, "run", join(root, "src", "cli.ts"), "--check", "--staged"],
    { cwd, env: hookEnv() },
  );

  const findings = output(result);
  expect(findings).toMatch(/^-x\.ts:\d+:\d+ braces /m);
  expect(findings).toMatch(/^a\nb\.ts:\d+:\d+ braces /m);
  expect(new TextDecoder().decode(result.stderr).trimEnd()).toBe(
    `fix and restage with: cd ${realpathSync(cwd)} && stanza --fix -- -x.ts 'a\nb.ts' && git --literal-pathspecs add -- -x.ts 'a\nb.ts'`,
  );

  expect(result.exitCode).toBe(1);
});

function stagedCheck(cwd: string): ReturnType<typeof Bun.spawnSync> {
  return Bun.spawnSync(
    [process.execPath, "run", join(root, "src", "cli.ts"), "--check", "--staged"],
    { cwd, env: hookEnv() },
  );
}

test("--check --staged reads filtered blobs through their smudge filter", () => {
  const cwd = mkdtempSync(join(tmpdir(), "stanza-hooks-repo-"));
  const git = (...args: string[]) => Bun.spawnSync(["git", ...args], { cwd });
  git("init", "-q");
  git("config", "filter.rot.clean", "tr a-zA-Z n-za-mN-ZA-M");
  git("config", "filter.rot.smudge", "tr a-zA-Z n-za-mN-ZA-M");

  writeFileSync(join(cwd, ".gitattributes"), "*.ts filter=rot\n");
  writeFileSync(join(cwd, "a.ts"), readFileSync(fixture, "utf8"));
  git("add", ".");

  const findings = output(stagedCheck(cwd));
  expect(rules(findings, "a.ts")).toContain("braces");
  expect(rules(findings, "a.ts")).not.toContain("parse");

  const shim = mkdtempSync(join(tmpdir(), "stanza-old-git-"));
  writeFileSync(
    join(shim, "git"),
    `#!/bin/sh\ncase "$*" in *--attr-source*) echo "unknown option" >&2; exit 129;; esac\nexec "${Bun.which("git")}" "$@"\n`,
  );

  chmodSync(join(shim, "git"), 0o755);

  const older = Bun.spawnSync(
    [process.execPath, "run", join(root, "src", "cli.ts"), "--check", "--staged"],
    { cwd, env: { ...hookEnv(), PATH: `${shim}:${process.env.PATH}` } },
  );

  expect(output(older)).toBe(findings);
});

test("--check --staged does not restage a file with unstaged changes", () => {
  const cwd = repository();
  writeFileSync(join(cwd, "a.ts"), `${readFileSync(fixture, "utf8")}console.log("wip");\n`);

  const stderr = new TextDecoder().decode(stagedCheck(cwd).stderr);
  expect(stderr).toBe(
    "these also have unstaged changes, fix them with --fix and restage by hand: a.ts\n",
  );
});

test("the Stop hook forwards STANZA_FLAGS to stanza", () => {
  const original = readFileSync(fixture, "utf8");
  expect(braces(stopHook())).toBeLessThan(braces(original));
  expect(braces(stopHook("--no-braces"))).toBe(braces(original));
});

test("pre-commit runs source over a stale binary when bun is on PATH", () => {
  const stale = rootWithBinary('touch "$0.ran"; exit 99');
  const source = preCommit();
  const result = preCommit(undefined, undefined, stale);

  expect(output(source)).toContain(" braces ");
  expect(output(result)).toBe(output(source));
  expect(result.exitCode).toBe(source.exitCode);
  expect(existsSync(join(stale, "bin", "stanza.ran"))).toBe(false);
});

test("the Stop hook runs source over a stale binary when bun is on PATH", () => {
  const stale = rootWithBinary('touch "$0.ran"; exit 99');
  const original = readFileSync(fixture, "utf8");
  const fixed = stopHook();
  expect(braces(fixed)).toBeLessThan(braces(original));
  expect(stopHook(undefined, stale)).toBe(fixed);

  const unparseable = { "a.ts": "export const = ;\n" };
  const blocked = stopHookOutput(unparseable);
  expect(blocked).toContain('"decision":"block"');
  expect(stopHookOutput(unparseable, stale)).toBe(blocked);

  expect(existsSync(join(stale, "bin", "stanza.ran"))).toBe(false);
}, 15_000);

test("pre-commit runs bin/stanza when bun is missing", () => {
  const binary = rootWithBinary(
    `exec "${process.execPath}" run "${join(root, "src", "cli.ts")}" "$@"`,
  );

  const result = preCommit(undefined, undefined, binary, pathWithoutBun());

  expect(rules(output(result), "a.ts")).toContain("braces");
  expect(result.exitCode).not.toBe(0);
});

test("both hooks run bin/stanza when source dependencies are missing", () => {
  const binary = rootWithBinary(
    `touch "$0.ran"; exec "${process.execPath}" run "${join(root, "src", "cli.ts")}" "$@"`,
  );

  rmSync(join(binary, "node_modules"));

  expect(rules(output(preCommit(undefined, undefined, binary)), "a.ts")).toContain("braces");
  expect(braces(stopHook(undefined, binary))).toBeLessThan(braces(readFileSync(fixture, "utf8")));
  expect(existsSync(join(binary, "bin", "stanza.ran"))).toBe(true);
});

test("pre-commit asks for a rebuild when bin/stanza predates --staged", () => {
  const stale = rootWithBinary(
    'echo "Usage: stanza (--fix | --check) [--changed | <paths...>]" >&2; exit 2',
  );

  const result = preCommit(undefined, undefined, stale, pathWithoutBun());

  expect(new TextDecoder().decode(result.stderr)).toBe(
    `stanza pre-commit hook: ${stale}/bin/stanza predates --staged, so nothing was checked; run 'bun run build' in ${stale}\n`,
  );

  expect(result.exitCode).toBe(0);
});

test("pre-commit blocks on a bad STANZA_FLAGS instead of reading it as a stale binary", () => {
  const result = preCommit(undefined, "--changed");
  expect(new TextDecoder().decode(result.stderr)).toStartWith("Usage: stanza");
  expect(result.exitCode).toBe(2);
});

test("--check --staged smudges with the filter the index names", () => {
  const cwd = mkdtempSync(join(tmpdir(), "stanza-hooks-repo-"));
  const git = (...args: string[]) => Bun.spawnSync(["git", ...args], { cwd });
  git("init", "-q");
  git("config", "filter.rot.clean", "tr a-zA-Z n-za-mN-ZA-M");
  git("config", "filter.rot.smudge", "tr a-zA-Z n-za-mN-ZA-M");

  writeFileSync(join(cwd, ".gitattributes"), "*.ts filter=rot\n");
  writeFileSync(join(cwd, "a.ts"), readFileSync(fixture, "utf8"));
  git("add", ".");
  writeFileSync(join(cwd, ".gitattributes"), "");

  const findings = output(stagedCheck(cwd));
  expect(rules(findings, "a.ts")).toContain("braces");
  expect(rules(findings, "a.ts")).not.toContain("parse");
});

test("--check --staged reads attributes from the index", () => {
  const cwd = repository();
  writeFileSync(join(cwd, ".gitattributes"), "a.ts linguist-generated\n");
  expect(rules(output(stagedCheck(cwd)), "a.ts")).toContain("braces");
});

test("pre-commit names bin/stanza and bun when neither is available", () => {
  const result = preCommit(undefined, undefined, rootWithoutBinary(), pathWithoutBun());
  const message = new TextDecoder().decode(result.stderr);

  expect(message).toContain("bin/stanza");
  expect(message).toContain("nor bun");
  expect(result.exitCode).toBe(1);
});

function hookCommand(
  input: string,
  args: string[] = [],
  env: Record<string, string | undefined> = { ...hookEnv(), FORCE_COLOR: "3" },
): ReturnType<typeof Bun.spawnSync> {
  return Bun.spawnSync([process.execPath, "run", join(root, "src", "cli.ts"), "hook", ...args], {
    cwd: root,
    env,
    stdin: new TextEncoder().encode(input),
  });
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
  const path = join(mkdtempSync(join(tmpdir(), "stanza-hook-transcript-")), "stop.jsonl");
  const text = lines.map((line) => (typeof line === "string" ? line : JSON.stringify(line)));
  writeFileSync(path, text.join("\n"));

  return path;
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

function expectSilent(result: ReturnType<typeof Bun.spawnSync>): void {
  expect(output(result)).toBe("");
  expect(new TextDecoder().decode(result.stderr)).toBe("");
  expect(result.exitCode).toBe(0);
}

test("stanza hook fixes only the files the transcript says the agent wrote", () => {
  const cwd = agentAndHuman();
  const path = transcript(userTurn, ...toolCalls(["Edit", join(cwd, "agent.ts")]));
  const result = hookCommand(JSON.stringify({ cwd, transcript_path: path }));

  expect(readFileSync(join(cwd, "human.ts"))).toEqual(readFileSync(fixture));
  expect(readFileSync(join(cwd, "agent.ts"))).toEqual(readFileSync(bodiesAfter));
  expectSilent(result);
});

test("stanza hook fixes a file whose edit result the transcript does not hold yet", () => {
  const cwd = agentAndHuman();
  const [edit] = toolCalls(["Edit", join(cwd, "agent.ts")]);
  const result = hookCommand(JSON.stringify({ cwd, transcript_path: transcript(userTurn, edit) }));

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
  const outside = join(mkdtempSync(join(tmpdir(), "stanza-hook-outside-file-")), "outside.ts");
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
  const outside = mkdtempSync(join(tmpdir(), "stanza-hook-outside-"));
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
    expect(new TextDecoder().decode(result.stderr)).toContain("stanza hook [--no-braces]");
    expect(new TextDecoder().decode(result.stderr)).toStartWith(
      `stanza hook: unexpected argument ${args.at(-1)}\n`,
    );

    expect(result.exitCode).toBe(1);
  }
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

test("the Stop hook runs bin/stanza without bun on PATH", () => {
  const binary = rootWithBinary(
    `exec "${process.execPath}" run "${join(root, "src", "cli.ts")}" "$@"`,
  );

  const cwd = repository(wallAndBodies());
  const result = Bun.spawnSync([join(binary, "hook.sh")], {
    cwd: root,
    env: { ...hookEnv(), PATH: pathWithoutBun() },
    stdin: new TextEncoder().encode(JSON.stringify({ cwd })),
  });

  expect(result.exitCode).toBe(0);
  expect(new TextDecoder().decode(result.stderr)).toBe("");
  expect(JSON.parse(output(result)).reason).toContain("wall.ts:2:3 wall ");
});

test("the Stop hook names bin/stanza and bun when neither is available", () => {
  const hookRoot = rootWithoutBinary();
  const result = Bun.spawnSync([join(hookRoot, "hook.sh")], {
    cwd: root,
    env: { ...hookEnv(), PATH: pathWithoutBun() },
    stdin: new TextEncoder().encode("{}"),
  });

  expect(output(result)).toBe("");
  expect(result.exitCode).toBe(1);
  expect(new TextDecoder().decode(result.stderr)).toBe(
    `stanza Stop hook: neither ${hookRoot}/bin/stanza nor bun with installed dependencies is available; run 'bun install' or 'bun run build' in ${hookRoot}\n`,
  );

  const disabled = Bun.spawnSync([join(hookRoot, "hook.sh")], {
    cwd: root,
    env: { ...hookEnv(), AGENT_HOOKS: "0", PATH: pathWithoutBun() },
    stdin: new TextEncoder().encode("{}"),
  });

  expect(output(disabled)).toBe("");
  expect(new TextDecoder().decode(disabled.stderr)).toBe("");
  expect(disabled.exitCode).toBe(0);
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

test("stanza hook asks to reread a file it rewrote that still has a finding", () => {
  const wall = readFileSync(join(root, "tests", "fixtures", "wall", "wall.before.ts"), "utf8");
  const cwd = repository({ "mixed.ts": `${wall}\n${readFileSync(fixture, "utf8")}` });
  const result = hookCommand(JSON.stringify({ cwd }));
  const reason = JSON.parse(output(result)).reason;

  expect(reason).toContain("mixed.ts:2:3 wall ");
  expect(reason).toEndWith("stanza rewrote mixed.ts, so read it again before editing.");
});

test("the Stop hook turns a stale binary's exit 2 into a non-blocking exit 1", () => {
  const stale = rootWithBinary("echo usage >&2; exit 2");
  const result = Bun.spawnSync([join(stale, "hook.sh")], {
    cwd: root,
    env: { ...hookEnv(), PATH: pathWithoutBun() },
    stdin: new TextEncoder().encode("{}"),
  });

  expect(result.exitCode).toBe(1);
  expect(new TextDecoder().decode(result.stderr)).toBe("usage\n");
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
