import { describe, expect, test } from "bun:test";
import { chmodSync, mkdirSync, readFileSync, rmSync, symlinkSync, writeFileSync } from "node:fs";
import { join, resolve } from "node:path";
import {
  colorless,
  git,
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
