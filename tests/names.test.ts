import { expect, test } from "bun:test";
import { chmodSync, mkdirSync, readFileSync, rmSync, symlinkSync, writeFileSync } from "node:fs";
import { join } from "node:path";
import {
  git,
  gitBinary,
  inDirectory,
  run,
  scratch,
  scratchGitRepository,
  shell,
} from "./support.ts";

const singleBlock = "if (a) {\n  b();\n}\n";

const bracesAt = (path: string) =>
  `${path}:1:8: braces: remove the braces around this single statement body\n`;

const onLinux = test.skipIf(process.platform !== "linux");

const raw = (path: string) => Buffer.from(path, "latin1");

const notUtf8 = (name: string) => `stanza: name is not valid UTF-8: "${name}"\n`;

const oneFixable = "1 finding in 1 file, fixable with --fix\n";

function failingCheckAttr(): string {
  const bin = scratch("check-attr-shim");
  const executable = Bun.which(gitBinary);
  if (!executable) throw new Error("Git executable not found");

  writeFileSync(
    join(bin, "git"),
    `#!/bin/sh\nfor arg in "$@"; do\n  if [ "$arg" = check-attr ]; then\n    echo 'forced attribute failure' >&2\n    exit 128\n  fi\ndone\nexec '${executable}' "$@"\n`,
  );

  chmodSync(join(bin, "git"), 0o755);
  return bin;
}

const attributeFailure = {
  code: 2,
  stderr: "stanza: git check-attr failed: forced attribute failure\n",
  stdout: "",
};

test("a relative name that is not UTF-8 needs the working directory too", () => {
  const directory = scratch("deleted-cwd-raw");

  const fromDeleted = (...names: string[]) => {
    const gone = join(directory, `gone-${Math.random().toString(36).slice(2)}`);
    mkdirSync(gone);
    const raw = names.map((name) => `"$(printf '${name}')"`).join(" ");
    return shell(`cd "${gone}" && rmdir "${gone}" && exec "$1" --check ${raw}`, "");
  };

  const cwdGone = {
    code: 2,
    stderr: "stanza: cannot read the working directory: no such file or directory\n",
    stdout: "",
  };

  expect(fromDeleted("b\\377.ts")).toEqual(cwdGone);
  expect(fromDeleted("/a.ts", "b\\377.ts")).toEqual(cwdGone);

  expect(fromDeleted("/b\\377.ts")).toEqual({
    code: 2,
    stderr: 'stanza: name is not valid UTF-8: "/b\\377.ts"\n',
    stdout: "",
  });
});

onLinux("every selection names a file that is not UTF-8 the way git quotes it", () => {
  const cwd = scratchGitRepository();
  mkdirSync(raw(join(cwd, "d\xff")));
  const names = ["bad\xff.ts", "d\xff/bad\xff.ts", "bad\xef\xbf\xbd.ts"];
  for (const name of names) writeFileSync(raw(join(cwd, name)), "export {};\n");

  git(cwd, "add", "-A");
  git(cwd, "commit", "-qm", "init");
  for (const name of names) writeFileSync(raw(join(cwd, name)), singleBlock);

  const sibling = bracesAt("bad\u{fffd}.ts");
  const reported = notUtf8("bad\\377.ts") + notUtf8("d\\377/bad\\377.ts");
  for (const source of ["--check .", "--check --changed"])
    expect(inDirectory(cwd, `exec "$1" ${source}`)).toEqual({
      code: 2,
      stderr: reported + oneFixable,
      stdout: sibling,
    });

  expect(inDirectory(cwd, `exec "$1" --check "$(printf 'd\\377')"`)).toEqual({
    code: 2,
    stderr: notUtf8("d\\377"),
    stdout: "",
  });

  git(cwd, "add", "-A");
  const repair = `fix and restage with: cd ${cwd} && stanza --fix -- 'bad\u{fffd}.ts' && git --literal-pathspecs add -- 'bad\u{fffd}.ts'\n`;
  expect(inDirectory(cwd, `exec "$1" --check --staged`)).toEqual({
    code: 2,
    stderr: `${reported}1 finding in 1 staged file, fixable\n${repair}`,
    stdout: sibling,
  });

  const stop = JSON.stringify({ cwd, hook_event_name: "Stop" });
  expect(inDirectory(cwd, `exec "$1" hook`, stop)).toEqual({
    code: 1,
    stderr: reported.replaceAll("stanza:", "stanza hook:"),
    stdout: "",
  });

  expect(readFileSync(raw(join(cwd, "bad\xef\xbf\xbd.ts")), "utf8")).toBe("if (a) b();\n");

  for (const name of names.slice(0, 2))
    expect(readFileSync(raw(join(cwd, name)), "utf8")).toBe(singleBlock);
});

onLinux("a name that is not UTF-8 under a folder replaced by a symlink is skipped", () => {
  const cwd = scratchGitRepository();
  writeFileSync(join(cwd, ".gitignore"), "hidden/\n");
  writeFileSync(join(cwd, "keep.ts"), "export {};\n");
  mkdirSync(join(cwd, "src"));
  writeFileSync(raw(join(cwd, "src/bad\xff.ts")), "export {};\n");

  git(cwd, "add", "-A");
  git(cwd, "commit", "-qm", "init");

  rmSync(join(cwd, "src"), { recursive: true });
  mkdirSync(join(cwd, "hidden"));
  writeFileSync(raw(join(cwd, "hidden/bad\xff.ts")), singleBlock);
  symlinkSync("hidden", join(cwd, "src"));

  for (const source of ["--check .", "--check --changed"]) {
    const result = inDirectory(cwd, `exec "$1" ${source}`);
    expect(result.stderr).not.toContain("not valid UTF-8");
    expect(result.code).toBe(0);
  }
});

onLinux("an argument that is not UTF-8 is named and never read as its lossy image", () => {
  const cwd = scratch("argv-names");
  writeFileSync(raw(join(cwd, "\xff.ts")), singleBlock);
  writeFileSync(raw(join(cwd, "\xef\xbf\xbd.ts")), singleBlock);
  writeFileSync(join(cwd, "ok.ts"), singleBlock);
  symlinkSync(raw("\xff.ts"), join(cwd, "link.ts"));

  const named = notUtf8("\\377.ts");
  expect(inDirectory(cwd, `exec "$1" --check ok.ts "$(printf '\\377.ts')"`)).toEqual({
    code: 2,
    stderr: named + oneFixable,
    stdout: bracesAt("ok.ts"),
  });

  expect(inDirectory(cwd, `exec "$1" --check -- "$(printf '\\377.ts')"`)).toEqual({
    code: 2,
    stderr: named,
    stdout: "",
  });

  expect(inDirectory(cwd, `exec "$1" --check link.ts ok.ts`)).toEqual({
    code: 2,
    stderr: named + oneFixable,
    stdout: bracesAt("ok.ts"),
  });

  for (const script of [
    `exec "$1" --check --stdin "$(printf '\\377.ts')"`,
    `exec "$1" explain "$(printf '\\377.ts')"`,
  ]) {
    const result = inDirectory(cwd, script, singleBlock);
    expect(result.stderr).toStartWith(named);
    expect(result.stdout).toBe("");
    expect(result.code).toBe(2);
  }
});

onLinux("a working directory whose name is not UTF-8 is never resolved as its lossy image", () => {
  const parent = scratch("cwd-names");
  for (const directory of ["d\xff", "d\xef\xbf\xbd"]) {
    mkdirSync(raw(join(parent, directory)));
    writeFileSync(raw(join(parent, directory, "a.ts")), singleBlock);
  }

  const inside = (args: string) =>
    shell(`cd "$(printf '%s/d\\377' "${parent}")" && exec "$1" ${args}`, "");

  expect(inside("--check a.ts")).toEqual({
    code: 2,
    stderr: `stanza: cannot use the working directory, its name is not valid UTF-8: "${parent}/d\\377"\n`,
    stdout: "",
  });

  const sibling = join(parent, "d\u{fffd}", "a.ts");
  expect(inside(`--check "${sibling}"`)).toEqual({
    code: 1,
    stderr: oneFixable,
    stdout: bracesAt(sibling),
  });
});

onLinux("the directory walk names files below a name that is not UTF-8", () => {
  const cwd = scratch("walk-names");
  mkdirSync(raw(join(cwd, "d\xff", "node_modules")), { recursive: true });

  const files = [
    "bad\xff.ts",
    "d\xff/x.ts",
    "d\xff/node_modules/y.ts",
    "caf\xe9.md",
    "bad\xef\xbf\xbd.ts",
  ];

  for (const name of files) writeFileSync(raw(join(cwd, name)), singleBlock);

  expect(inDirectory(cwd, `exec "$1" --check .`)).toEqual({
    code: 2,
    stderr: notUtf8("bad\\377.ts") + notUtf8("d\\377/x.ts") + oneFixable,
    stdout: bracesAt("bad\u{fffd}.ts"),
  });
});

onLinux("a path leading to a name that is not UTF-8 is named while the rest still runs", () => {
  const cwd = scratch("alias-names");
  mkdirSync(raw(join(cwd, "d\xff")));
  writeFileSync(raw(join(cwd, "d\xff", "x.ts")), singleBlock);
  writeFileSync(join(cwd, "ok.ts"), singleBlock);
  symlinkSync(raw("d\xff"), join(cwd, "alias"));

  for (const target of ["alias/x.ts", "alias"])
    expect(inDirectory(cwd, `exec "$1" --check ${target} ok.ts`)).toEqual({
      code: 2,
      stderr: notUtf8(target === "alias" ? "d\\377" : "d\\377/x.ts") + oneFixable,
      stdout: bracesAt("ok.ts"),
    });
});

onLinux("ignore files still apply below a name that is not UTF-8", () => {
  const cwd = scratch("walk-ignores");
  mkdirSync(raw(join(cwd, "d\xff", "gen")), { recursive: true });
  mkdirSync(raw(join(cwd, "e\xff", ".gitignore")), { recursive: true });
  writeFileSync(join(cwd, ".gitignore"), "gen/\nskip?.ts\nz???.ts\n");
  writeFileSync(raw(join(cwd, "d\xff", ".gitignore")), "local.ts\n");

  const files = ["d\xff/x.ts", "d\xff/local.ts", "d\xff/gen/y.ts", "skip\xff.ts", "z\xff.ts"];
  for (const name of files) writeFileSync(raw(join(cwd, name)), singleBlock);

  expect(inDirectory(cwd, `exec "$1" --check .`)).toEqual({
    code: 2,
    stderr:
      'stanza: cannot read "e\\377/.gitignore": is a directory\n' +
      notUtf8("d\\377/x.ts") +
      notUtf8("z\\377.ts"),
    stdout: "",
  });
});

onLinux("the stanza attribute opts out a name that is not UTF-8", () => {
  const cwd = scratchGitRepository();
  mkdirSync(join(cwd, "o"));
  writeFileSync(join(cwd, ".gitattributes"), "opt*.ts -stanza\n");

  for (const name of ["opt\xff.ts", "o/opt\xff.ts", "bad\xff.ts"])
    writeFileSync(raw(join(cwd, name)), singleBlock);

  git(cwd, "add", "-A");

  for (const source of [
    "--check .",
    "--check --changed",
    "--check --staged",
    `--check . "$(printf 'bad\\377.ts')" "$(printf 'opt\\377.ts')"`,
  ])
    expect(inDirectory(cwd, `exec "$1" ${source}`)).toEqual({
      code: 2,
      stderr: notUtf8("bad\\377.ts"),
      stdout: "",
    });

  const optedOut = (shown: string) =>
    `stanza: nothing to format in ${shown}, the stanza attribute opts its files out\n`;

  expect(inDirectory(cwd, `exec "$1" --check "$(printf 'opt\\377.ts')"`)).toEqual({
    code: 0,
    stderr: optedOut('"opt\\377.ts"'),
    stdout: "",
  });

  expect(inDirectory(cwd, `exec "$1" --check o`)).toEqual({
    code: 0,
    stderr: optedOut("o"),
    stdout: "",
  });
});

onLinux("named files that are not UTF-8 follow the selection rules", () => {
  const cwd = scratchGitRepository();
  mkdirSync(join(cwd, "g"));
  writeFileSync(join(cwd, ".gitattributes"), "opt*.ts -stanza\n");
  symlinkSync(raw("opt\xff.ts"), join(cwd, "alias.ts"));

  for (const name of ["opt\xff.ts", "bad\xff.ts", "bad\xff.gen.ts", "g/bad\xff.gen.ts"])
    writeFileSync(raw(join(cwd, name)), singleBlock);

  const skipped = (shown: string, why: string) => `stanza: nothing to format in ${shown}, ${why}\n`;

  expect(inDirectory(cwd, `exec "$1" --check alias.ts`)).toEqual({
    code: 0,
    stderr: skipped("alias.ts", "the stanza attribute opts its files out"),
    stdout: "",
  });

  symlinkSync(raw("bad\xff.ts"), join(cwd, "alias.gen.ts"));
  expect(inDirectory(cwd, `exec "$1" --check alias.gen.ts`)).toEqual({
    code: 0,
    stderr: skipped("alias.gen.ts", "generated files are skipped"),
    stdout: "",
  });

  expect(inDirectory(cwd, `exec "$1" --check g`)).toEqual({
    code: 0,
    stderr: skipped("g", "generated files are skipped"),
    stdout: "",
  });

  expect(inDirectory(cwd, `exec "$1" --check "$(printf 'bad\\377.gen.ts')"`).code).toBe(0);

  const spellings = `. "$(printf './bad\\377.ts')" "$(printf '${cwd}/bad\\377.ts')"`;
  expect(inDirectory(cwd, `exec "$1" --check ${spellings}`)).toEqual({
    code: 2,
    stderr: notUtf8("bad\\377.ts"),
    stdout: "",
  });

  const outside = scratch("ignore-bytes");
  writeFileSync(join(outside, ".gitignore"), raw("bad\xff.ts\n"));
  writeFileSync(raw(join(outside, "bad\xff.ts")), singleBlock);
  writeFileSync(raw(join(outside, "bad\xfe.ts")), singleBlock);
  expect(inDirectory(outside, `exec "$1" --check .`)).toEqual({
    code: 2,
    stderr: notUtf8("bad\\376.ts"),
    stdout: "",
  });
});

test("check-attr failure wins over invalid names", () => {
  const cwd = scratchGitRepository();
  git(cwd, "commit", "--allow-empty", "-qm", "base");

  const stdin = Buffer.from("export {};\n");
  const blob = git(cwd, { stdin }, "hash-object", "-w", "--stdin").trim();
  const entry = Buffer.concat([Buffer.from(`100644 ${blob}\t`), raw("bad\xff.ts\0")]);
  git(cwd, { stdin: entry }, "update-index", "-z", "--index-info");

  const env = { PATH: `${failingCheckAttr()}:${process.env.PATH}` };
  expect(run({ cwd, env }, "--check", "--staged")).toEqual(attributeFailure);
  expect(run({ cwd }, "--check", "--staged")).toEqual({
    code: 2,
    stderr: notUtf8("bad\\377.ts"),
    stdout: "",
  });

  writeFileSync(join(cwd, "good.ts"), "export {};\n");
  git(cwd, "add", "good.ts");
  expect(run({ cwd, env }, "--check", "--staged")).toEqual(attributeFailure);
  expect(run({ cwd }, "--check", "--staged")).toEqual({
    code: 2,
    stderr: `${notUtf8("bad\\377.ts")}checked 1 staged file, no findings\n`,
    stdout: "",
  });
});

onLinux("check-attr failure wins over invalid names in every selection", () => {
  const cwd = scratchGitRepository();
  git(cwd, "commit", "--allow-empty", "-qm", "base");

  writeFileSync(raw(join(cwd, "bad\xff.ts")), singleBlock);
  git(cwd, "add", "-A");
  git(cwd, "commit", "-qm", "init");
  writeFileSync(raw(join(cwd, "bad\xff.ts")), `${singleBlock}\n`);

  const bin = failingCheckAttr();
  const check = (selection: string) =>
    inDirectory(cwd, `PATH="${bin}:$PATH" exec "$1" --check ${selection}`);

  const named = `"$(printf 'bad\\377.ts')"`;
  for (const selection of [".", "--changed", named])
    expect(check(selection), selection).toEqual(attributeFailure);

  writeFileSync(join(cwd, "good.ts"), "export {};\n");

  // good.ts reaches the shim's check-attr only when git's attribute metadata or a GIT_ variable turns the precheck off.
  const siblingChecked = `${attributeFailure.stderr}checked 1 file, no findings\n`;
  for (const selection of [".", "--changed", `good.ts ${named}`]) {
    const { code, stderr, stdout } = check(selection);
    expect({ code, stdout }, selection).toEqual({ code: 2, stdout: "" });
    expect([attributeFailure.stderr, siblingChecked], selection).toContain(stderr);
  }
});

test("a selection failure two names share prints once", () => {
  const cwd = scratchGitRepository();
  const names = `"$(printf 'a\\377.ts')" "$(printf 'b\\377.ts')"`;
  const script = `PATH="${failingCheckAttr()}:$PATH" exec "$1" --check ${names}`;
  expect(inDirectory(cwd, script)).toEqual(attributeFailure);
});
