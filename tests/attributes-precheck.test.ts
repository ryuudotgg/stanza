import { describe, expect, test } from "bun:test";
import { mkdirSync, readFileSync, rmSync, symlinkSync, writeFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { cli, gitBinary, scratch } from "./support.ts";

const violating = "function value() { if (true) { return 1; } return 2; }\n";

function write(path: string, text: string): void {
  mkdirSync(dirname(path), { recursive: true });
  writeFileSync(path, text);
}

function fixture(omitAttributeMetadata = false) {
  const root = scratch("attributes");
  const home = join(root, "home");
  const cwd = join(root, "repo");
  const log = join(root, "git.log");
  const bin = join(root, "bin");

  const env: NodeJS.ProcessEnv = {
    ...process.env,
    HOME: home,
    XDG_CONFIG_HOME: join(home, ".config"),
  };

  for (const name of Object.keys(env)) if (name.startsWith("GIT_")) delete env[name];

  const executable = Bun.which(gitBinary);
  if (!executable) throw new Error("Git executable not found");

  mkdirSync(cwd);
  mkdirSync(home);
  const metadataFilter = omitAttributeMetadata
    ? `if [ "$3" = var ] && [ "$4" = -l ]; then\noutput=$('${executable}' "$@")\nstatus=$?\nprintf '%s\\n' "$output" | sed '/^GIT_ATTR_/d'\nexit "$status"\nfi\n`
    : "";

  write(
    join(bin, "git"),
    `#!/bin/sh\nprintf '%s\\n' "$*" >> '${log}'\n${metadataFilter}exec '${executable}' "$@"\n`,
  );

  Bun.spawnSync(["chmod", "+x", join(bin, "git")]);
  env.PATH = `${bin}:${env.PATH}`;

  const git = (...args: string[]) => {
    const result = Bun.spawnSync([executable, "-C", cwd, ...args], { env });
    expect(result.exitCode).toBe(0);
  };

  git("init", "-q");
  git("config", "user.name", "Test User");
  git("config", "user.email", "test@example.com");
  write(join(cwd, "source/file.ts"), "export const value = 1;\n");

  const metadata = Bun.spawnSync(["git", "-C", cwd, "var", "-l"], { env });
  expect(metadata.exitCode).toBe(0);
  const supportsPrecheck = /^GIT_ATTR_SYSTEM=.+\nGIT_ATTR_GLOBAL=.+$/m.test(
    new TextDecoder().decode(metadata.stdout),
  );

  const invoke = (args: string[], stdin?: string) => {
    writeFileSync(log, "");
    const result = Bun.spawnSync([process.execPath, cli, ...args], {
      cwd,
      env,
      stdin: stdin === undefined ? undefined : Buffer.from(stdin),
      timeout: 5000,
    });

    const calls = readFileSync(log, "utf8")
      .split("\n")
      .filter((line) => line !== "");

    const spawns = calls.filter((line) => line.includes("check-attr"));
    return { result, calls, spawns };
  };

  const run = (...args: string[]) => invoke(args);
  const hook = (input: Record<string, unknown>) => invoke(["hook"], JSON.stringify(input));

  const generated = (file: string) => {
    const result = Bun.spawnSync(
      [executable, "-C", cwd, "check-attr", "linguist-generated", "--", file],
      { env },
    );

    expect(result.exitCode).toBe(0);
    return new TextDecoder().decode(result.stdout).trim().split(": ").at(-1);
  };

  return { root, home, cwd, env, git, run, hook, generated, supportsPrecheck };
}

function attributesBranch(): ReturnType<typeof fixture> {
  const repository = fixture();
  const { cwd, git } = repository;
  git("add", "source/file.ts");
  git("commit", "-q", "-m", "init");

  git("switch", "-q", "-c", "attrs");
  write(join(cwd, ".gitattributes"), "*.ts linguist-generated\n");
  git("add", ".gitattributes");
  git("commit", "-q", "-m", "attrs");
  git("switch", "-q", "-");

  write(join(cwd, "source/file.ts"), violating);
  return repository;
}

function expectUntouched(
  { cwd, run }: ReturnType<typeof fixture>,
  file: string,
  ...modes: string[][]
): void {
  for (const args of modes) {
    const { result, spawns } = run(...args);
    expect(result.exitCode).toBe(0);
    expect(new TextDecoder().decode(result.stdout)).toBe("");
    expect(spawns.length).toBeGreaterThan(0);
  }

  expect(readFileSync(join(cwd, file), "utf8")).toBe(violating);
}

describe("attribute spawn precheck", () => {
  test("skips check-attr with no reachable source, including an ordinary index", () => {
    const { cwd, git, run, supportsPrecheck } = fixture();
    for (const indexed of [false, true]) {
      if (indexed) git("add", "source/file.ts");

      const { result, spawns } = run("--check", "source/file.ts");
      expect(result.exitCode).toBe(0);
      expect(spawns.length).toBe(supportsPrecheck ? 0 : 1);
    }

    write(join(cwd, ".gitattributes"), "*.ts text\n");
    expect(run("--check", ".").spawns.length).toBe(supportsPrecheck ? 0 : 1);
  });

  for (const source of [
    "directory",
    "root",
    "untracked",
    "info",
    "configured",
    "newline",
    "global",
    "include",
    "includeIf",
    "macro",
    "index",
  ])
    test(`preserves generated selection from ${source}`, () => {
      const { root, home, cwd, git, run } = fixture();
      const attributes = "*.ts linguist-generated\n";
      if (source === "directory") write(join(cwd, "source/.gitattributes"), attributes);
      if (["root", "untracked", "index"].includes(source))
        write(join(cwd, ".gitattributes"), attributes);

      if (source === "info") write(join(cwd, ".git/info/attributes"), attributes);
      if (["configured", "newline"].includes(source)) {
        const path = join(root, source === "newline" ? "attributes\nextra" : "attributes");
        write(path, attributes);
        git("config", "core.attributesFile", path);
      }

      if (source === "global") write(join(home, ".config/git/attributes"), attributes);
      if (["include", "includeIf"].includes(source)) {
        write(join(root, "attributes"), attributes);
        write(join(root, "included"), `[core]\nattributesFile = ${join(root, "attributes")}\n`);
        const section = source === "include" ? "include" : `includeIf "gitdir:${cwd}/"`;
        write(join(home, ".gitconfig"), `[${section}]\npath = ${join(root, "included")}\n`);
      }

      if (source === "macro")
        write(join(cwd, ".gitattributes"), "[attr]generated linguist-generated\n*.ts generated\n");

      git("add", "source/file.ts");
      if (["root", "directory", "macro", "index"].includes(source)) git("add", ".");
      if (source === "index") rmSync(join(cwd, ".gitattributes"));

      write(
        join(cwd, "source/file.ts"),
        "function value() { if (true) { return 1; } return 2; }\n",
      );

      git("add", "source/file.ts");

      for (const args of [
        ["--check", "source/file.ts"],
        ["--check", "."],
        ["--check", "--staged"],
        ["--fix", "source/file.ts"],
      ]) {
        const { result, spawns } = run(...args);
        const stagedUntracked = source === "untracked" && args.includes("--staged");
        expect(result.exitCode).toBe(stagedUntracked ? 1 : 0);
        if (!stagedUntracked) expect(new TextDecoder().decode(result.stdout)).toBe("");
        expect(readFileSync(join(cwd, "source/file.ts"), "utf8")).toBe(
          "function value() { if (true) { return 1; } return 2; }\n",
        );

        expect(spawns.length).toBeGreaterThan(0);
      }
    });

  test("unrelated macros, unreadable sources and dangling links conservatively spawn", () => {
    const { cwd, run } = fixture();
    write(join(cwd, ".gitattributes"), "[attr]custom text\n*.ts custom\n");
    expect(run("--check", "source/file.ts").spawns.length).toBeGreaterThan(0);

    rmSync(join(cwd, ".gitattributes"));
    mkdirSync(join(cwd, ".gitattributes"));
    expect(run("--check", "source/file.ts").spawns.length).toBeGreaterThan(0);

    rmSync(join(cwd, ".gitattributes"), { recursive: true });
    symlinkSync("missing-attributes", join(cwd, ".gitattributes"));
    expect(run("--check", "source/file.ts").spawns.length).toBeGreaterThan(0);
  });

  test("Git environment overrides and version four indexes conservatively spawn", () => {
    const { env, git, run } = fixture();
    env.GIT_ATTR_NOSYSTEM = "1";
    expect(run("--check", "source/file.ts").spawns.length).toBeGreaterThan(0);

    delete env.GIT_ATTR_NOSYSTEM;
    env.GIT_CONFIG_COUNT = "1";
    env.GIT_CONFIG_KEY_0 = "core.attributesFile";
    env.GIT_CONFIG_VALUE_0 = "/missing-attributes";
    expect(run("--check", "source/file.ts").spawns.length).toBeGreaterThan(0);

    delete env.GIT_CONFIG_COUNT;
    delete env.GIT_CONFIG_KEY_0;
    delete env.GIT_CONFIG_VALUE_0;

    git("add", "source/file.ts");
    git("update-index", "--index-version", "4");
    expect(run("--check", "source/file.ts").spawns.length).toBeGreaterThan(0);
  });

  test("split indexes conservatively spawn", () => {
    const { git, run } = fixture();
    git("add", "source/file.ts");
    git("update-index", "--split-index");
    expect(run("--check", "source/file.ts").spawns.length).toBeGreaterThan(0);
  });

  test("a symlink to a FIFO cannot block the precheck", () => {
    const { root, cwd, run } = fixture();
    const fifo = join(root, "attributes.fifo");
    expect(Bun.spawnSync(["mkfifo", fifo]).exitCode).toBe(0);
    symlinkSync(fifo, join(cwd, ".gitattributes"));

    const { result, spawns } = run("--check", "source/file.ts");
    expect(result.exitCode).toBe(0);
    expect(spawns.length).toBeGreaterThan(0);
  });

  test("a changed source is reread on the next invocation", () => {
    const { cwd, run, supportsPrecheck } = fixture();
    expect(run("--check", "source/file.ts").spawns.length).toBe(supportsPrecheck ? 0 : 1);

    write(join(cwd, "source/.gitattributes"), "*.ts linguist-generated\n");
    expect(run("--check", "source/file.ts").spawns.length).toBeGreaterThan(0);
  });

  test("attr.tree naming a tree that marks files generated keeps path runs off them", () => {
    const repository = attributesBranch();
    repository.git("config", "attr.tree", "attrs");
    if (repository.generated("source/file.ts") === "unspecified") return;

    expectUntouched(
      repository,
      "source/file.ts",
      ["--check", "source/file.ts"],
      ["--fix", "source/file.ts"],
    );
  }, 60_000);

  test("attr.tree naming a tree that marks files generated keeps --changed off them", () => {
    const repository = attributesBranch();
    repository.git("config", "attr.tree", "attrs");
    if (repository.generated("source/file.ts") === "unspecified") return;

    expectUntouched(repository, "source/file.ts", ["--check", "--changed"], ["--fix", "--changed"]);
  }, 60_000);

  test("attr.tree naming a tree that marks files generated keeps the edit hook off them", () => {
    const { cwd, git, hook, generated } = attributesBranch();
    git("config", "attr.tree", "attrs");
    if (generated("source/file.ts") === "unspecified") return;

    const file_path = join(cwd, "source/file.ts");
    const { result, spawns } = hook({
      cwd,
      hook_event_name: "PostToolUse",
      tool_name: "Edit",
      tool_input: { file_path },
      tool_response: { filePath: file_path },
    });

    const output =
      new TextDecoder().decode(result.stdout) + new TextDecoder().decode(result.stderr);

    expect(result.exitCode).toBe(0);
    expect(output).not.toContain("stanza formatted");
    expect(spawns.length).toBeGreaterThan(0);
    expect(readFileSync(file_path, "utf8")).toBe(violating);
  }, 60_000);

  test("GIT_ATTR_SOURCE naming a tree that marks files generated keeps path runs off them", () => {
    const repository = attributesBranch();
    repository.env.GIT_ATTR_SOURCE = "attrs";
    if (repository.generated("source/file.ts") === "unspecified") return;

    expectUntouched(
      repository,
      "source/file.ts",
      ["--check", "source/file.ts"],
      ["--fix", "source/file.ts"],
    );
  }, 60_000);

  test("core.attributesFile through a symlink then .. keeps generated files unformatted", () => {
    const repository = fixture();
    const { home, cwd, git } = repository;

    mkdirSync(join(home, "deep/inner"), { recursive: true });
    symlinkSync(join(home, "deep/inner"), join(home, "link"));
    write(join(home, "deep/attributes"), "*.ts linguist-generated\n");
    git("config", "--global", "core.attributesFile", join(home, "link") + "/../attributes");

    write(join(cwd, "source/file.ts"), violating);
    expect(repository.generated("source/file.ts")).toBe("set");

    expectUntouched(
      repository,
      "source/file.ts",
      ["--check", "source/file.ts"],
      ["--fix", "source/file.ts"],
    );
  }, 60_000);

  test("a .gitattributes between the file and the root keeps generated files unformatted", () => {
    const repository = fixture();
    const { cwd } = repository;
    write(join(cwd, "source/.gitattributes"), "*.ts linguist-generated\n");
    write(join(cwd, "source/nested/file.ts"), violating);
    expect(repository.generated("source/nested/file.ts")).toBe("set");

    expectUntouched(
      repository,
      "source/nested/file.ts",
      ["--check", "source/nested/file.ts"],
      ["--fix", "source/nested/file.ts"],
    );
  }, 60_000);

  test("inert Git variables keep a plain repository at the single git var spawn", () => {
    const { env, git, run, supportsPrecheck } = fixture();
    git("add", "source/file.ts");
    git("commit", "-q", "-m", "init");

    for (const inert of [{}, { GIT_EDITOR: "true", GIT_PAGER: "cat", GIT_TERMINAL_PROMPT: "0" }]) {
      Object.assign(env, inert);
      const { result, calls } = run("--check", "source/file.ts");
      expect(result.exitCode).toBe(0);
      expect(calls.length).toBe(supportsPrecheck ? 1 : 2);
      expect(calls[0]).toContain("var -l");
    }
  }, 60_000);

  const attributeVariables: [string, (root: string) => string][] = [
    ["GIT_ATTR_SOURCE", () => "HEAD"],
    ["GIT_ATTR_NOSYSTEM", () => "1"],
    ["GIT_DIR", (root) => join(root, "repo/.git")],
    ["GIT_WORK_TREE", () => "."],
    ["GIT_INDEX_FILE", () => ".git/index"],
    ["GIT_CONFIG_GLOBAL", (root) => join(root, "home/.gitconfig")],
    ["GIT_CONFIG_COUNT", () => "0"],
    ["GIT_EDITOR", () => "true\nfalse"],
  ];

  for (const [name, value] of attributeVariables)
    test(`${name}=${JSON.stringify(value("<root>"))} spawns check-attr`, () => {
      const { root, env, git, run } = fixture();
      git("add", "source/file.ts");
      git("commit", "-q", "-m", "init");

      env[name] = value(root);
      expect(run("--check", "source/file.ts").spawns.length).toBe(1);
    }, 60_000);

  test("a multi line config value cannot forge GIT_ATTR_GLOBAL", () => {
    const repository = fixture();
    const { root, home, cwd, git } = repository;
    write(join(home, ".config/git/attributes"), "*.ts linguist-generated\n");
    git("config", "forged.value", `x\nGIT_ATTR_GLOBAL=${join(root, "missing")}`);

    write(join(cwd, "source/file.ts"), violating);
    expect(repository.generated("source/file.ts")).toBe("set");

    expectUntouched(
      repository,
      "source/file.ts",
      ["--check", "source/file.ts"],
      ["--fix", "source/file.ts"],
    );
  }, 60_000);

  for (const [label, suffix] of [
    ["a bare forged variable", "\nGIT_CONFIG_SYSTEM"],
    ["a forged assignment", "\nGIT_CONFIG_SYSTEM=x"],
    ["a trailing newline", "\n"],
  ])
    test(`a multi line core.attributesFile ending in ${label} keeps generated files unformatted`, () => {
      const repository = fixture();
      const { home, cwd, git } = repository;
      const path = join(home, "attributes") + suffix;

      write(path, "*.ts linguist-generated\n");
      git("config", "--global", "core.attributesFile", path);

      write(join(cwd, "source/file.ts"), violating);
      expect(repository.generated("source/file.ts")).toBe("set");

      expectUntouched(
        repository,
        "source/file.ts",
        ["--check", "source/file.ts"],
        ["--fix", "source/file.ts"],
      );
    }, 60_000);

  test("attr.tree after a multi line config value still spawns", () => {
    const repository = attributesBranch();
    repository.git("config", "forged.value", "x\nGIT_EDITOR=true");
    repository.git("config", "attr.tree", "attrs");
    if (repository.generated("source/file.ts") === "unspecified") return;

    expectUntouched(
      repository,
      "source/file.ts",
      ["--check", "source/file.ts"],
      ["--fix", "source/file.ts"],
    );
  }, 60_000);

  test("missing Git attribute metadata preserves the Git fallback", () => {
    const { cwd, run, supportsPrecheck } = fixture(true);
    expect(supportsPrecheck).toBe(false);

    const ordinary = run("--check", "source/file.ts");
    expect(ordinary.result.exitCode).toBe(0);
    expect(ordinary.spawns.length).toBe(1);

    write(join(cwd, ".gitattributes"), "*.ts linguist-generated\n");
    write(join(cwd, "source/file.ts"), "function value() { if (true) { return 1; } return 2; }\n");

    const generated = run("--check", "source/file.ts");
    expect(generated.result.exitCode).toBe(0);
    expect(new TextDecoder().decode(generated.result.stdout)).toBe("");
    expect(generated.spawns.length).toBe(1);
  });
});
