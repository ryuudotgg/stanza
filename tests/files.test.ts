import { describe, expect, test } from "bun:test";
import {
  chmodSync,
  cpSync,
  existsSync,
  mkdirSync,
  readFileSync,
  realpathSync,
  renameSync,
  rmSync,
  symlinkSync,
  writeFileSync,
} from "node:fs";
import { join, relative, resolve } from "node:path";
import {
  colorless,
  git,
  gitRefusesOwnership,
  gitBinary,
  hunkFunction,
  run,
  scratch,
  scratchGitRepository,
  stanzaBinary,
  superproject,
} from "./support.ts";

const bracedReturn = `${hunkFunction("f")}\n`;
const unbracedReturn = "function f(a: boolean) {\n  if (a) return 1;\n  return 2;\n}\n";
const singleBlock = "if (a) {\n  b();\n}\n";

function write(path: string, text = "export {};\n"): void {
  mkdirSync(resolve(path, ".."), { recursive: true });
  writeFileSync(path, text);
}

test("a vanished selection directory leaves its sibling in scope", () => {
  const braced = "if (ready) { run(); }\n";
  const cwd = scratchGitRepository();
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
    const cwd = scratchGitRepository();
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
    const cwd = scratchGitRepository();
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
    const cwd = scratchGitRepository();
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
    const cwd = scratchGitRepository();
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
    const cwd = scratchGitRepository();
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
    const cwd = scratchGitRepository();
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
      const cwd = scratchGitRepository();
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

describe("a folder argument in a repository judges skipped names from the root", () => {
  const braced = "if (a) {\n  b();\n}\n";
  function committed(): string {
    const cwd = scratchGitRepository();
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

test("--changed passes over a deleted file and still checks its siblings", () => {
  const cwd = scratchGitRepository();

  write(join(cwd, "gone.ts"));
  write(join(cwd, "good.ts"));
  git(cwd, "add", ".");
  git(cwd, "commit", "-qm", "initial");

  rmSync(join(cwd, "gone.ts"));
  write(join(cwd, "good.ts"), "if (a) {\n  b();\n}\n");

  const checked = run({ cwd }, "--check", "--changed");
  expect(checked.code).toBe(1);
  expect(checked.stdout).toBe(
    "good.ts:1:8: braces: remove the braces around this single statement body\n",
  );

  expect(checked.stderr).not.toContain("cannot read");

  expect(run({ cwd }, "--fix", "--changed").code).toBe(0);
  expect(readFileSync(join(cwd, "good.ts"), "utf8")).toBe("if (a) b();\n");
  expect(() => readFileSync(join(cwd, "gone.ts"))).toThrow();
});

test.skipIf(process.getuid?.() === 0)(
  "an ignore file git cannot read fails a repository run as it fails the walk",
  () => {
    const braced = "if (a) {\n  b();\n}\n";
    const cwd = scratchGitRepository();
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
    const cwd = scratchGitRepository();
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

const pinned = "if (ok) {\n  run();\n}\n";
const edited = `${pinned}if (done) {\n  stop();\n}\n`;
function optedOut(): string {
  const cwd = scratchGitRepository();
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
    const cwd = scratchGitRepository();
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
    const cwd = scratchGitRepository();
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
    const cwd = scratchGitRepository();
    write(join(cwd, ".gitattributes"), "pinned.gen.ts -stanza\n");
    write(join(cwd, "pinned.gen.ts"));
    expect(run({ cwd }, "explain", "--", "pinned.gen.ts:1").stdout).toEndWith(
      "\nnote: --fix and --check skip this file, its stanza attribute opts it out\n",
    );
  });
});

const probe = scratch("case-probe");
writeFileSync(join(probe, "case"), "");
const caseInsensitive = existsSync(join(probe, "CASE"));

test.skipIf(!caseInsensitive)("directory selection preserves the on-disk file case", () => {
  const cwd = scratchGitRepository({
    files: { "src/case.ts": "function f(a: boolean) {\n  if (a) {\n    first();\n  }\n}\n" },
    staged: true,
  });

  renameSync(join(cwd, "src/case.ts"), join(cwd, "src/Case.ts"));

  const result = run({ cwd }, "--check", ".");
  expect(result.code).toBe(1);
  expect(result.stdout).toContain("src/Case.ts");
  expect(result.stdout).not.toContain("src/case.ts");
});

test.skipIf(!caseInsensitive)("selection preserves the on-disk directory case", () => {
  const cwd = scratchGitRepository({
    files: {
      "src/b.ts": "export {};\n",
      "src/lib/a.ts": "function f(a: boolean) {\n  if (a) {\n    first();\n  }\n}\n",
    },
    staged: true,
  });

  renameSync(join(cwd, "src/lib"), join(cwd, "src/Lib"));

  for (const path of [".", "src/lib/a.ts", "src/LIB/A.ts"]) {
    const result = run({ cwd }, "--check", path);
    expect(result.code).toBe(1);
    expect(result.stdout).toContain("src/Lib/a.ts");
    expect(result.stdout).not.toContain("src/lib/");
  }
});

test("missing paths use the supplied spelling", () => {
  const cwd = scratch("missing");

  expect(run({ cwd }, "--check", "nope.ts")).toEqual({
    code: 2,
    stderr: "stanza: no such file: nope.ts\n",
    stdout: "",
  });
});

test.skipIf(process.getuid?.() === 0)(
  "error lines for unreadable folders print in byte order across path arguments",
  () => {
    const cwd = scratch("unreadable");
    const folders = ["\u{ff71}", "\u{1f600}"];
    for (const folder of folders) mkdirSync(join(cwd, folder), { mode: 0 });

    try {
      const lines =
        "stanza: cannot read \u{ff71}: permission denied\nstanza: cannot read \u{1f600}: permission denied\n";

      expect(run({ cwd }, "--check", ".")).toEqual({ code: 2, stderr: lines, stdout: "" });
      expect(run({ cwd }, "--check", "\u{1f600}", "\u{ff71}").stderr).toBe(lines);
    } finally {
      for (const folder of folders) chmodSync(join(cwd, folder), 0o755);
    }
  },
);

test.skipIf(process.getuid?.() === 0)(
  "error lines of several path arguments merge by path, whatever each error says",
  () => {
    const cwd = scratch("arguments");
    mkdirSync(join(cwd, "z"), { mode: 0 });

    try {
      expect(run({ cwd }, "--check", "z", "a").stderr).toBe(
        "stanza: no such file: a\nstanza: cannot read z: permission denied\n",
      );
    } finally {
      chmodSync(join(cwd, "z"), 0o755);
    }
  },
);

test("directories with no source files report an empty selection", () => {
  const plain = scratch("plain");
  mkdirSync(join(plain, "docs"));
  writeFileSync(join(plain, "docs", "a.md"), "text\n");

  const repository = scratchGitRepository({ files: { "docs/a.md": "text\n" } });
  for (const cwd of [plain, repository])
    expect(run({ cwd }, "--check", "docs")).toEqual({
      code: 0,
      stderr: "stanza: no TypeScript or JavaScript files under docs\n",
      stdout: "",
    });
});

test("generated paths report why the selection is empty", () => {
  const cwd = scratch("generated");
  mkdirSync(join(cwd, "api"));
  writeFileSync(join(cwd, "a.gen.ts"), "export const value = 1;\n");
  writeFileSync(join(cwd, "api", "schema.gen.ts"), "export const value = 1;\n");

  for (const path of ["a.gen.ts", "api"])
    expect(run({ cwd }, "--check", path)).toEqual({
      code: 0,
      stderr: `stanza: nothing to format in ${path}, generated files are skipped\n`,
      stdout: "",
    });
});

test("header generated selections report why nothing was formatted", () => {
  const cwd = scratch("generated-header-dir");
  const generated = "// @generated\nif (a) { b(); }\n";
  mkdirSync(join(cwd, "dir"));
  writeFileSync(join(cwd, "dir", "a.ts"), generated);

  for (const mode of ["--check", "--fix"])
    for (const path of ["dir", "dir/a.ts"])
      expect(run({ cwd }, mode, path)).toEqual({
        code: 0,
        stderr: `stanza: nothing to format in ${path}, generated files are skipped\n`,
        stdout: "",
      });

  writeFileSync(join(cwd, "dir", "b.ts"), "if (a) {\n  b();\n}\n");

  const checked = run({ cwd }, "--check", "dir");
  expect(checked.code).toBe(1);
  expect(checked.stderr).toBe("1 finding in 1 file, fixable with --fix\n");
  expect(checked.stdout).toContain("dir/b.ts:1:8: braces: ");
  expect(checked.stdout).not.toContain("a.ts");

  const fixed = run({ cwd }, "--fix", "dir");
  expect(fixed.stderr).toBe("rewrote 1 file, no findings left\n");
  expect(readFileSync(join(cwd, "dir", "a.ts"), "utf8")).toBe(generated);
});

test("an opted out file beside a header generated one gets its own reason", () => {
  const cwd = scratchGitRepository({
    files: {
      ".gitattributes": "pinned.ts -stanza\n",
      "gen.ts": "// @generated\nif (a) { b(); }\n",
      "pinned.ts": "if (a) {\n  b();\n}\n",
    },
  });

  const stderr =
    "stanza: nothing to format in gen.ts, generated files are skipped\nstanza: nothing to format in pinned.ts, the stanza attribute opts its files out\n";

  for (const mode of ["--check", "--fix"])
    expect(run({ cwd }, mode, "gen.ts", "pinned.ts")).toEqual({ code: 0, stderr, stdout: "" });
});

test("a set stanza attribute opts a skipped folder in on every source", () => {
  const body = "if (a) {\n  b();\n}\n";
  const cwd = scratchGitRepository({
    files: { ".gitattributes": "dist/** stanza\n", "dist/a.ts": body },
    staged: true,
  });

  git(cwd, "commit", "-qm", "init");
  const finding = "dist/a.ts:1:8: braces: remove the braces around this single statement body\n";
  const found = (summary = "1 finding in 1 file, fixable with --fix") => ({
    code: 1,
    stderr: `${summary}\n`,
    stdout: finding,
  });

  expect(run({ cwd }, "--check", ".")).toEqual(found());
  expect(run({ cwd }, "--check", "dist/a.ts")).toEqual(found());

  for (const path of [".", "a.ts"])
    expect(run({ cwd: join(cwd, "dist") }, "--check", path)).toEqual({
      ...found(),
      stdout: finding.replace("dist/", ""),
    });

  expect(run({ cwd }, "explain", "dist/a.ts:1").stdout).not.toContain("skip this file");

  writeFileSync(join(cwd, "dist", "a.ts"), body.replace("b()", "c()"));
  git(cwd, "add", "dist/a.ts");
  expect(run({ cwd }, "--check", "--changed")).toEqual(found());

  const piped = run({ cwd, stdin: Buffer.from(body) }, "--check", "--stdin", "dist/a.ts");
  expect(piped.code).toBe(1);
  expect(piped.stdout).toBe(finding);

  const staged = run({ cwd }, "--check", "--staged");
  expect(staged.code).toBe(1);
  expect(staged.stdout).toBe(finding);
});

test("--staged needs the working tree to opt a skipped folder in too, as --fix does", () => {
  const cwd = scratchGitRepository({
    files: { ".gitattributes": "dist/** stanza\n", "dist/a.ts": "if (a) {\n  b();\n}\n" },
    staged: true,
  });

  writeFileSync(join(cwd, ".gitattributes"), "\n");

  for (const args of [
    ["--check", "--staged"],
    ["--fix", "dist/a.ts"],
  ])
    expect(run({ cwd }, ...args)).toMatchObject({ code: 0, stdout: "" });
});

test("the other skips still win over a set stanza attribute", () => {
  const body = "if (a) {\n  b();\n}\n";
  const cwd = scratchGitRepository({
    files: {
      ".gitattributes": "dist/** stanza\ndist/ling.ts linguist-generated\n",
      ".gitignore": "dist/ignored.ts\n",
      "dist/ignored.ts": body,
      "dist/a.d.ts": body,
      "dist/a.gen.ts": body,
      "dist/ling.ts": body,
      "dist/header.ts": `// @generated\n${body}`,
    },
  });

  expect(run({ cwd }, "--check", ".")).toEqual({
    code: 0,
    stderr: "stanza: nothing to format in ., generated files are skipped\n",
    stdout: "",
  });
});

test("ignored paths report why the selection is empty", () => {
  const plain = scratch("ignored");
  mkdirSync(join(plain, "ignored"));
  writeFileSync(join(plain, ".gitignore"), "ignored/\n");
  writeFileSync(join(plain, "ignored", "i.ts"), "export const value = 1;\n");

  const repository = scratchGitRepository({
    files: {
      ".gitignore": "ignored/\n",
      "ignored/i.ts": "export const value = 1;\n",
    },
  });

  expect(run({ cwd: plain }, "--check", "ignored")).toEqual({
    code: 0,
    stderr: "stanza: nothing to format in ignored, a .gitignore hides its files\n",
    stdout: "",
  });

  expect(run({ cwd: repository }, "--check", "ignored")).toEqual({
    code: 0,
    stderr: "stanza: nothing to format in ignored, git ignores its files\n",
    stdout: "",
  });
});

test("a .gitignore above a folder outside git hides its files without blaming git", () => {
  const parent = scratch("ancestors");
  writeFileSync(join(parent, ".gitignore"), "a.ts\n");
  mkdirSync(join(parent, "project", "src"), { recursive: true });
  writeFileSync(join(parent, "project", "src", "a.ts"), "if (a) {\n  b();\n}\n");

  const cwd = join(parent, "project");
  const hidden = "stanza: nothing to format in ., a .gitignore hides its files\n";

  expect(run({ cwd }, "--check", ".")).toEqual({ code: 0, stderr: hidden, stdout: "" });

  const env = { ...process.env, PATH: scratch("no-git") };
  const result = run({ cwd, env }, "--check", ".");
  expect(result.code).toBe(0);
  expect(result.stdout).toBe("");
  expect(result.stderr.split("\n")).toEqual([expect.stringContaining("git"), hidden.trim(), ""]);
});

const submoduleNotice =
  "stanza: skipped git submodule vendor/sub, pass its path to format its files\n";

test("selections name the submodules they skip", () => {
  const cwd = superproject({ "a.ts": "export const value = 1;\n" });
  const wall =
    "vendor/sub/s.ts:1:1: wall: split these 7 statements into steps with a blank line (by hand)\n";

  expect(run({ cwd }, "--check", ".")).toEqual({
    code: 0,
    stderr: `${submoduleNotice}checked 1 file, no findings\n`,
    stdout: "",
  });

  expect(run({ cwd }, "--check", "vendor")).toEqual({
    code: 0,
    stderr: submoduleNotice,
    stdout: "",
  });

  for (const paths of [["vendor/sub"], ["vendor/sub", "."]])
    expect(run({ cwd }, "--check", ...paths)).toEqual({
      code: 1,
      stderr: "1 finding in 1 file, by hand\n",
      stdout: wall,
    });
});

test("--changed names a changed submodule it skips", () => {
  const cwd = superproject({ "a.ts": "export const value = 1;\n" });
  expect(run({ cwd }, "--check", "--changed")).toEqual({
    code: 0,
    stderr: "checked 1 file, no findings\n",
    stdout: "",
  });

  writeFileSync(join(cwd, "vendor", "sub", "s.ts"), "g();\n");
  expect(run({ cwd }, "--check", "--changed")).toEqual({
    code: 0,
    stderr: `${submoduleNotice}checked 1 file, no findings\n`,
    stdout: "",
  });
});

test("a selection holding only a submodule names it as the reason", () => {
  const cwd = superproject({});
  expect(run({ cwd }, "--check", ".")).toEqual({ code: 0, stderr: submoduleNotice, stdout: "" });

  const outside = scratch("alias");
  symlinkSync(cwd, join(outside, "linked"));
  expect(run({ cwd: outside }, "--check", "linked")).toEqual({
    code: 0,
    stderr: `stanza: skipped git submodule ${relative(outside, join(cwd, "vendor", "sub"))}, pass its path to format its files\n`,
    stdout: "",
  });
});

test("a named submodule stays out of the outer argument's reason", () => {
  const cwd = superproject({});
  writeFileSync(join(cwd, "vendor", "sub", ".gitattributes"), "s.ts linguist-generated\n");

  expect(run({ cwd }, "--check", ".", "vendor/sub")).toEqual({
    code: 0,
    stderr:
      "stanza: no TypeScript or JavaScript files under .\nstanza: nothing to format in vendor/sub, generated files are skipped\n",
    stdout: "",
  });
});

test("each skipped submodule gets its own line, and an alias names it", () => {
  const cwd = superproject({});
  const other = scratchGitRepository({ files: { "o.ts": "export const o = 1;\n" }, staged: true });
  git(other, "commit", "-qm", "init");
  git(cwd, "-c", "protocol.file.allow=always", "submodule", "add", "-q", other, "lib/other");
  symlinkSync(join(cwd, "vendor", "sub"), join(cwd, "linked"));

  expect(run({ cwd }, "--check", ".")).toEqual({
    code: 0,
    stderr: `stanza: skipped git submodule lib/other, pass its path to format its files\n${submoduleNotice}`,
    stdout: "",
  });

  expect(run({ cwd }, "--check", ".", "linked").stderr).toBe(
    "stanza: skipped git submodule lib/other, pass its path to format its files\n1 finding in 1 file, by hand\n",
  );
});

test("a submodule whose name is not UTF-8 is named under any locale", () => {
  const cwd = superproject({});
  const gitmodules = Buffer.concat([
    Buffer.from('[submodule "vendor/'),
    Buffer.from([0xff]),
    Buffer.from('"]\n\tpath = vendor/sub\n'),
  ]);

  writeFileSync(join(cwd, ".gitmodules"), gitmodules);

  for (const LC_ALL of ["C", "C.UTF-8"])
    expect(run({ cwd, env: { ...process.env, LC_ALL } }, "--check", ".")).toEqual({
      code: 0,
      stderr: submoduleNotice,
      stdout: "",
    });
});

test("staged submodule commits are named", () => {
  const cwd = superproject({});
  const sub = join(cwd, "vendor", "sub");
  writeFileSync(join(sub, "s.ts"), "g();\n");
  git(sub, "commit", "-qam", "next");
  git(cwd, "add", "vendor/sub");

  expect(run({ cwd }, "--check", "--staged")).toEqual({
    code: 0,
    stderr: `${submoduleNotice}no staged files to check\n`,
    stdout: "",
  });
});

test("skipped paths report why the selection is empty", () => {
  const cwd = scratch("skipped");
  mkdirSync(join(cwd, "types"));
  writeFileSync(join(cwd, "types", "a.d.ts"), "export declare const value: number;\n");

  expect(run({ cwd }, "--check", "types")).toEqual({
    code: 0,
    stderr: "stanza: nothing to format in types, its files are always skipped\n",
    stdout: "",
  });

  const repository = scratchGitRepository({ files: { "dist/a.ts": "export const value = 1;\n" } });
  expect(run({ cwd: repository }, "--check", "dist/a.ts")).toEqual({
    code: 0,
    stderr: "stanza: nothing to format in dist/a.ts, its files are always skipped\n",
    stdout: "",
  });
});

function unreadableTree(dir: string): void {
  const line = "export const z = 1;\n";
  mkdirSync(join(dir, "nr"));
  writeFileSync(join(dir, "ok.ts"), line);
  writeFileSync(join(dir, "nr", "a.ts"), line);
  writeFileSync(join(dir, "nr", "b.ts"), line);
}

test.skipIf(process.getuid?.() === 0)("unreadable directories name themselves and exit 2", () => {
  const repository = scratchGitRepository();
  unreadableTree(repository);

  git(repository, "add", ".");
  git(repository, "commit", "-qm", "init");

  const walked = scratch("cli");
  unreadableTree(walked);

  for (const dir of [repository, walked]) {
    mkdirSync(join(dir, "un"));
    writeFileSync(join(dir, "un", "u.ts"), "export const z = 1;\n");
    chmodSync(join(dir, "nr"), 0o000);
    chmodSync(join(dir, "un"), 0o000);

    try {
      expect(run({ cwd: dir }, "--check", ".")).toEqual({
        code: 2,
        stderr:
          "stanza: cannot read nr: permission denied\nstanza: cannot read un: permission denied\nchecked 1 file, no findings\n",
        stdout: "",
      });

      expect(run({ cwd: dir }, "--check", "nr")).toEqual({
        code: 2,
        stderr: "stanza: cannot read nr: permission denied\n",
        stdout: "",
      });

      expect(run({ cwd: dir }, "--check", "nr/a.ts")).toEqual({
        code: 2,
        stderr: "stanza: cannot read nr/a.ts: permission denied\n",
        stdout: "",
      });
    } finally {
      chmodSync(join(dir, "nr"), 0o755);
      chmodSync(join(dir, "un"), 0o755);
    }
  }
});

test.skipIf(process.getuid?.() === 0)("unreadable directory edges match across selection", () => {
  const repository = scratchGitRepository();
  const walked = scratch("cli");
  const outside = scratch("cli");
  symlinkSync(join(repository, "pkg"), join(outside, "link"));

  for (const dir of [repository, walked]) {
    for (const path of ["node_modules/x/a.ts", "ns/a.ts", "pkg/un/u.ts", "pkg/ok.ts"]) {
      mkdirSync(join(dir, path, ".."), { recursive: true });
      writeFileSync(join(dir, path), "export const z = 1;\n");
    }

    chmodSync(join(dir, "node_modules", "x"), 0o000);
    chmodSync(join(dir, "pkg", "un"), 0o000);
    chmodSync(join(dir, "ns"), 0o600);
  }

  try {
    for (const dir of [repository, walked])
      expect(run({ cwd: dir }, "--check", ".")).toEqual({
        code: 2,
        stderr:
          "stanza: cannot read ns: permission denied\nstanza: cannot read pkg/un: permission denied\nchecked 1 file, no findings\n",
        stdout: "",
      });

    expect(run({ cwd: outside }, "--check", "link")).toEqual({
      code: 2,
      stderr: "stanza: cannot read link/un: permission denied\nchecked 1 file, no findings\n",
      stdout: "",
    });
  } finally {
    for (const dir of [repository, walked]) {
      chmodSync(join(dir, "node_modules", "x"), 0o755);
      chmodSync(join(dir, "pkg", "un"), 0o755);
      chmodSync(join(dir, "ns"), 0o755);
    }
  }
});

test.skipIf(process.getuid?.() === 0)(
  "a tracked file in a listable but unsearchable directory names it",
  () => {
    const cwd = scratchGitRepository({
      files: { "ns/a.ts": "export const z = 1;\n", "ok.ts": "export const z = 1;\n" },
      staged: true,
    });

    chmodSync(join(cwd, "ns"), 0o600);
    try {
      expect(run({ cwd }, "--check", ".")).toEqual({
        code: 2,
        stderr: "stanza: cannot read ns: permission denied\nchecked 1 file, no findings\n",
        stdout: "",
      });
    } finally {
      chmodSync(join(cwd, "ns"), 0o755);
    }
  },
);

function committed(files: Record<string, string>): string {
  const cwd = scratchGitRepository({ files, staged: true });
  git(cwd, "commit", "-qm", "init");
  return cwd;
}

test.skipIf(process.getuid?.() === 0)(
  "--changed names an unreachable changed file and an unopened folder as path mode does",
  () => {
    const cwd = committed({
      "sub/a.ts": "export const z = 1;\n",
      "ok.ts": "export const z = 1;\n",
    });

    writeFileSync(join(cwd, "sub", "a.ts"), "export const z = 2;\n");
    mkdirSync(join(cwd, "plain"));
    writeFileSync(join(cwd, "plain", "u.ts"), "export const z = 1;\n");

    for (const [folder, mode] of [
      ["sub", 0o644],
      ["plain", 0o000],
    ] as const) {
      chmodSync(join(cwd, folder), mode);
      try {
        const expected = (summary: string) => ({
          code: 2,
          stderr: `stanza: cannot read ${folder}: permission denied\n${summary}\n`,
          stdout: "",
        });

        expect(run({ cwd }, "--check", ".")).toEqual(expected("checked 2 files, no findings"));
        expect(run({ cwd }, "--check", "--changed")).toEqual(
          expected("checked 1 file, no findings"),
        );
      } finally {
        chmodSync(join(cwd, folder), 0o755);
      }
    }
  },
);

test.skipIf(process.getuid?.() === 0)(
  "an unopened folder whose name git masks prints git's name and reason",
  () => {
    const cwd = committed({ "ok.ts": "export const z = 1;\n" });
    const folders = ["plain", "l s", "c\rr"];
    for (const folder of folders) {
      mkdirSync(join(cwd, folder));
      writeFileSync(join(cwd, folder, "u.ts"), "export const z = 1;\n");
      chmodSync(join(cwd, folder), 0o000);
    }

    try {
      const expected = {
        code: 2,
        stderr:
          "stanza: cannot read c?r: permission denied\n" +
          "stanza: cannot read l s: permission denied\n" +
          "stanza: cannot read plain: permission denied\n",
        stdout: "",
      };

      expect(run({ cwd }, "--check", ".")).toEqual({
        ...expected,
        stderr: `${expected.stderr}checked 1 file, no findings\n`,
      });

      expect(run({ cwd }, "--check", "--changed")).toEqual(expected);
    } finally {
      for (const folder of folders) chmodSync(join(cwd, folder), 0o755);
    }
  },
);

test("--changed skips a deleted tracked file and one under a deleted folder in silence", () => {
  const cwd = committed({ "gone/a.ts": "export const z = 1;\n", "b.ts": "export const z = 1;\n" });
  rmSync(join(cwd, "b.ts"));
  rmSync(join(cwd, "gone"), { recursive: true });

  expect(run({ cwd }, "--check", "--changed")).toEqual({
    code: 0,
    stderr: "no changed files to check\n",
    stdout: "",
  });
});

const symlinkLoop = {
  code: 2,
  stderr: "stanza: cannot read lp1.ts: too many levels of symbolic links\n",
  stdout: "",
};

test("a symlink loop is unreadable, not missing", () => {
  const dir = scratch("cli");
  symlinkSync("lp2.ts", join(dir, "lp1.ts"));
  symlinkSync("lp1.ts", join(dir, "lp2.ts"));

  expect(run({ cwd: dir }, "--check", "lp1.ts")).toEqual(symlinkLoop);
});

test("a FIFO named as a path is reported as not a regular file", () => {
  const dir = scratch("cli");
  expect(Bun.spawnSync(["mkfifo", join(dir, "fifo.ts")]).exitCode).toBe(0);
  symlinkSync("fifo.ts", join(dir, "link.ts"));

  for (const name of ["fifo.ts", "link.ts"]) {
    const refused = { code: 2, stderr: `stanza: not a regular file: ${name}\n`, stdout: "" };
    expect(run({ cwd: dir }, "--check", name)).toEqual(refused);
    expect(run({ cwd: dir }, "--fix", name)).toEqual(refused);
    expect(run({ cwd: dir }, "explain", `${name}:1`)).toEqual(refused);
  }

  writeFileSync(join(dir, "a.ts"), bracedReturn);
  expect(run({ cwd: dir }, "--fix", "--braces", "fifo.ts", "a.ts")).toEqual({
    code: 2,
    stderr: "stanza: not a regular file: fifo.ts\nrewrote 1 file, no findings left\n",
    stdout: "",
  });

  expect(readFileSync(join(dir, "a.ts"), "utf8")).toBe(unbracedReturn);
});

test("a symlink to a regular file checks like the file it names", () => {
  const dir = scratch("cli");
  writeFileSync(join(dir, "real.ts"), bracedReturn);
  symlinkSync("real.ts", join(dir, "link.ts"));

  const real = run({ cwd: dir }, "--check", "--braces", "real.ts");
  const linked = run({ cwd: dir }, "--check", "--braces", "link.ts");
  expect(linked.code).toBe(real.code);
  expect(linked.stdout).toBe(real.stdout);
  expect(linked.stderr).toBe(real.stderr);
});

function linkedParentTree(): string {
  const cwd = scratchGitRepository({
    files: { "a.ts": braced("root"), "real/a.ts": singleBlock },
  });

  mkdirSync(join(cwd, "real", "inner"));
  symlinkSync("real/inner", join(cwd, "link"));
  return cwd;
}

test("a path stepping up out of a symlinked folder names the file the system opens", () => {
  for (const absolute of [false, true]) {
    const spelling = (cwd: string) => (absolute ? `${cwd}/link/../a.ts` : "link/../a.ts");
    const checked = linkedParentTree();
    expect(run({ cwd: checked }, "--check", spelling(checked))).toEqual({
      code: 1,
      stdout: bracesAt("real/a.ts"),
      stderr: "1 finding in 1 file, fixable with --fix\n",
    });

    const fixed = linkedParentTree();
    expect(run({ cwd: fixed }, "--fix", spelling(fixed)).code).toBe(0);
    expect(readFileSync(join(fixed, "real", "a.ts"), "utf8")).toBe("if (a) b();\n");
    expect(readFileSync(join(fixed, "a.ts"), "utf8")).toBe(braced("root"));
  }

  expect(run({ cwd: linkedParentTree() }, "--check", "missing/../a.ts")).toEqual({
    code: 2,
    stdout: "",
    stderr: "stanza: no such file: missing/../a.ts\n",
  });
});

test("--stdin steps up out of a symlinked folder, a missing file included", () => {
  const cwd = linkedParentTree();
  writeFileSync(join(cwd, ".gitattributes"), "/a.ts -stanza\n/new.ts -stanza\n");

  const stdin = (path: string) =>
    run({ cwd, stdin: Buffer.from(singleBlock) }, "--check", "--stdin", path);

  const named = stdin("real/new.ts");
  expect(named).toEqual({ code: 1, stdout: bracesAt("real/new.ts"), stderr: "" });
  expect(stdin("link/../new.ts")).toEqual(named);
  expect(stdin("link/../a.ts")).toEqual({ code: 1, stdout: bracesAt("real/a.ts"), stderr: "" });
});

test("--stdin honours linguist-generated for a path whose directory does not exist", () => {
  const dir = scratchGitRepository();
  const source = `export ${hunkFunction("f")}\n`;

  writeFileSync(join(dir, ".gitattributes"), "gen/** linguist-generated\n");

  expect(
    run({ cwd: dir, stdin: Buffer.from(source) }, "--fix", "--stdin", "gen/v2/api.ts"),
  ).toEqual({ code: 0, stderr: "", stdout: source });
});

test("without git the fallback walk stops at the repository above it", () => {
  const cwd = scratch("ignore-boundary");
  const source = "if (ok) {\n  run();\n}\n";

  writeFileSync(join(cwd, ".gitignore"), "project/\nsub/\n");
  mkdirSync(join(cwd, "project/.git"), { recursive: true });
  mkdirSync(join(cwd, "project/sub"));
  writeFileSync(join(cwd, "project/keep.ts"), source);
  writeFileSync(join(cwd, "project/sub/keep.ts"), source);

  const env = { ...process.env, PATH: scratch("no-git") };
  const whole = run({ cwd, env }, "--check", "project");
  const nested = run({ cwd, env }, "--check", "project/sub");

  expect(whole.stdout).toContain("project/keep.ts:");
  expect(whole.stdout).toContain("project/sub/keep.ts:");
  expect(nested.stdout).toContain("project/sub/keep.ts:");
  expect([whole.code, nested.code]).toEqual([1, 1]);
});

test("falls back when git is absent", () => {
  const fixture = join(import.meta.dir, "fixtures");
  const copy = scratch("fixtures");
  const env = { ...process.env, PATH: scratch("no-git") };

  cpSync(fixture, copy, { recursive: true });

  const withGit = run("--check", copy);
  const withoutGit = run({ env }, "--check", copy);

  expect(withoutGit.code).toBe(withGit.code);
  expect(withoutGit.stdout).toBe(withGit.stdout);
  expect(withGit.stderr).toMatch(
    /^\d+ findings in \d+ files: \d+ fixable with --fix, \d+ by hand\n$/,
  );

  expect(withoutGit.stderr.split("\n")).toEqual([
    expect.stringContaining("git"),
    withGit.stderr.trimEnd(),
    "",
  ]);

  const root = join(import.meta.dir, "..");
  const json = run({ cwd: root, env }, "--check", "--json", "scripts");

  expect(() => JSON.parse(json.stdout)).not.toThrow();
  expect(json.stderr.split("\n")).toEqual([expect.stringContaining("git"), ""]);
});

function checkWithoutGit(cwd: string) {
  return run({ cwd, env: { ...process.env, PATH: scratch("no-git") } }, "--check", ".");
}

test.skipIf(process.getuid?.() === 0)(
  "the walk without git still reports an unreadable regular .gitignore",
  () => {
    const cwd = scratchGitRepository({ files: { "a.ts": braced("a"), ".gitignore": "a.ts\n" } });
    chmodSync(join(cwd, ".gitignore"), 0);

    try {
      const result = checkWithoutGit(cwd);
      expect(result.code).toBe(2);
      expect(result.stderr).toContain("stanza: cannot read .gitignore: permission denied\n");
    } finally {
      chmodSync(join(cwd, ".gitignore"), 0o644);
    }
  },
);

test("the walk without git skips a linked or FIFO .gitignore", () => {
  const kinds = {
    link: (cwd: string) => symlinkSync("rules", join(cwd, ".gitignore")),
    "link to a folder": (cwd: string) => {
      mkdirSync(join(cwd, "folder"));
      symlinkSync("folder", join(cwd, ".gitignore"));
    },
    "link to a FIFO": (cwd: string) => {
      expect(Bun.spawnSync(["mkfifo", join(cwd, "pipe")]).exitCode).toBe(0);
      symlinkSync("pipe", join(cwd, ".gitignore"));
    },
    FIFO: (cwd: string) =>
      expect(Bun.spawnSync(["mkfifo", join(cwd, ".gitignore")]).exitCode).toBe(0),
  };

  for (const [kind, make] of Object.entries(kinds)) {
    const cwd = scratchGitRepository({ files: { "a.ts": braced("a"), rules: "a.ts\n" } });
    make(cwd);

    const result = checkWithoutGit(cwd);
    const findings = result.stdout.split("\n").filter(Boolean);
    expect({
      kind,
      code: result.code,
      findings: findings.map((line) => line.split(": ")[0]),
    }).toEqual({
      kind,
      code: 1,
      findings: ["a.ts:1:8"],
    });

    expect(findings[0]).toContain(": braces: ");
  }
});

test("a git failure names its folder from the working directory and the index as git does", () => {
  const cwd = scratchGitRepository({
    files: { "sub/a.ts": "export const a = 1;\n" },
    staged: true,
  });

  git(cwd, "commit", "-qm", "init");
  writeFileSync(join(cwd, "sub", "a.ts"), "export const a = 2;\n");
  const index = join(cwd, ".git", "index");
  writeFileSync(index, readFileSync(index).subarray(0, 12));

  const fatal = "fatal: .git/index: index file smaller than expected";
  expect(run({ cwd }, "--check", "--changed")).toEqual({
    code: 2,
    stdout: "",
    stderr: `stanza: git diff failed: ${fatal}\nstanza: git ls-files failed: ${fatal}\n`,
  });

  const fromSub = run({ cwd: join(cwd, "sub") }, "--check", "--changed");
  expect(fromSub.code).toBe(2);
  expect(fromSub.stderr.split("\n")[0]).toBe(`stanza: git diff failed in ..: ${fatal}`);

  expect(run({ cwd: join(cwd, "sub") }, "--check", "--staged")).toEqual({
    code: 2,
    stdout: "",
    stderr: `stanza: git diff failed in ..: ${fatal}\n`,
  });

  const walked = run({ cwd }, "--check", ".");
  expect(walked.code).toBe(2);
  expect(walked.stderr.split("\n")[0]).toStartWith(`stanza: git ls-files failed: ${fatal}`);
});

test("--changed without git exits 2 naming git", () => {
  const env = { ...process.env, PATH: scratch("no-git") };

  const root = join(import.meta.dir, "..");
  const changed = run({ cwd: root, env }, "--check", "--changed");

  expect(changed.code).toBe(2);
  expect(changed.stderr.split("\n")).toEqual([expect.stringContaining("git"), ""]);
  expect(changed.stderr).not.toContain("at ");

  const staged = run({ cwd: root, env }, "--check", "--staged");
  expect(staged).toEqual({
    code: 2,
    stdout: "",
    stderr: "stanza: --staged needs git, which was not found on PATH\n",
  });
});

for (const args of [
  ["--check", "--changed"],
  ["--check", "--staged"],
])
  test(`${args.join(" ")} outside a repository exits 2 naming it`, () => {
    expect(run({ cwd: scratch("outside") }, ...args)).toEqual({
      code: 2,
      stdout: "",
      stderr: "stanza: not inside a git repository\n",
    });
  });

for (const args of [
  ["--check", "."],
  ["--check", "a.ts"],
  ["--check", "--changed"],
  ["--check", "--staged"],
])
  test(`git refusing an existing repository exits 2 instead of walking it: ${args.join(" ")}`, () => {
    const dir = scratchGitRepository({ files: { "a.ts": "export const a = 1;\n" } });
    const result = run({ cwd: dir, env: gitRefusesOwnership() }, ...args);
    expect(result.code).toBe(2);
    expect(result.stderr).toContain("dubious ownership");
  });

test("a repository hidden by GIT_CEILING_DIRECTORIES falls back to the walk", () => {
  const dir = realpathSync(scratchGitRepository());
  mkdirSync(join(dir, "sub"));
  writeFileSync(join(dir, "sub", "dirty.ts"), `export ${hunkFunction("f")}\n`);

  const env = { ...process.env, GIT_CEILING_DIRECTORIES: dir };
  const result = run({ cwd: join(dir, "sub"), env }, "--check", ".");
  expect(result.code).toBe(1);
  expect(result.stdout).toContain("dirty.ts");
});

const braced = (name: string) => `if (${name}) {\n  ${name}();\n}\n`;

const bracesAt = (path: string) =>
  `${path}:1:8: braces: remove the braces around this single statement body\n`;
