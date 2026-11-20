import { describe, expect, test } from "bun:test";
import {
  chmodSync,
  mkdirSync,
  readFileSync,
  realpathSync,
  rmSync,
  symlinkSync,
  writeFileSync,
} from "node:fs";
import { join, resolve } from "node:path";
import {
  byteOrder,
  collectChanged,
  colorless,
  gitBinary,
  run,
  scratch,
  scratchGitRepository,
  stanzaBinary,
} from "./support.ts";

function write(path: string, text = "export {};\n"): void {
  mkdirSync(resolve(path, ".."), { recursive: true });
  writeFileSync(path, text);
}

function git(cwd: string, ...args: string[]): void {
  const result = Bun.spawnSync([gitBinary, "-C", cwd, ...args]);
  if (result.exitCode !== 0) throw new Error(new TextDecoder().decode(result.stderr));
}

function repository(): string {
  const cwd = scratchGitRepository();

  git(cwd, "config", "user.email", "test@example.com");
  git(cwd, "config", "user.name", "Test User");
  git(cwd, "config", "commit.gpgsign", "false");

  return cwd;
}

test("a vanished selection directory leaves its sibling in scope", () => {
  const braced = "if (ready) { run(); }\n";
  const cwd = repository();
  write(join(cwd, "gone/a.ts"), braced);
  write(join(cwd, "good.ts"), braced);

  const bin = scratch("renaming-git");
  const executable = Bun.which(gitBinary) ?? gitBinary;
  writeFileSync(
    join(bin, "git"),
    `#!/bin/sh\n'${executable}' "$@"\nstatus=$?\ncase " $* " in *" ls-files "*) [ -d '${cwd}/gone' ] && mv '${cwd}/gone' '${cwd}/vanished';; esac\nexit $status\n`,
    { mode: 0o755 },
  );

  const env = { ...process.env, PATH: `${bin}:${process.env.PATH ?? ""}` };
  const result = run({ cwd, env }, "--check", "gone", "good.ts");
  const failures = result.stderr.split("\n").filter((line) => line.startsWith("stanza: "));

  expect(result.code).toBe(2);
  expect(result.stdout).toBe(
    "good.ts:1:12: braces: remove the braces around this single statement body\n",
  );

  expect(failures).toEqual(["stanza: cannot read gone: no such file or directory"]);
});

test.skipIf(process.getuid?.() === 0)(
  "an unsearchable ignored folder of generated files keeps the skipped notice",
  () => {
    const cwd = repository();
    write(join(cwd, ".gitignore"), "locked/\n");
    write(join(cwd, "types.d.ts"), "export type A = 1;\n");
    write(join(cwd, "locked/schema.gen.ts"));

    git(cwd, "add", "-f", ".");
    git(cwd, "commit", "-qm", "initial");

    chmodSync(join(cwd, "locked"), 0o000);
    try {
      expect(run({ cwd }, "--check", ".")).toEqual({
        code: 0,
        stdout: "",
        stderr: "stanza: nothing to format in ., its files are always skipped\n",
      });
    } finally {
      chmodSync(join(cwd, "locked"), 0o755);
    }
  },
);

describe("a path run that formats nothing", () => {
  const braced = "if (a) {\n  b();\n}\n";

  test("gives each argument its own reason", () => {
    const cwd = repository();
    write(join(cwd, ".gitignore"), "ignored/\n");
    write(join(cwd, "a.gen.ts"), braced);
    write(join(cwd, "ignored/i.ts"), braced);

    const notice = (reason: string) => ({ code: 0, stdout: "", stderr: `stanza: ${reason}\n` });
    const generated = "nothing to format in a.gen.ts, generated files are skipped";
    const ignored = "nothing to format in ignored, git ignores its files";

    expect(run({ cwd }, "--check", "a.gen.ts", "ignored")).toEqual({
      code: 0,
      stdout: "",
      stderr: `stanza: ${generated}\nstanza: ${ignored}\n`,
    });

    expect(run({ cwd }, "--check", "ignored")).toEqual(notice(ignored));
    expect(run({ cwd }, "--check", "a.gen.ts")).toEqual(notice(generated));
  });

  test("keeps a header generated file's reason to its own argument", () => {
    const cwd = repository();
    write(join(cwd, ".gitignore"), "ignored/\n");
    write(join(cwd, "a.ts"), `// @generated\n${braced}`);
    write(join(cwd, "ignored/i.ts"), braced);

    expect(run({ cwd }, "--check", "a.ts", "ignored")).toEqual({
      code: 0,
      stdout: "",
      stderr:
        "stanza: nothing to format in a.ts, generated files are skipped\nstanza: nothing to format in ignored, git ignores its files\n",
    });

    write(join(cwd, ".gitattributes"), "opted.ts -stanza\n");
    write(join(cwd, "opted.ts"), braced);
    expect(run({ cwd }, "--check", "a.ts", "opted.ts").stderr).toBe(
      "stanza: nothing to format in a.ts, generated files are skipped\nstanza: nothing to format in opted.ts, the stanza attribute opts its files out\n",
    );
  });

  test("names an untracked nested repository to pass by path", () => {
    const cwd = repository();
    mkdirSync(join(cwd, "inner"));
    git(join(cwd, "inner"), "init", "-q");
    write(join(cwd, "inner/a.ts"), braced);

    expect(run({ cwd }, "--check", ".")).toEqual({
      code: 0,
      stdout: "",
      stderr: "stanza: skipped nested git repository inner, pass its path to format its files\n",
    });

    expect(run({ cwd }, "--check", "inner")).toEqual({
      code: 1,
      stdout: "inner/a.ts:1:8: braces: remove the braces around this single statement body\n",
      stderr: "1 finding in 1 file, fixable with --fix\n",
    });
  });

  test("keeps a named nested repository out of the outer argument's reason", () => {
    const cwd = repository();
    write(join(cwd, "types.d.ts"), "export type A = 1;\n");

    mkdirSync(join(cwd, "inner"));
    git(join(cwd, "inner"), "init", "-q");
    write(join(cwd, "inner/.gitattributes"), "a.ts linguist-generated\n");
    write(join(cwd, "inner/a.ts"), braced);

    const skipped = "stanza: nothing to format in ., its files are always skipped\n";
    const nested =
      "stanza: skipped nested git repository inner, pass its path to format its files\n";

    expect(run({ cwd }, "--check", ".", ".").stderr).toBe(
      "stanza: nothing to format in ., ., its files are always skipped\n" + nested,
    );

    expect(run({ cwd }, "--check", ".", "inner").stderr).toBe(
      skipped + "stanza: nothing to format in inner, generated files are skipped\n",
    );
  });

  test("does not count a generated name reached through a symlinked folder", () => {
    const cwd = repository();
    write(join(cwd, "dir/a.gen.ts"), braced);
    git(cwd, "add", ".");
    git(cwd, "commit", "-qm", "initial");

    const elsewhere = scratch("elsewhere");
    write(join(elsewhere, "a.gen.ts"), braced);
    rmSync(join(cwd, "dir"), { recursive: true });
    symlinkSync(elsewhere, join(cwd, "dir"));

    expect(run({ cwd }, "--check", ".").stderr).toBe(
      "stanza: no TypeScript or JavaScript files under .\n",
    );
  });
});

test("collects modified and untracked files", () => {
  const cwd = repository();
  write(join(cwd, "tracked.ts"));
  git(cwd, "add", "tracked.ts");
  git(cwd, "commit", "-qm", "initial");

  write(join(cwd, "tracked.ts"), "export const changed = true;\n");
  write(join(cwd, "untracked.ts"));

  expect(collectChanged(cwd)).toEqual({
    changedLines: new Map(),
    files: [join(realpathSync(cwd), "tracked.ts"), join(realpathSync(cwd), "untracked.ts")],
    errors: [],
    warnings: [],
    submodules: [],
  });
});

test("collects staged files before the first commit", () => {
  const cwd = repository();
  write(join(cwd, "staged.ts"));
  git(cwd, "add", "staged.ts");
  expect(collectChanged(cwd)).toEqual({
    changedLines: new Map(),
    files: [join(realpathSync(cwd), "staged.ts")],
    errors: [],
    warnings: [],
    submodules: [],
  });
});

const multiline = "const a = {\n  b: 1,\n};\nexport const c = a;\n";

test("a tracked name that begins with a byte order mark is selected under its own name", () => {
  const cwd = scratchGitRepository({ files: { "\u{feff}lead.ts": multiline }, staged: true });
  const env = colorless();
  const result = Bun.spawnSync([stanzaBinary(), "--check", "."], { cwd, env });
  const decoder = new TextDecoder("utf-8", { ignoreBOM: true });

  expect(result.exitCode).toBe(1);
  expect(decoder.decode(result.stderr)).toBe("1 finding in 1 file, fixable with --fix\n");
  expect(decoder.decode(result.stdout)).toBe(
    "\u{feff}lead.ts:4:1: after-multiline: add a blank line after the multi-line statement above\n",
  );
});

describe("a changed name that begins with a byte order mark", () => {
  for (const generated of [true, false])
    test(`is checked against its attributes, generated=${generated}`, () => {
      const cwd = repository();
      const path = join(cwd, "\u{feff}gen.ts");
      const marker = generated ? "\u{feff}gen.ts linguist-generated\n" : "";

      write(join(cwd, ".gitattributes"), `*.md text\n${marker}`);
      write(path);
      git(cwd, "add", ".");
      git(cwd, "commit", "-qm", "initial");
      write(path, multiline);

      expect(run({ cwd }, "--fix", "--changed").code).toBe(0);
      expect(readFileSync(path, "utf8")).toBe(
        generated ? multiline : multiline.replace("};\n", "};\n\n"),
      );
    });
});

test("hunks keep their line numbers despite git config, environment and binary attributes", () => {
  const source = `\n${Array.from({ length: 20 }, (_, index) => `const n${index} = ${index};\n`).join("")}`;
  const cwd = scratchGitRepository({
    files: { "a.ts": source, "binary.ts": source, ".gitattributes": "binary.ts binary\n" },
    staged: true,
  });

  git(cwd, "config", "commit.gpgsign", "false");
  git(cwd, "-c", "user.email=t@t", "-c", "user.name=t", "commit", "-qm", "init");
  git(cwd, "config", "diff.interHunkContext", "10");
  git(cwd, "config", "diff.suppressBlankEmpty", "true");

  const changed = source.replace("n2 = 2", "n2 = 30").replace("n8 = 8", "n8 = 90");
  write(join(cwd, "a.ts"), changed);
  write(join(cwd, "binary.ts"), changed);
  write(join(cwd, "untracked.ts"), changed);

  const previous = process.env.GIT_DIFF_OPTS;
  process.env.GIT_DIFF_OPTS = "--unified=10";

  try {
    const collected = collectChanged(cwd, true);
    expect(collected.errors).toEqual([]);
    expect(collected.changedLines).toEqual(
      new Map([
        [join(realpathSync(cwd), "a.ts"), { lines: new Set([4, 10]), deletedAfter: new Set() }],
        [
          join(realpathSync(cwd), "binary.ts"),
          { lines: new Set([4, 10]), deletedAfter: new Set() },
        ],
      ]),
    );
  } finally {
    if (previous === undefined) delete process.env.GIT_DIFF_OPTS;
    else process.env.GIT_DIFF_OPTS = previous;
  }
});

test("hunks map quoted paths and record deletions between lines", () => {
  const cwd = scratchGitRepository({
    files: { 'a "quoted".ts': "a();\nb();\nc();\n", "z.ts": "x();\n" },
    staged: true,
  });

  git(cwd, "config", "commit.gpgsign", "false");
  git(cwd, "-c", "user.email=t@t", "-c", "user.name=t", "commit", "-qm", "init");
  write(join(cwd, 'a "quoted".ts'), "a();\nc();\n");
  write(join(cwd, "z.ts"), "y();\n");

  const collected = collectChanged(cwd, true);
  expect(collected.errors).toEqual([]);
  expect(collected.changedLines).toEqual(
    new Map([
      [join(realpathSync(cwd), 'a "quoted".ts'), { lines: new Set(), deletedAfter: new Set([1]) }],
      [join(realpathSync(cwd), "z.ts"), { lines: new Set([1]), deletedAfter: new Set() }],
    ]),
  );
});

test("hunks merge typechange sections before matching file names", () => {
  const cwd = scratchGitRepository({ files: { "z.ts": "z();\n" }, staged: true });
  symlinkSync("z.ts", join(cwd, "a.ts"));
  git(cwd, "add", "a.ts");

  git(cwd, "config", "commit.gpgsign", "false");
  git(cwd, "-c", "user.email=t@t", "-c", "user.name=t", "commit", "-qm", "init");

  rmSync(join(cwd, "a.ts"));
  write(join(cwd, "a.ts"), "a();\nb();\n");
  write(join(cwd, "z.ts"), "y();\n");

  const collected = collectChanged(cwd, true);
  expect(collected.errors).toEqual([]);
  expect(collected.changedLines.get(join(realpathSync(cwd), "a.ts"))).toEqual({
    lines: new Set([1, 2]),
    deletedAfter: new Set(),
  });

  expect(collected.changedLines.get(join(realpathSync(cwd), "z.ts"))).toEqual({
    lines: new Set([1]),
    deletedAfter: new Set(),
  });
});

test("a file named HEAD does not widen the changed set", () => {
  const cwd = repository();
  write(join(cwd, "tracked.ts"));
  git(cwd, "add", "tracked.ts");
  git(cwd, "commit", "-qm", "initial");

  write(join(cwd, "HEAD"), "x\n");
  expect(collectChanged(cwd)).toEqual({
    changedLines: new Map(),
    files: [],
    errors: [],
    warnings: [],
    submodules: [],
  });
});

describe("a folder argument in a repository judges skipped names from the root", () => {
  const braced = "if (a) {\n  b();\n}\n";
  function committed(): string {
    const cwd = repository();
    write(join(cwd, "dist/a.ts"), braced);
    git(cwd, "add", ".");
    git(cwd, "commit", "-qm", "initial");
    return cwd;
  }

  const runs = (cwd: string) => [
    ...[".", "dist/a.ts", "dist"].map((path) => ({ path, result: run({ cwd }, "--check", path) })),
    { path: ".", result: run({ cwd: join(cwd, "dist") }, "--check", ".") },
  ];

  test("skips what naming a file in it skips", () => {
    const cwd = committed();
    for (const { path, result } of runs(cwd))
      expect(result).toEqual({
        code: 0,
        stdout: "",
        stderr: `stanza: nothing to format in ${path}, its files are always skipped\n`,
      });
  });

  test("takes what a set stanza attribute opts in", () => {
    const cwd = committed();
    write(join(cwd, ".gitattributes"), "dist/** stanza=true\n");
    git(cwd, "add", ".gitattributes");
    git(cwd, "commit", "-qm", "attributes");

    const results = runs(cwd).map(({ result }) => result);
    const finding = (name: string) => ({
      code: 1,
      stdout: `${name}:1:8: braces: remove the braces around this single statement body\n`,
      stderr: "1 finding in 1 file, fixable with --fix\n",
    });

    expect(results).toEqual([...Array(3).fill(finding("dist/a.ts")), finding("a.ts")]);
  });

  test.skipIf(process.getuid?.() === 0)(
    "reports no folder in it that git cannot open, as the root run does not",
    () => {
      const cwd = committed();
      write(join(cwd, "dist/locked/b.ts"), braced);
      chmodSync(join(cwd, "dist/locked"), 0o000);

      try {
        for (const path of [".", "dist"])
          expect(run({ cwd }, "--check", path)).toEqual({
            code: 0,
            stdout: "",
            stderr: `stanza: nothing to format in ${path}, its files are always skipped\n`,
          });
      } finally {
        chmodSync(join(cwd, "dist/locked"), 0o755);
      }
    },
  );

  test.skipIf(process.getuid?.() === 0)(
    "reports a folder git cannot open when the stanza attribute may opt its files in",
    () => {
      const cwd = committed();
      write(join(cwd, ".gitattributes"), "dist/** stanza=true\n");
      write(join(cwd, "dist/locked/b.ts"), braced);
      chmodSync(join(cwd, "dist/locked"), 0o000);

      try {
        for (const path of [".", "dist"])
          expect(run({ cwd }, "--check", path)).toEqual({
            code: 2,
            stdout: "dist/a.ts:1:8: braces: remove the braces around this single statement body\n",
            stderr:
              "stanza: cannot read dist/locked: permission denied\n1 finding in 1 file, fixable with --fix\n",
          });
      } finally {
        chmodSync(join(cwd, "dist/locked"), 0o755);
      }
    },
  );

  test.skipIf(process.getuid?.() === 0)(
    "reads the opt in of a folder git can enter but not list",
    () => {
      const cwd = committed();
      write(join(cwd, "dist/locked/.gitattributes"), "*.ts stanza\n");
      write(join(cwd, "dist/locked/b.ts"), braced);
      chmodSync(join(cwd, "dist/locked"), 0o100);

      try {
        for (const path of [".", "dist"])
          expect(run({ cwd }, "--check", path)).toEqual({
            code: 2,
            stdout: "",
            stderr: "stanza: cannot read dist/locked: permission denied\n",
          });
      } finally {
        chmodSync(join(cwd, "dist/locked"), 0o755);
      }
    },
  );
});

test("a set stanza attribute opts files below skipped folders in", () => {
  const cwd = repository();
  write(
    join(cwd, ".gitattributes"),
    "dist/** stanza\nnode_modules/** stanza=true\nbuild/** stanza=yes\ndist/ling.ts linguist-generated\n",
  );

  const names = [
    "dist/a.ts",
    "node_modules/x/b.ts",
    "build/c.ts",
    "dist/a.d.ts",
    "dist/a.gen.ts",
    "dist/ling.ts",
  ];

  for (const name of names) write(join(cwd, name));
  const root = realpathSync(cwd);
  const taken = [join(root, "dist/a.ts"), join(root, "node_modules/x/b.ts")];
  expect(collectChanged(cwd).files).toEqual(taken);
});

function trackedSymlink(): string {
  const cwd = repository();
  const outside = join(scratch("files"), "outside.ts");

  write(outside);
  write(join(cwd, "inside.ts"));
  symlinkSync(outside, join(cwd, "link.ts"));
  git(cwd, "add", "inside.ts", "link.ts");
  return cwd;
}

test("--changed skips a tracked symlink so fixes never write outside the repo", () => {
  const cwd = trackedSymlink();
  expect(collectChanged(cwd)).toEqual({
    changedLines: new Map(),
    files: [join(realpathSync(cwd), "inside.ts")],
    errors: [],
    warnings: [],
    submodules: [],
  });
});

function movedDirectory(): string {
  const cwd = repository();
  const elsewhere = scratch("files");

  write(join(cwd, "keep.ts"));
  write(join(cwd, "pkg/moved.ts"));
  git(cwd, "add", "keep.ts", "pkg/moved.ts");
  git(cwd, "commit", "-qm", "initial");

  rmSync(join(cwd, "pkg"), { recursive: true });
  write(join(elsewhere, "moved.ts"));
  symlinkSync(elsewhere, join(cwd, "pkg"));
  write(join(cwd, "keep.ts"), "export const changed = true;\n");
  return cwd;
}

test("--changed skips a tracked directory replaced by a symlink out of the repo", () => {
  const cwd = movedDirectory();
  expect(collectChanged(cwd)).toEqual({
    changedLines: new Map(),
    files: [join(realpathSync(cwd), "keep.ts")],
    errors: [],
    warnings: [],
    submodules: [],
  });
});

function ignoredTarget(): string {
  const cwd = repository();

  write(join(cwd, ".gitignore"), "hidden/\n");
  write(join(cwd, "keep.ts"));
  write(join(cwd, "src/a.ts"));
  git(cwd, "add", ".gitignore", "keep.ts", "src/a.ts");
  git(cwd, "commit", "-qm", "initial");

  rmSync(join(cwd, "src"), { recursive: true });
  write(join(cwd, "hidden/a.ts"), "export const hidden = true;\n");
  symlinkSync("hidden", join(cwd, "src"));
  write(join(cwd, "keep.ts"), "export const changed = true;\n");
  return cwd;
}

test("--changed skips a tracked directory replaced by a symlink to an ignored folder", () => {
  const cwd = ignoredTarget();
  expect(collectChanged(cwd)).toEqual({
    changedLines: new Map(),
    files: [join(realpathSync(cwd), "keep.ts")],
    errors: [],
    warnings: [],
    submodules: [],
  });
});

test.skipIf(process.getuid?.() === 0)(
  "a replaced directory linking to an unsearchable ignored folder is skipped without an error",
  () => {
    const cwd = ignoredTarget();
    const kept = [join(realpathSync(cwd), "keep.ts")];

    chmodSync(join(cwd, "hidden"), 0o000);
    try {
      expect(collectChanged(cwd)).toEqual({
        changedLines: new Map(),
        files: kept,
        errors: [],
        warnings: [],
        submodules: [],
      });
    } finally {
      chmodSync(join(cwd, "hidden"), 0o755);
    }
  },
);

test("a broken branch ref is an error, an orphan branch is not", () => {
  const cwd = repository();
  write(join(cwd, "tracked.ts"));
  git(cwd, "add", "tracked.ts");
  git(cwd, "commit", "-q", "-m", "initial");

  git(cwd, "checkout", "-q", "--orphan", "fresh");
  expect(collectChanged(cwd)).toEqual({
    changedLines: new Map(),
    files: [join(realpathSync(cwd), "tracked.ts")],
    errors: [],
    warnings: [],
    submodules: [],
  });

  writeFileSync(join(cwd, ".git", "refs", "heads", "fresh"), "junk\n");
  const broken = collectChanged(cwd);
  expect(broken.files).toEqual([]);
  expect(broken.errors).toEqual([expect.stringContaining("git symbolic-ref failed")]);
});

function brokenIndex(): string {
  const cwd = repository();
  write(join(cwd, "tracked.ts"));
  git(cwd, "add", "tracked.ts");
  writeFileSync(join(cwd, ".git", "index"), "junkjunkjunkjunkjunk");
  return cwd;
}

function expectGitErrors(collected: { files: string[]; errors: string[] }): void {
  expect(collected.files).toEqual([]);
  expect(collected.errors.length).toBeGreaterThan(0);
  for (const error of collected.errors) expect(error).toContain("git");
}

test("--changed with a broken git index is an error, not an empty selection", () => {
  expectGitErrors(collectChanged(brokenIndex()));
});

test.skipIf(process.getuid?.() === 0)(
  "an ignore file git cannot read fails a repository run as it fails the walk",
  () => {
    const braced = "if (a) {\n  b();\n}\n";
    const cwd = repository();
    write(join(cwd, "sub/.gitignore"), "secret/\n");
    write(join(cwd, "top.ts"), braced);

    git(cwd, "add", ".");
    git(cwd, "commit", "-qm", "initial");

    write(join(cwd, "top.ts"), braced.replace("a", "c"));
    write(join(cwd, "sub/secret/s.ts"), braced);

    const top = "top.ts:1:8: braces: remove the braces around this single statement body\n";
    const secret =
      "sub/secret/s.ts:1:8: braces: remove the braces around this single statement body\n";

    chmodSync(join(cwd, "sub/.gitignore"), 0o000);
    try {
      for (const source of ["--changed", "."])
        expect(run({ cwd }, "--check", source)).toEqual({
          code: 2,
          stdout: secret + top,
          stderr:
            "stanza: cannot read sub/.gitignore: permission denied\n2 findings in 2 files, all fixable with --fix\n",
        });
    } finally {
      chmodSync(join(cwd, "sub/.gitignore"), 0o600);
    }

    for (const source of ["--changed", "."])
      expect(run({ cwd }, "--check", source)).toEqual({
        code: 1,
        stdout: top,
        stderr: "1 finding in 1 file, fixable with --fix\n",
      });
  },
);

test.skipIf(process.getuid?.() === 0)(
  "an ignore file git cannot read in an opted in skipped folder fails every source",
  () => {
    const braced = "if (a) {\n  b();\n}\n";
    const cwd = repository();
    write(join(cwd, ".gitattributes"), "dist/** stanza\n");
    write(join(cwd, "dist/.gitignore"), "secret/\n");
    write(join(cwd, "dist/a.ts"), braced);

    git(cwd, "add", ".");
    git(cwd, "commit", "-qm", "initial");

    write(join(cwd, "dist/a.ts"), braced.replace("a", "c"));
    write(join(cwd, "dist/secret/s.ts"), braced);

    const finding = (path: string) =>
      `${path}:1:8: braces: remove the braces around this single statement body\n`;

    chmodSync(join(cwd, "dist/.gitignore"), 0o000);
    try {
      for (const source of ["--changed", "."])
        expect(run({ cwd }, "--check", source)).toEqual({
          code: 2,
          stdout: finding("dist/a.ts") + finding("dist/secret/s.ts"),
          stderr:
            "stanza: cannot read dist/.gitignore: permission denied\n2 findings in 2 files, all fixable with --fix\n",
        });
    } finally {
      chmodSync(join(cwd, "dist/.gitignore"), 0o600);
    }
  },
);

describe("a folder outside git named through a symlink", () => {
  const braced = "if (a) {\n  b();\n}\n";

  test("is walked once, from its real location", () => {
    const cwd = scratch("walk-spellings");
    write(join(cwd, "real/a.ts"), braced);
    symlinkSync("real", join(cwd, "link"));

    const above = scratch("walk-spellings-above");
    symlinkSync(cwd, join(above, "alias"));

    const expected = {
      code: 1,
      stdout: "real/a.ts:1:8: braces: remove the braces around this single statement body\n",
      stderr: "1 finding in 1 file, fixable with --fix\n",
    };

    expect(run({ cwd }, "--check", "link", "real")).toEqual(expected);
    expect(run({ cwd }, "--check", "link")).toEqual(expected);
    expect(run({ cwd }, "--check", join(above, "alias/real"))).toEqual(expected);
  });

  test("applies the ignore files above its real location", () => {
    const cwd = scratch("walk-real-ignores");
    write(join(cwd, "project/.gitignore"), "gen/\n");
    write(join(cwd, "project/src/gen/g.ts"), braced);
    mkdirSync(join(cwd, "elsewhere"));
    symlinkSync("../project/src", join(cwd, "elsewhere/link"));

    const linked = run({ cwd }, "--check", "elsewhere/link");
    expect(linked.code).toBe(0);
    expect(linked.stdout).toBe("");
    expect(run({ cwd }, "--check", "project/src").code).toBe(0);
  });

  test.skipIf(process.getuid?.() === 0)(
    "names what it cannot read under the folder as typed",
    () => {
      const cwd = scratch("walk-typed-errors");
      write(join(cwd, "real/a.ts"), braced);
      write(join(cwd, "real/locked/b.ts"), braced);
      write(join(cwd, "real/sub/.gitignore"), "x.ts\n");
      symlinkSync("real", join(cwd, "link"));

      chmodSync(join(cwd, "real/locked"), 0o000);
      chmodSync(join(cwd, "real/sub/.gitignore"), 0o000);

      try {
        expect(run({ cwd }, "--check", "link")).toEqual({
          code: 2,
          stdout: "real/a.ts:1:8: braces: remove the braces around this single statement body\n",
          stderr:
            "stanza: cannot read link/locked: permission denied\nstanza: cannot read link/sub/.gitignore: permission denied\n1 finding in 1 file, fixable with --fix\n",
        });
      } finally {
        chmodSync(join(cwd, "real/locked"), 0o755);
        chmodSync(join(cwd, "real/sub/.gitignore"), 0o644);
      }
    },
  );
});

describe("a repository the walk outside git meets", () => {
  const braced = "if (a) {\n  b();\n}\n";
  const finding = (name: string) =>
    `${name}:1:8: braces: remove the braces around this single statement body\n`;

  function nested(): string {
    const cwd = scratch("nested-repository");
    write(join(cwd, ".gitignore"), "skipped/\nchild/listed.ts\n");

    for (const name of ["child", "skipped"]) {
      mkdirSync(join(cwd, name));
      git(join(cwd, name), "init", "-q");
    }

    write(join(cwd, "child/.gitattributes"), "pinned.ts -stanza\ngen.ts linguist-generated\n");

    for (const name of [
      "child/pinned.ts",
      "child/gen.ts",
      "child/plain.ts",
      "child/listed.ts",
      "skipped/a.ts",
    ])
      write(join(cwd, name), braced);

    return cwd;
  }

  test("selects what naming the repository selects", () => {
    const cwd = nested();
    const named = run({ cwd }, "--check", "child");
    expect(named).toEqual({
      code: 1,
      stdout: ["child/listed.ts", "child/plain.ts"].map(finding).join(""),
      stderr: "2 findings in 2 files, all fixable with --fix\n",
    });

    expect(run({ cwd }, "--check", ".")).toEqual(named);
    expect(run({ cwd }, "--fix", ".").code).toBe(0);

    for (const name of ["child/listed.ts", "child/plain.ts"])
      expect(readFileSync(join(cwd, name), "utf8")).toBe("if (a) b();\n");

    for (const name of ["child/pinned.ts", "child/gen.ts", "skipped/a.ts"])
      expect(readFileSync(join(cwd, name), "utf8")).toBe(braced);
  });

  test("is walked as a folder when git is missing", () => {
    const cwd = nested();
    const env = { ...process.env, PATH: scratch("no-git") };
    expect(run({ cwd, env }, "--check", ".")).toEqual({
      code: 1,
      stdout: ["child/gen.ts", "child/pinned.ts", "child/plain.ts"].map(finding).join(""),
      stderr:
        "stanza: git not found on PATH, file selection fell back to the directory walk\n3 findings in 3 files, all fixable with --fix\n",
    });
  });

  test("reports git's error for a folder git refuses, as naming it does", () => {
    const cwd = scratch("refused-repository");
    write(join(cwd, "child/a.ts"), braced);
    mkdirSync(join(cwd, "child/.git"));

    const named = run({ cwd }, "--check", "child");
    expect(named.code).toBe(2);
    expect(named.stderr).toStartWith("stanza: git rev-parse failed in child: ");
    expect(run({ cwd }, "--check", ".")).toEqual(named);
  });

  test.skipIf(process.getuid?.() === 0)(
    "reports a repository folder it cannot read, as naming it does",
    () => {
      const cwd = scratch("unreadable-repository");
      mkdirSync(join(cwd, "child"));
      git(join(cwd, "child"), "init", "-q");

      write(join(cwd, "child/a.ts"), braced);
      git(join(cwd, "child"), "add", "a.ts");
      write(join(cwd, "child/.gitignore"), "a.ts\n");
      chmodSync(join(cwd, "child/.gitignore"), 0o000);
      chmodSync(join(cwd, "child"), 0o111);

      try {
        const named = run({ cwd }, "--check", "child");
        expect(named).toEqual({
          code: 2,
          stdout: "",
          stderr: "stanza: cannot read child: permission denied\n",
        });

        expect(run({ cwd }, "--check", ".")).toEqual(named);
      } finally {
        chmodSync(join(cwd, "child"), 0o755);
      }
    },
  );
});

for (const [born, hunks] of [
  [false, false],
  [false, true],
  [true, false],
  [true, true],
])
  test(`a written set narrows changed files to it, born=${born}, hunks=${hunks}`, () => {
    const names = ["plain.ts", "star*.ts", "question?.ts", "bracket[1].ts", ":(glob)*.ts"];
    const cwd = repository();
    for (const name of names) write(join(cwd, name));
    git(cwd, "add", "-A");

    if (born) git(cwd, "commit", "-qm", "initial");

    for (const name of names) write(join(cwd, name), "export const value = 2;\n");
    for (const name of names) write(join(cwd, `untracked-${name}`));
    write(join(cwd, "directory/child.ts"));

    const whole = collectChanged(cwd, hunks);
    expect(whole.errors).toEqual([]);

    const selections = [
      ...names.map((name) => new Set([join(cwd, name), join(cwd, `untracked-${name}`)])),
      new Set(names.map((name) => join(cwd, name))),
      new Set([join(cwd, "directory")]),
      new Set([join(cwd, "missing.ts")]),
      new Set([join(cwd, "..", "outside.ts")]),
      new Set<string>(),
    ];

    for (const written of selections)
      expect(collectChanged(cwd, hunks, written)).toEqual({
        ...whole,
        files: whole.files.filter((path) => written.has(path)),
        changedLines: new Map([...whole.changedLines].filter(([path]) => written.has(path))),
      });
  });

for (const [index, hunks] of [
  ["the index", false],
  ["the index", true],
  ["a version 4 index", true],
  ["GIT_INDEX_FILE", true],
] as const)
  test(`a written set selects unindexed and new files as the whole run does, ${index}, hunks=${hunks}`, () => {
    const cwd = repository();
    const names = ["kept.ts", "removed.ts"];
    for (const name of names) write(join(cwd, name));
    git(cwd, "add", "-A");
    git(cwd, "commit", "-qm", "initial");

    for (const name of names) write(join(cwd, name), "export const value = 2;\n");
    write(join(cwd, "new.ts"));

    const other = join(cwd, ".git", "other-index");
    const previous = process.env.GIT_INDEX_FILE;

    if (index === "GIT_INDEX_FILE") {
      writeFileSync(other, readFileSync(join(cwd, ".git", "index")));
      process.env.GIT_INDEX_FILE = other;
    }

    try {
      const removed = Bun.spawnSync([gitBinary, "-C", cwd, "rm", "-q", "--cached", "removed.ts"], {
        env: process.env,
      });

      expect(removed.exitCode).toBe(0);
      if (index === "a version 4 index") git(cwd, "update-index", "--index-version", "4");

      const whole = collectChanged(cwd, hunks);
      expect(whole.files).toEqual(
        ["kept.ts", "new.ts", "removed.ts"].map((name) => join(cwd, name)),
      );

      expect([...whole.changedLines.keys()]).toEqual(hunks ? [join(cwd, "kept.ts")] : []);

      const selections = [
        ["kept.ts"],
        ["removed.ts"],
        ["new.ts"],
        ["kept.ts", "new.ts"],
        ["kept.ts", "removed.ts", "new.ts"],
      ].map((selected) => new Set(selected.map((name) => join(cwd, name))));

      for (const written of selections)
        expect(collectChanged(cwd, hunks, written)).toEqual({
          ...whole,
          files: whole.files.filter((path) => written.has(path)),
          changedLines: new Map([...whole.changedLines].filter(([path]) => written.has(path))),
        });
    } finally {
      if (previous === undefined) delete process.env.GIT_INDEX_FILE;
      else process.env.GIT_INDEX_FILE = previous;
    }
  });

test("a written set past the pathspec budget still narrows changed files", () => {
  const cwd = repository();
  const written = new Set(
    Array.from({ length: 400 }, (_, index) => join(cwd, `${"long".repeat(50)}-${index}.ts`)),
  );

  for (const path of written) write(path);
  write(join(cwd, "other.ts"));

  expect(collectChanged(cwd, false, written).files).toEqual([...written].sort(byteOrder));
});

test("a written set past the pathspec budget lists untracked files as the whole run does", () => {
  const cwd = repository();
  const tracked = Array.from({ length: 400 }, (_, index) => `${"long".repeat(50)}-${index}.ts`);
  for (const name of ["a.ts", ...tracked]) write(join(cwd, name));
  git(cwd, "add", "-A");
  git(cwd, "commit", "-qm", "initial");

  write(join(cwd, "\uFEFFa.ts"));

  const whole = collectChanged(cwd);
  expect(whole.errors).toEqual([]);

  const written = new Set(["a.ts", ...tracked].map((name) => join(cwd, name)));
  expect(collectChanged(cwd, true, written).files).toEqual(
    whole.files.filter((path) => written.has(path)),
  );

  expect(collectChanged(cwd, false, written).files).toEqual([...written].sort(byteOrder));
});

const pinned = "if (ok) {\n  run();\n}\n";
const edited = `${pinned}if (done) {\n  stop();\n}\n`;
function optedOut(): string {
  const cwd = repository();
  write(join(cwd, ".gitattributes"), "pinned.ts -stanza\nfalse.ts stanza=false\n");
  write(join(cwd, "pinned.ts"), pinned);
  write(join(cwd, "false.ts"), pinned);

  git(cwd, "add", ".");
  git(cwd, "commit", "-qm", "init");

  for (const name of ["pinned.ts", "false.ts"]) write(join(cwd, name), edited);
  git(cwd, "add", ".");

  return cwd;
}

describe("the stanza attribute", () => {
  test("keeps every selection source off a file it opts out", () => {
    const cwd = optedOut();
    const unchanged = () => {
      for (const name of ["pinned.ts", "false.ts"])
        expect(readFileSync(join(cwd, name), "utf8")).toBe(edited);
    };

    expect(run({ cwd }, "--check", "--braces", ".")).toEqual({
      code: 0,
      stdout: "",
      stderr: "stanza: nothing to format in ., the stanza attribute opts its files out\n",
    });

    for (const source of [["."], ["pinned.ts", "false.ts"]]) {
      expect(run({ cwd }, "--fix", "--braces", ...source).code).toBe(0);
      unchanged();
    }

    const stdin = run(
      { cwd, stdin: Buffer.from(edited) },
      "--fix",
      "--braces",
      "--stdin",
      "pinned.ts",
    );

    expect(stdin).toEqual({ code: 0, stdout: edited, stderr: "" });
  });

  test("keeps --changed and --staged off a file it opts out", () => {
    const cwd = optedOut();
    for (const source of ["--staged", "--changed"])
      expect(run({ cwd }, "--check", source, "--braces")).toEqual({
        code: 0,
        stdout: "",
        stderr: `no ${source.slice(2)} files to check\n`,
      });

    expect(run({ cwd }, "--fix", "--braces", "--changed").code).toBe(0);

    for (const name of ["pinned.ts", "false.ts"])
      expect(readFileSync(join(cwd, name), "utf8")).toBe(edited);
  });

  test("names the attribute as the reason a file is skipped", () => {
    const cwd = optedOut();
    expect(run({ cwd }, "--check", "pinned.ts")).toEqual({
      code: 0,
      stdout: "",
      stderr: "stanza: nothing to format in pinned.ts, the stanza attribute opts its files out\n",
    });
  });

  test("explain names the attribute as the reason a file is skipped", () => {
    const cwd = optedOut();
    expect(run({ cwd }, "explain", "--", "pinned.ts:1").stdout).toEndWith(
      "\nnote: --fix and --check skip this file, its stanza attribute opts it out\n",
    );
  });

  test("--staged honours an opt out the index does not hold yet", () => {
    const cwd = repository();
    write(join(cwd, "pinned.ts"), edited);
    git(cwd, "add", "pinned.ts");
    expect(run({ cwd }, "--check", "--braces", "--staged").code).toBe(1);

    write(join(cwd, ".gitattributes"), "pinned.ts -stanza\n");
    expect(run({ cwd }, "--check", "--braces", "--staged")).toEqual({
      code: 0,
      stdout: "",
      stderr: "no staged files to check\n",
    });
  });

  test("selects a file the attribute sets or leaves unspecified, and never calls it generated", () => {
    const cwd = repository();
    write(
      join(cwd, ".gitattributes"),
      "set.ts stanza\nvalue.ts stanza=yes\nunset.ts !stanza\nboth.ts -stanza linguist-generated\n",
    );

    for (const name of ["set.ts", "value.ts", "unset.ts", "plain.ts", "both.ts"])
      write(join(cwd, name));

    expect(run({ cwd }, "--check", "both.ts").stderr).toBe(
      "stanza: nothing to format in both.ts, the stanza attribute opts its files out\n",
    );

    write(join(cwd, ".gitattributes"), "pinned.gen.ts -stanza\n");
    write(join(cwd, "pinned.gen.ts"));
    expect(run({ cwd }, "--check", "pinned.gen.ts").stderr).toBe(
      "stanza: nothing to format in pinned.gen.ts, the stanza attribute opts its files out\n",
    );
  });

  test("explain names the attribute that opts out a generated name", () => {
    const cwd = repository();
    write(join(cwd, ".gitattributes"), "pinned.gen.ts -stanza\n");
    write(join(cwd, "pinned.gen.ts"));
    expect(run({ cwd }, "explain", "--", "pinned.gen.ts:1").stdout).toEndWith(
      "\nnote: --fix and --check skip this file, its stanza attribute opts it out\n",
    );
  });
});
