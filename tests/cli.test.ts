import { expect, test } from "bun:test";
import {
  cpSync,
  chmodSync,
  linkSync,
  lstatSync,
  mkdirSync,
  readFileSync,
  readdirSync,
  realpathSync,
  rmSync,
  statSync,
  symlinkSync,
  writeFileSync,
} from "node:fs";
import { join, relative } from "node:path";
import catalog from "../docs/lib/catalog.json";
import { version } from "../package.json" with { type: "json" };
import { cpuMs, cpuSpent } from "./budget.ts";
import {
  colorless,
  findingRow,
  git,
  gitBinary,
  hunkFunction,
  run,
  scratch,
  scratchGitRepository,
  superproject,
} from "./support.ts";

const gitRefusesOwnership = {
  ...process.env,
  GIT_CONFIG_GLOBAL: "/dev/null",
  GIT_CONFIG_NOSYSTEM: "1",
  GIT_TEST_ASSUME_DIFFERENT_OWNER: "1",
};

test("--help prints every hook registration and the rule catalog", () => {
  const { registrations, rules } = catalog;
  const result = run("--help");
  expect(result.code).toBe(0);
  expect(result.stderr).toBe("");

  const lines = result.stdout.split("\n");
  for (const [client, { files, settings }] of Object.entries(registrations)) {
    const heading = lines.indexOf(`${client} ${files.join(" or ")}:`);
    expect(heading, client).toBeGreaterThanOrEqual(0);

    const end = lines.indexOf("", heading + 1);
    expect(JSON.parse(lines.slice(heading + 1, end).join("\n"))).toEqual(settings);
  }

  for (const rule of Object.keys(rules)) expect(result.stdout).toContain(rule);
});

test("--version prints the source version", () => {
  const result = run("--version");
  expect(result.code).toBe(0);
  expect(result.stderr).toBe("");
  expect(result.stdout).toBe(`stanza ${version}\n`);
});

test("missing paths use the supplied spelling", () => {
  const cwd = scratch("missing");

  expect(run({ cwd }, "--check", "nope.ts")).toEqual({
    code: 2,
    stderr: "stanza: no such file: nope.ts\n",
    stdout: "",
  });
});

const oneFinding = "f(\n  1,\n);\ng();\n";

test("files print in the byte order of their paths, as git lists them", () => {
  const cwd = scratch("byte-order");
  for (const name of ["api.ts", "Button.ts", "_x.ts", "é.ts", "10.ts", "9.ts"])
    writeFileSync(join(cwd, name), oneFinding);

  const order = ["10.ts", "9.ts", "Button.ts", "_x.ts", "api.ts", "é.ts"];
  const checked = run({ cwd }, "--check", ".");
  const json = run({ cwd }, "--check", "--json", ".");

  expect(
    checked.stdout
      .split("\n")
      .filter(Boolean)
      .map((row) => row.split(":")[0]),
  ).toEqual(order);

  expect(JSON.parse(json.stdout).map((finding: { path: string }) => finding.path)).toEqual(order);
});

test("the findings of two paths never interleave, even when they differ by an invisible character", () => {
  const cwd = scratch("interleave");
  const softHyphen = "\u00ad";
  writeFileSync(join(cwd, "a.ts"), `${oneFinding}\n${oneFinding}`);
  writeFileSync(join(cwd, `a${softHyphen}.ts`), `// x\n${oneFinding}\n${oneFinding}`);

  const rows = run({ cwd }, "--check", ".").stdout.split("\n").filter(Boolean);

  expect(rows.map((row) => row.split(": ")[0])).toEqual([
    "a.ts:4:1",
    "a.ts:9:1",
    `a${softHyphen}.ts:5:1`,
    `a${softHyphen}.ts:10:1`,
  ]);
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

      expect(run({ cwd }, "--check", ".").stderr).toBe(lines);
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

test("explain names a missing path with the supplied spelling", () => {
  const cwd = scratch("missing");
  expect(run({ cwd }, "explain", "nope.ts:3")).toEqual({
    code: 2,
    stderr: "stanza: no such file: nope.ts\n",
    stdout: "",
  });
});

test("fallback selection handles question marks in ignore patterns", () => {
  const cwd = scratch("ignore-question");
  writeFileSync(join(cwd, ".gitignore"), "/?.ts\n");
  const source = "if (ok) {\n  run();\n}\n";
  writeFileSync(join(cwd, "a.ts"), source);
  writeFileSync(join(cwd, "ab.ts"), source);

  const result = run({ cwd }, "--check", ".");
  expect(result.code).toBe(1);
  expect(result.stdout).toContain("ab.ts");
  expect(result.stdout).not.toContain("a.ts:");
  expect(result.stderr).toBe("1 finding in 1 file, fixable with --fix\n");
});

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

test("do not edit comments still report findings", () => {
  const cwd = scratch("header-comments");
  const source = `${hunkFunction("f")}\n`;
  for (const [index, header] of [
    "// Do not edit the order below",
    "// TODO: do not edit this until the API settles",
  ].entries())
    for (const line of [1, 2]) {
      const path = `comment-${index}-${line}.ts`;
      const prefix = line === 2 ? "export const note = 1;\n" : "";
      writeFileSync(join(cwd, path), `${prefix}${header}\n${source}`);

      const result = run({ cwd }, "--check", path);
      expect(result.code).toBe(1);
      expect(result.stderr).toBe("1 finding in 1 file, fixable with --fix\n");
      expect(result.stdout).toContain(": braces: ");
    }
});

test("generated header comments skip explicit files", () => {
  const cwd = scratch("generated-headers");
  const source = `${hunkFunction("f")}\n`;
  const headers = [
    "// Code generated by protoc-gen-go. DO NOT EDIT.",
    "// @generated",
    "/* eslint-disable */\n// This file was automatically generated. Do not edit.",
    "// THIS FILE IS AUTOMATICALLY GENERATED. DO NOT EDIT.",
    "/**\n * This file was auto-generated by openapi-typescript.\n * Do not make direct changes to the file.\n */",
  ];

  for (const [index, header] of headers.entries()) {
    const path = `generated-${index}.ts`;
    writeFileSync(join(cwd, path), `${header}\n${source}`);

    expect(run({ cwd }, "--check", path)).toEqual({
      code: 0,
      stderr: `stanza: nothing to format in ${path}, generated files are skipped\n`,
      stdout: "",
    });
  }
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

test("--hunks keeps an opted in file below a skipped folder to its changed lines", () => {
  const source = (f: number, g: number) => `${hunkFunction("f", f)}\n\n${hunkFunction("g", g)}\n`;

  const cwd = committed({
    ".gitattributes": "dist/** stanza\n",
    "dist/a.ts": source(2, 2),
    "src/z.ts": source(2, 2),
  });

  writeFileSync(join(cwd, "dist", "a.ts"), source(2, 3));
  writeFileSync(join(cwd, "src", "z.ts"), source(3, 2));
  git(cwd, "add", "dist/a.ts", "src/z.ts");

  const braces = "braces: remove the braces around this single statement body\n";
  const findings = `dist/a.ts:9:10: ${braces}src/z.ts:2:10: ${braces}`;
  for (const source of ["--changed", "--staged"])
    expect(run({ cwd }, "--check", source, "--hunks").stdout).toBe(findings);
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

test("explain names the invalid line using the supplied path", () => {
  const result = run("explain", "a.ts:0");
  expect(result.code).toBe(2);
  expect(result.stderr).toStartWith("stanza: a.ts:0: the line must be 1 or more\n");
});

test("explain picks the language from a symlink target, as --check does", () => {
  const dir = scratch("cli");
  writeFileSync(join(dir, "target.js"), "const x: number = 1;\n");
  symlinkSync("target.js", join(dir, "link.ts"));

  const result = run({ cwd: dir }, "explain", "link.ts:1");
  expect(result.code).toBe(2);
  expect(result.stderr).toStartWith("stanza: target.js:1 does not parse:");
  expect(run({ cwd: dir }, "--check", "link.ts").code).toBe(2);
});

test("explain refuses a path through a file with the line --check prints", () => {
  const cwd = scratchGitRepository({ files: { "dirty.ts": singleBlock } });
  const missing = { code: 2, stderr: "stanza: no such file: dirty.ts/x.ts\n", stdout: "" };
  expect(run({ cwd }, "--check", "dirty.ts/x.ts")).toEqual(missing);
  expect(run({ cwd }, "explain", "dirty.ts/x.ts:1")).toEqual(missing);
});

test.each([
  [["--chek", "src"], "unknown flag --chek, did you mean --check?"],
  [["--check", "--jsno", "src"], "unknown flag --jsno, did you mean --json?"],
  [["--wat", "src"], "unknown flag --wat"],
  [["--check", "--verbose", "a.ts"], "unknown flag --verbose"],
  [["--check", "--fix", "src"], "use one of --fix or --check"],
  [["--fix", "--fix", "src"], "--fix given twice"],
  [["--check", "--json", "--json", "src"], "--json given twice"],
  [["--check", "-h", "src"], "-h takes no other arguments"],
  [["--che\nk", "src"], 'unknown flag "--che\\nk", did you mean --check?'],
])("a usage error names the problem and points at --help: %j", (args, problem) => {
  expect(run(...args)).toEqual({
    code: 2,
    stderr: `stanza: ${problem}\nrun \`stanza --help\` for usage\n`,
    stdout: "",
  });
});

test("--help, -h and --version beside other arguments are usage errors", () => {
  for (const flag of ["--help", "-h", "--version"]) {
    const result = run("--check", flag, "a.ts");
    expect(result.code).toBe(2);
    expect(result.stdout).toBe("");
    expect(result.stderr).toContain(`${flag} takes no other arguments`);
  }
});

test("a file that is not UTF-8 is reported and left untouched", () => {
  const dir = scratch("cli");
  const file = join(dir, "latin1.ts");
  const bytes = Buffer.from('function f(a) {\n  if (a) {\n    log("caf\xe9");\n  }\n}\n', "latin1");
  writeFileSync(file, bytes);

  const result = run("--fix", file);
  expect(result.code).toBe(2);
  expect(result.stdout).toContain(": parse: not valid UTF-8");
  expect(readFileSync(file)).toEqual(bytes);
});

test("a file with lone CR line endings is reported and left untouched", () => {
  const source = "if (a) {\r  x();\r}\r";
  const piped = run({ stdin: Buffer.from(source) }, "--fix", "--stdin", "x.ts", "--braces");
  expect(piped).toEqual({
    code: 2,
    stdout: source,
    stderr: "x.ts:1:9: parse: CR line endings are not supported, left untouched\n",
  });

  const dir = scratch("cli");
  writeFileSync(join(dir, "cr.ts"), source);
  writeFileSync(join(dir, "lf.ts"), "if (a) {\n  x();\n}\n");

  const checked = run({ cwd: dir }, "--check", "--braces", "cr.ts", "lf.ts");
  expect(checked.code).toBe(2);
  expect(checked.stdout.split("\n").filter(Boolean)).toEqual([
    "cr.ts:1:9: parse: CR line endings are not supported, left untouched",
    "lf.ts:1:8: braces: remove the braces around this single statement body",
  ]);

  const fixed = run({ cwd: dir }, "--fix", "--braces", "cr.ts", "lf.ts");
  expect(fixed.code).toBe(2);
  expect(readFileSync(join(dir, "cr.ts"), "utf8")).toBe(source);
  expect(readFileSync(join(dir, "lf.ts"), "utf8")).toBe("if (a) x();\n");

  const mixed = "if (a) {\n  x();\r}\n";
  expect(run({ stdin: Buffer.from(mixed) }, "--fix", "--stdin", "x.ts", "--braces")).toEqual({
    code: 2,
    stdout: mixed,
    stderr: "x.ts:2:7: parse: CR line endings are not supported, left untouched\n",
  });
});

const nestedArrays = (n: number) => `const a = ${"[".repeat(n)}0${"]".repeat(n)};\n`;

const MILLION_DEEP: [string, (depth: number) => string][] = [
  ["arrays.ts", nestedArrays],
  ["parens.ts", (n) => `const a = ${"(".repeat(n)}0${")".repeat(n)};\n`],
  ["ifs.ts", (n) => `${"if (a) {\n".repeat(n)}b();\n${"}\n".repeat(n)}`],
  ["objects.ts", (n) => `const a = ${"{ a: ".repeat(n)}0${" }".repeat(n)};\n`],
  ["templates.ts", (n) => `const a = ${"`${".repeat(n)}0${"}`".repeat(n)};\n`],
  ["elements.tsx", (n) => `const a = ${"<a>".repeat(n)}x${"</a>".repeat(n)};\n`],
  ["bangs.ts", (n) => `const a = ${"!".repeat(n)}b;\n`],
];

test("a file nested a million levels deep is reported and left untouched", () => {
  const dir = scratch("deep");
  for (const [name, nested] of MILLION_DEEP) {
    const file = join(dir, name);
    const source = nested(1_000_000);
    writeFileSync(file, source);

    const { value: result, ms } = cpuSpent(() => run({ cwd: dir }, "--fix", name));
    expect(result.code).toBe(2);
    expect(result.stdout).toMatch(
      new RegExp(
        `^${name.replace(".", "\\.")}:\\d+:\\d+: parse: nests more than 10000 levels deep, left untouched\\n$`,
      ),
    );

    expect(ms).toBeLessThan(1_000);
    expect(readFileSync(file, "utf8")).toBe(source);
    rmSync(file);
  }

  writeFileSync(join(dir, "arrays.ts"), nestedArrays(1_000_000));

  for (const fixable of ["a.ts", "b.ts", "c.ts"])
    writeFileSync(join(dir, fixable), "if (a) {\n  x();\n}\n");

  const fixed = run({ cwd: dir }, "--fix", "--braces", ".");
  expect(fixed.code).toBe(2);
  expect(fixed.stdout).toContain("arrays.ts:");

  for (const fixable of ["a.ts", "b.ts", "c.ts"])
    expect(readFileSync(join(dir, fixable), "utf8")).toBe("if (a) x();\n");
}, 60_000);

test("a lint config nested a million levels deep leaves the run going", () => {
  const cwd = scratchGitRepository({
    files: {
      "eslint.config.js": `export default ${"[".repeat(1_000_000)}{}${"]".repeat(1_000_000)};\n`,
      "a.ts": "if (a) {\n  x();\n}\n",
    },
  });

  const result = run({ cwd }, "--check", "a.ts");
  expect(result.code).toBe(0);
  expect(result.stderr).toContain("cannot tell whether eslint.config.js enforces braces");
}, 60_000);

test("a CR byte inside a template literal of an LF file is not a line ending", () => {
  const fixtures = join(import.meta.dir, "fixtures", "braces");
  const before = readFileSync(join(fixtures, "template-cr.before.ts"));
  const after = readFileSync(join(fixtures, "template-cr.after.ts"), "utf8");
  expect(run({ stdin: before }, "--fix", "--stdin", "x.ts", "--braces")).toEqual({
    code: 0,
    stdout: after,
    stderr: "",
  });

  const unterminated = Buffer.from("const t = `a\rb`;");
  expect(run({ stdin: unterminated }, "--check", "--stdin", "x.ts")).toEqual({
    code: 0,
    stdout: "",
    stderr: "",
  });
});

const bracedReturn = `${hunkFunction("f")}\n`;
const unbracedReturn = "function f(a: boolean) {\n  if (a) return 1;\n  return 2;\n}\n";

test("--fix keeps the file mode", () => {
  const dir = scratch("cli");
  const file = join(dir, "a.ts");
  writeFileSync(file, bracedReturn);
  chmodSync(file, 0o755);

  expect(run({ cwd: dir }, "--fix", "--braces", "a.ts").code).toBe(0);
  expect(statSync(file).mode & 0o777).toBe(0o755);
  expect(readFileSync(file, "utf8")).toBe(unbracedReturn);
  expect(readdirSync(dir)).toEqual(["a.ts"]);
});

test("--fix writes a file whose name is at the length limit", () => {
  const dir = scratch("cli");
  const name = `${"a".repeat(252)}.ts`;
  writeFileSync(join(dir, name), bracedReturn);

  expect(run({ cwd: dir }, "--fix", "--braces", name).code).toBe(0);
  expect(readFileSync(join(dir, name), "utf8")).toBe(unbracedReturn);
  expect(readdirSync(dir)).toEqual([name]);
});

test("--fix keeps a hard link shared", () => {
  const dir = scratch("cli");
  const file = join(dir, "a.ts");
  const twin = join(dir, "b.txt");
  writeFileSync(file, bracedReturn);
  linkSync(file, twin);

  expect(run({ cwd: dir }, "--fix", "--braces", "a.ts").code).toBe(0);
  expect(readFileSync(twin, "utf8")).toBe(unbracedReturn);
  expect(statSync(file).ino).toBe(statSync(twin).ino);
});

const sizeLimited = (dir: string) => {
  const prefix = ["sh", "-c", 'ulimit -f 2; exec "$@"', "sh"];
  Bun.spawnSync([...prefix, "sh", "-c", "head -c 8192 /dev/zero > probe"], { cwd: dir });
  const limit = statSync(join(dir, "probe")).size;
  expect([1024, 2048]).toContain(limit);
  rmSync(join(dir, "probe"));

  const limited = (...args: string[]) => {
    const { code, stdout } = run({ cwd: dir, prefix }, ...args);
    return { code, stdout };
  };

  return { limit, limited };
};

const spread = "function f() {\n  const x = {\n    a,\n  };\n  log(x);\n}\n";
const padded = (size: number) => `${spread}// ${"x".repeat(size - spread.length - 4)}\n`;

test("a replacement write cut short keeps the file", () => {
  const dir = scratch("cli");
  const { limit, limited } = sizeLimited(dir);
  const original = padded(limit);
  writeFileSync(join(dir, "a.ts"), original);

  expect(limited("--fix", "a.ts")).toEqual({
    code: 2,
    stdout: "a.ts:1:1: write: cannot write: file too large\n",
  });

  expect(readFileSync(join(dir, "a.ts"), "utf8")).toBe(original);
  expect(readdirSync(dir)).toEqual(["a.ts"]);
});

test("an in place write cut short restores the file, or says it may be truncated", () => {
  const dir = scratch("cli");
  const { limit, limited } = sizeLimited(dir);
  const cases = [
    { size: limit, message: "cannot write: file too large", kept: limit },
    {
      size: limit + 100,
      message: "cannot write: file too large, restore failed, the file may be truncated",
      kept: limit,
    },
  ];

  for (const { size, message, kept } of cases) {
    const original = padded(size);
    writeFileSync(join(dir, "a.ts"), original);
    linkSync(join(dir, "a.ts"), join(dir, "b.txt"));

    expect(limited("--fix", "a.ts")).toEqual({
      code: 2,
      stdout: `a.ts:1:1: write: ${message}\n`,
    });

    const left = readFileSync(join(dir, "a.ts"), "utf8");
    expect(left.length).toBe(kept);
    if (size <= limit) expect(left).toBe(original);
    rmSync(join(dir, "b.txt"));
  }
});

const lateNonBlockingReader = `
import array, fcntl, os, signal, subprocess, sys, termios, time
r, w = os.pipe()
fcntl.fcntl(w, fcntl.F_SETFL, fcntl.fcntl(w, fcntl.F_GETFL) | os.O_NONBLOCK)
child = subprocess.Popen(sys.argv[1:], stdout=w, stderr=sys.stderr)
os.close(w)
held, last, since = array.array("i", [0]), -1, 0.0
deadline = time.monotonic() + 20
while child.poll() is None:
    fcntl.ioctl(r, termios.FIONREAD, held)
    now = time.monotonic()
    if held[0] != last:
        last, since = held[0], now
    elif held[0] and now - since > 0.2:
        break
    if now > deadline:
        child.kill()
        sys.exit("the pipe never filled")
    time.sleep(0.01)
with os.fdopen(r, "rb") as stream:
    sys.stdout.buffer.write(stream.read())
code = child.wait(timeout=20)
if code < 0:
    if -code != signal.SIGKILL:
        signal.signal(-code, signal.SIG_DFL)
    os.kill(os.getpid(), -code)
sys.exit(code)
`;

test("a report to a non-blocking stdout waits for a reader of the full pipe", () => {
  const dir = scratch("nonblocking");
  for (let index = 0; index < 1800; index++)
    writeFileSync(join(dir, `f${String(index).padStart(4, "0")}.ts`), "if (a) {\n  b();\n}\n");

  for (const flags of [[], ["--json"]]) {
    const expected = run({ cwd: dir }, "--check", ...flags, ".");
    expect(expected.code).toBe(1);
    expect(expected.stdout.length).toBeGreaterThan(65536);

    const prefix = ["python3", "-c", lateNonBlockingReader];
    expect(run({ cwd: dir, prefix }, "--check", ...flags, ".")).toEqual(expected);
  }
}, 60_000);

test.skipIf(process.getuid?.() === 0)("--fix writes a file in a read only directory", () => {
  const dir = scratch("cli");
  const locked = join(dir, "locked");
  const file = join(locked, "a.ts");

  mkdirSync(locked);
  writeFileSync(file, bracedReturn);
  chmodSync(locked, 0o555);

  try {
    expect(run({ cwd: dir }, "--fix", "--braces", "locked/a.ts").code).toBe(0);
    expect(readFileSync(file, "utf8")).toBe(unbracedReturn);
    expect(readdirSync(locked)).toEqual(["a.ts"]);
  } finally {
    chmodSync(locked, 0o755);
  }
});

test("--fix writes through a symlink", () => {
  const dir = scratch("cli");
  const target = join(dir, "target");
  const src = join(dir, "src");
  mkdirSync(target);
  mkdirSync(src);

  const real = join(target, "real.ts");
  const link = join(src, "link.ts");
  writeFileSync(real, bracedReturn);
  symlinkSync(real, link);

  expect(run({ cwd: dir }, "--fix", "--braces", link).code).toBe(0);
  expect(lstatSync(link).isSymbolicLink()).toBe(true);
  expect(readFileSync(real, "utf8")).toBe(unbracedReturn);
  expect(readdirSync(target)).toEqual(["real.ts"]);
  expect(readdirSync(src)).toEqual(["link.ts"]);
});

test.skipIf(process.getuid?.() === 0)("--fix continues after an unreadable file", () => {
  const dir = scratch("cli");
  const fixtures = join(import.meta.dir, "fixtures", "braces");
  const before = readFileSync(join(fixtures, "bodies.before.ts"));
  const after = readFileSync(join(fixtures, "bodies.after.ts"));
  for (const args of [["."], ["a.ts", "b.ts", "c.ts"]]) {
    writeFileSync(join(dir, "a.ts"), before);
    writeFileSync(join(dir, "b.ts"), before);
    writeFileSync(join(dir, "c.ts"), before);
    chmodSync(join(dir, "b.ts"), 0o000);

    try {
      const result = run({ cwd: dir }, "--fix", ...args);
      expect(result.code).toBe(2);
      expect(result.stderr).toBe("rewrote 2 files, no findings left, 1 file failed\n");
      expect(result.stdout).toBe("b.ts:1:1: parse: cannot read: permission denied\n");

      expect(readFileSync(join(dir, "a.ts"))).toEqual(after);
      expect(readFileSync(join(dir, "c.ts"))).toEqual(after);
    } finally {
      chmodSync(join(dir, "b.ts"), 0o644);
    }
  }
});

test.skipIf(process.getuid?.() === 0)("unreadable and read only files report the reason", () => {
  const dir = scratch("cli");
  writeFileSync(join(dir, "noread.ts"), bracedReturn);
  writeFileSync(join(dir, "ro.ts"), bracedReturn);
  chmodSync(join(dir, "noread.ts"), 0o000);
  chmodSync(join(dir, "ro.ts"), 0o444);

  try {
    expect(run({ cwd: dir }, "--check", "noread.ts")).toEqual({
      code: 2,
      stderr: "checked 1 file, no findings, 1 file failed\n",
      stdout: "noread.ts:1:1: parse: cannot read: permission denied\n",
    });

    expect(run({ cwd: dir }, "--fix", "--braces", "ro.ts")).toEqual({
      code: 2,
      stderr: "checked 1 file, no findings left, 1 file failed\n",
      stdout: "ro.ts:1:1: write: cannot write: permission denied\n",
    });

    const unread = run({ cwd: dir }, "--check", "--json", "noread.ts");
    expect(unread.code).toBe(2);
    expect(JSON.parse(unread.stdout)).toEqual([
      {
        path: "noread.ts",
        line: 1,
        col: 1,
        rule: "parse",
        message: "cannot read: permission denied",
        fixable: false,
      },
    ]);

    const unwritten = run({ cwd: dir }, "--fix", "--braces", "--json", "ro.ts");
    expect(unwritten.code).toBe(2);
    expect(JSON.parse(unwritten.stdout)).toEqual([
      {
        path: "ro.ts",
        line: 1,
        col: 1,
        rule: "write",
        message: "cannot write: permission denied",
        fixable: false,
      },
    ]);
  } finally {
    chmodSync(join(dir, "noread.ts"), 0o644);
    chmodSync(join(dir, "ro.ts"), 0o644);
  }
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

test.skipIf(process.getuid?.() === 0)("an unreadable folder names the system's reason", () => {
  const dir = scratch("cli");
  const locked = join(dir, "locked");
  mkdirSync(locked);
  writeFileSync(join(locked, "a.ts"), "export const z = 1;\n");
  chmodSync(locked, 0o000);

  try {
    expect(run({ cwd: dir }, "--check", ".")).toEqual({
      code: 2,
      stderr: "stanza: cannot read locked: permission denied\n",
      stdout: "",
    });
  } finally {
    chmodSync(locked, 0o755);
  }
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

test("explain reads a symlink loop as unreadable, not missing", () => {
  const dir = scratch("cli");
  symlinkSync("lp2.ts", join(dir, "lp1.ts"));
  symlinkSync("lp1.ts", join(dir, "lp2.ts"));
  expect(run({ cwd: dir }, "explain", "lp1.ts:1")).toEqual(symlinkLoop);
});

test("an empty path argument refuses the run before touching a file", () => {
  const braced = "if (a) {\n  b();\n}\n";
  const forms = [
    ["--fix", ""],
    ["--fix", "a.ts", ""],
    ["--fix", "--", "a.ts", ""],
    ["--check", "--", ""],
  ];

  for (const form of forms) {
    const cwd = scratchGitRepository({ files: { "a.ts": braced, "b.ts": braced } });
    expect(run({ cwd }, ...form)).toEqual({
      code: 2,
      stderr: 'stanza: "" is not a path, pass . for the current folder\n',
      stdout: "",
    });

    expect(readFileSync(join(cwd, "a.ts"), "utf8")).toBe(braced);
    expect(readFileSync(join(cwd, "b.ts"), "utf8")).toBe(braced);
  }
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

test("explain skips a symlinked file when --check skips its target", () => {
  const cwd = scratchGitRepository({ files: { "api.gen.ts": singleBlock } });
  symlinkSync("api.gen.ts", join(cwd, "alias.ts"));

  const target = run({ cwd }, "explain", "api.gen.ts:1");
  expect(target.code).toBe(0);
  expect(target.stdout).toEndWith(
    "note: --fix and --check skip this file as generated or excluded\n",
  );

  expect(run({ cwd }, "explain", "alias.ts:1")).toEqual(target);

  writeFileSync(join(cwd, "script"), singleBlock);
  symlinkSync("script", join(cwd, "link.ts"));
  expect(run({ cwd }, "--check", "link.ts").stderr).toContain("its files are always skipped");

  const linked = run({ cwd }, "explain", "link.ts:1");
  expect(linked.code).toBe(0);
  expect(linked.stdout).toEndWith(
    "note: --fix and --check skip this file as generated or excluded\n",
  );
});

test("explain keeps a symlinked file when --check keeps its target", () => {
  const cwd = scratchGitRepository({
    files: { "a.ts": singleBlock, ".gitattributes": "alias.ts -stanza\n" },
  });

  symlinkSync("a.ts", join(cwd, "alias.ts"));
  expect(run({ cwd }, "--check", "alias.ts")).toEqual({
    code: 1,
    stdout: bracesAt("a.ts"),
    stderr: "1 finding in 1 file, fixable with --fix\n",
  });

  const explained = run({ cwd }, "explain", "alias.ts:1");
  expect(explained).toEqual(run({ cwd }, "explain", "a.ts:1"));
  expect(explained.code).toBe(0);
  expect(explained.stdout).not.toContain("skip this file");

  writeFileSync(join(cwd, ".gitattributes"), "a.ts -stanza\n");
  expect(run({ cwd }, "explain", "alias.ts:1").stdout).toEndWith(
    "note: --fix and --check skip this file, its stanza attribute opts it out\n",
  );
});

test("explain warns that git is missing as --check does", () => {
  const cwd = scratch("explain-no-git");
  writeFileSync(join(cwd, "a.ts"), singleBlock);

  const warning = "stanza: git not found on PATH, file selection fell back to the directory walk\n";
  const env = { ...colorless(), PATH: scratch("empty-path") };
  expect(run({ cwd, env }, "--check", "a.ts").stderr).toStartWith(warning);

  const explained = run({ cwd, env }, "explain", "a.ts:1");
  expect(explained.code).toBe(0);
  expect(explained.stderr).toBe(warning);
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

test("explain steps up out of a symlinked folder as --check does", () => {
  const cwd = linkedParentTree();
  const real = run({ cwd }, "explain", "real/a.ts:1");
  expect(real.code).toBe(0);
  expect(run({ cwd }, "explain", "link/../a.ts:1")).toEqual(real);
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

test("deep input never overflows the stack in check or fix", () => {
  const started = cpuMs();
  const dir = scratch("cli");
  const depth = 50_000;
  const expression = Array(depth).fill("x").join(" + ");

  const sources: [string, string][] = [
    ["plain.ts", `function f(x) { return ${expression}; }\n`],
    ["bound.ts", `function f(x) {\n  const n = 1;\n  return ${expression};\n}\n`],
    ["member.ts", `a.x = 1;\nif (a${".b".repeat(depth)}) a;\n`],
    ["else.ts", `${"if (x) {\n  x = 1;\n} else ".repeat(8_000)}{\n  x = 2;\n}\n`],
    ["guards.ts", `${"if (x) ".repeat(depth)}x = 1;\ny = 2;\n`],
    ["repeat.ts", `f();\n${"if (x) {\n".repeat(5_000)}f();\n${"}\n".repeat(5_000)}f();\n`],
  ];

  for (const [name, source] of sources)
    for (const mode of ["--check", "--fix"]) {
      const file = join(dir, name);
      writeFileSync(file, source);

      const result = run(mode, file);
      expect(result.stderr).not.toContain("stanza:");

      if (name === "guards.ts") {
        expect(result.code).toBe(2);
        expect(result.stdout).toContain(
          ": parse: nests more than 10000 levels deep, left untouched",
        );
      } else expect([0, 1]).toContain(result.code);
    }

  expect(cpuMs() - started).toBeLessThan(30_000);
}, 60_000);

test("--fix keeps a leading byte order mark and first line columns ignore it", () => {
  const dir = scratch("cli");
  const fixtures = join(import.meta.dir, "fixtures", "braces");
  const mark = Buffer.from([0xef, 0xbb, 0xbf]);

  const file = join(dir, "bom.ts");
  writeFileSync(file, Buffer.concat([mark, readFileSync(join(fixtures, "bodies.before.ts"))]));

  const result = run("--fix", file);
  expect(result.code).toBe(0);
  expect(readFileSync(file)).toEqual(
    Buffer.concat([mark, readFileSync(join(fixtures, "bodies.after.ts"))]),
  );

  const plain = join(dir, "plain.ts");
  const first = "if (a) {\n  b();\n}\n";
  writeFileSync(plain, first);
  writeFileSync(file, Buffer.concat([mark, Buffer.from(first)]));

  const expected = run("--check", plain).stdout;
  expect(expected).toMatch(/plain\.ts:1:[2-9]/);
  expect(run("--check", file).stdout.replace("bom.ts", "plain.ts")).toBe(expected);
});

test("JSX in .js, .mjs and .cjs files parses", () => {
  const dir = scratch("cli");
  const source = "export function f(a) {\n  if (a) {\n    return <div/>;\n  }\n  return null;\n}\n";
  const fixed = "export function f(a) {\n  if (a) return <div/>;\n  return null;\n}\n";
  for (const extension of [".js", ".mjs", ".cjs"]) {
    const file = join(dir, `jsx${extension}`);
    const prefix = extension === ".cjs" ? "" : "export ";
    writeFileSync(file, source.replace("export ", prefix));

    const result = run("--check", file);
    expect(result.code).toBe(1);
    expect(result.stdout).toContain(": braces: ");
    expect(result.stdout).not.toContain(": parse: ");

    expect(run("--fix", file)).toEqual({
      code: 0,
      stderr: "rewrote 1 file, no findings left\n",
      stdout: "",
    });

    expect(readFileSync(file, "utf8")).toBe(fixed.replace("export ", prefix));
  }

  const cjs = join(dir, "top.cjs");
  writeFileSync(cjs, "if (a) {\n  return;\n}\n");
  expect(run("--check", cjs).stdout).not.toContain(": parse: ");

  const mjs = join(dir, "loop.mjs");
  writeFileSync(mjs, "for await (const item of items) {\n  consume(item);\n}\n");
  expect(run("--check", mjs).stdout).not.toContain(": parse: ");
});

test("exit codes: clean 0, findings 1, usage 2", () => {
  const dir = scratch("cli");
  writeFileSync(join(dir, "clean.ts"), "export const a = 1;\n");
  writeFileSync(join(dir, "dirty.ts"), `export ${hunkFunction("f")}\n`);

  expect(run("--check", join(dir, "clean.ts")).code).toBe(0);
  expect(run("--check", join(dir, "dirty.ts")).code).toBe(1);
  expect(run(join(dir, "dirty.ts")).code).toBe(2);
  expect(run("--fix", "--check", join(dir, "dirty.ts")).code).toBe(2);
});

test("--no-braces keeps braces and still reports blank line rules", () => {
  const original = readFileSync(
    join(import.meta.dir, "fixtures", "braces", "bodies.before.ts"),
    "utf8",
  );

  const dir = scratch("cli");
  const file = join(dir, "bodies.ts");
  writeFileSync(file, original);

  const check = run("--check", "--no-braces", file);
  expect(check.code).toBe(1);
  expect(check.stdout.split("\n").some((line) => line.includes(": braces: "))).toBe(false);
  expect(check.stdout).toContain(": after-multiline: ");
  expect(run("--fix", "--no-braces", file).code).toBe(0);

  const fixed = readFileSync(file, "utf8");
  expect(fixed).not.toBe(original);
  expect(fixed.split("{").length).toBe(original.split("{").length);
  expect(run("--check", "--no-braces", file)).toEqual({
    code: 0,
    stderr: "checked 1 file, no findings\n",
    stdout: "",
  });

  expect(run("--check", "--no-braces", "--no-braces", file).code).toBe(2);
});

test("package.json eslintConfig keeps braces in fix and check", () => {
  const dir = scratch("cli-braces");
  const file = join(dir, "x.ts");
  const source = "if (ok) {\n  run();\n}\n";
  writeFileSync(join(dir, "package.json"), '{ "eslintConfig": { "rules": { "curly": "error" } } }');
  writeFileSync(file, source);

  expect(run({ cwd: dir }, "--fix", "x.ts").code).toBe(0);
  expect(readFileSync(file, "utf8")).toBe(source);

  const checked = run({ cwd: dir }, "--check", "x.ts");
  expect(checked.code).toBe(0);
  expect(checked.stdout).not.toContain(": braces: ");
});

test("unknown braces config warns once and explicit flags settle it", () => {
  const dir = scratch("cli-braces");
  const source = readFileSync(join(import.meta.dir, "fixtures/braces/bodies.before.ts"), "utf8");
  const first = join(dir, "first.ts");
  const second = join(dir, "second.ts");

  writeFileSync(join(dir, "eslint.config.js"), "export default makeConfig();\n");
  writeFileSync(first, source);
  writeFileSync(second, source);

  const notice =
    "stanza: cannot tell whether eslint.config.js enforces braces, so braces stay; pass --braces or --no-braces to settle it\n";

  const checked = run({ cwd: dir }, "--check", first);
  expect(checked.stdout).not.toContain(": braces: ");
  expect(checked.stderr).toBe(`${notice}1 finding in 1 file, fixable with --fix\n`);

  const forced = run({ cwd: dir }, "--check", "--braces", first);
  expect(forced.stdout).toContain(": braces: ");
  expect(forced.stderr).toBe("9 findings in 1 file, all fixable with --fix\n");

  const json = run({ cwd: dir }, "--check", "--json", first);
  expect(() => JSON.parse(json.stdout)).not.toThrow();
  expect(json.stderr).toBe(notice);

  const bothFiles = run({ cwd: dir }, "--check", first, second);
  expect(bothFiles.stderr).toBe(`${notice}2 findings in 2 files, all fixable with --fix\n`);

  expect(run("--check", "--braces", "--no-braces", first).code).toBe(2);
  expect(run("--check", "--braces", "--braces", first).code).toBe(2);
});

test("--fix --stdin --json keeps stderr parseable under an unknown braces config", () => {
  const dir = scratch("cli-braces");
  const source = readFileSync(join(import.meta.dir, "fixtures/braces/bodies.before.ts"));
  writeFileSync(join(dir, "eslint.config.js"), "export default makeConfig();\n");

  const json = run({ cwd: dir, stdin: source }, "--fix", "--json", "--stdin", "bodies.ts");
  expect(() => JSON.parse(json.stderr)).not.toThrow();

  const text = run({ cwd: dir, stdin: source }, "--fix", "--stdin", "bodies.ts");
  expect(text.stderr).toContain("cannot tell whether eslint.config.js enforces braces");
});

test("known oxlint braces decisions print no notice", () => {
  const source = readFileSync(join(import.meta.dir, "fixtures/braces/bodies.before.ts"), "utf8");
  for (const curly of ["error", "off"]) {
    const dir = scratch("cli-braces");
    const file = join(dir, "bodies.ts");
    writeFileSync(join(dir, ".oxlintrc.json"), JSON.stringify({ rules: { curly } }));
    writeFileSync(file, source);

    expect(run({ cwd: dir }, "--check", file).stderr).not.toContain("stanza:");
  }
});

test("without git the fallback walk stops at the repository above it", () => {
  const bin = scratch("no-git");
  const cwd = scratch("ignore-boundary");
  const source = "if (ok) {\n  run();\n}\n";
  symlinkSync(process.execPath, join(bin, "bun"));

  writeFileSync(join(cwd, ".gitignore"), "project/\nsub/\n");
  mkdirSync(join(cwd, "project/.git"), { recursive: true });
  mkdirSync(join(cwd, "project/sub"));
  writeFileSync(join(cwd, "project/keep.ts"), source);
  writeFileSync(join(cwd, "project/sub/keep.ts"), source);

  const env = { ...process.env, PATH: bin };
  const whole = run({ cwd, env }, "--check", "project");
  const nested = run({ cwd, env }, "--check", "project/sub");

  expect(whole.stdout).toContain("project/keep.ts:");
  expect(whole.stdout).toContain("project/sub/keep.ts:");
  expect(nested.stdout).toContain("project/sub/keep.ts:");
  expect([whole.code, nested.code]).toEqual([1, 1]);
});

test("falls back when git is absent", () => {
  const bin = scratch("no-git");
  const fixture = join(import.meta.dir, "fixtures");
  const copy = scratch("fixtures");
  const env = { ...process.env, PATH: bin };

  symlinkSync(process.execPath, join(bin, "bun"));
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
});

test("--changed without git exits 2 naming git", () => {
  const bin = scratch("no-git");
  const env = { ...process.env, PATH: bin };
  symlinkSync(process.execPath, join(bin, "bun"));

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
  ["--check", "--changed"],
  ["--check", "--staged"],
])
  test(`a git failure while picking files exits 2: ${args.join(" ")}`, () => {
    const dir = scratchGitRepository({ files: { "a.ts": "export const a = 1;\n" }, staged: true });
    writeFileSync(join(dir, ".git", "index"), "junkjunkjunkjunkjunk");

    const result = run({ cwd: dir }, ...args);
    expect(result.code).toBe(2);
    expect(result.stderr).toContain("git");
  });

for (const args of [
  ["--check", "."],
  ["--check", "a.ts"],
  ["--check", "--changed"],
  ["--check", "--staged"],
])
  test(`git refusing an existing repository exits 2 instead of walking it: ${args.join(" ")}`, () => {
    const dir = scratchGitRepository({ files: { "a.ts": "export const a = 1;\n" } });
    const result = run({ cwd: dir, env: gitRefusesOwnership }, ...args);
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

test("--fix --stdin prints the fixed text and leaves the file alone", () => {
  const original = readFileSync(join(import.meta.dir, "fixtures", "braces", "bodies.before.ts"));
  const expected = readFileSync(join(import.meta.dir, "fixtures", "braces", "bodies.after.ts"));

  const dir = scratch("cli");
  writeFileSync(join(dir, "bodies.ts"), original);
  writeFileSync(join(dir, "copy.ts"), original);

  const result = run({ cwd: dir, stdin: original }, "--fix", "--stdin", "bodies.ts");
  expect(result.stdout).toBe(expected.toString("utf8"));
  expect(result.code).toBe(run("--fix", join(dir, "copy.ts")).code);
  expect(readFileSync(join(dir, "bodies.ts"))).toEqual(original);

  const piped = run({ cwd: dir, stdin: original }, "--fix", "--stdin", "bodies.ts");
  expect(piped).toEqual(result);
});

const braced = (name: string) => `if (${name}) {\n  ${name}();\n}\n`;
const unbraced = (name: string) => `if (${name}) ${name}();\n`;

function fixedWithBraces(source: string): ReturnType<typeof run> {
  return run({ stdin: Buffer.from(source) }, "--fix", "--stdin", "x.ts", "--braces");
}

test("--fix --stdin keeps braces that bound or sit inside a frozen region", () => {
  const sources = [
    "if (a) {\n  f(/* stanza-off */);\n}\n",
    "/* stanza-off */\nrun();\n\nif (a) {\n  f(/* stanza-on */);\n}\n",
  ];

  for (const source of sources)
    expect(fixedWithBraces(source)).toEqual({ code: 0, stderr: "", stdout: source });

  const nested =
    "function f() {\n  if (a) {\n    if (b) {\n      /* stanza-off */\n      x();\n    }\n  }\n}\n";

  expect(fixedWithBraces(nested)).toEqual({
    code: 0,
    stderr: "",
    stdout:
      "function f() {\n  if (a)\n    if (b) {\n      /* stanza-off */\n      x();\n    }\n}\n",
  });
});

test("stanza-off inside a decorator ahead of export ends with its callback", () => {
  const source =
    "@dec(() => {\n  // stanza-off\n  a();\n}) export class A {}\n\nfunction f() {\n  const x = 1;\n\n  if (x) return;\n}\n";

  for (const path of ["x.ts", "x.js"])
    expect(run({ stdin: Buffer.from(source) }, "--fix", "--stdin", path, "--braces")).toEqual({
      code: 0,
      stderr: "",
      stdout: source.replace("1;\n\n", "1;\n"),
    });
});

test("directive prefixes do not close or extend a frozen region", () => {
  const source = `/* stanza-off */\n${braced("a")}\n// stanza-on-call\n${braced("b")}\n// stanza-offline\n${braced("c")}\n/* stanza-on */\n${braced("d")}`;

  expect(fixedWithBraces(source)).toEqual({
    code: 0,
    stderr: "",
    stdout: source.replace(braced("d"), unbraced("d")),
  });

  for (const name of ["stanza-ignored", "stanza-off-by-one", "stanza-offé"])
    expect(fixedWithBraces(`// ${name}\n${braced("a")}`)).toEqual({
      code: 0,
      stderr: "",
      stdout: `// ${name}\n${unbraced("a")}`,
    });
});

test("each documented directive comment form applies", () => {
  const forms = [
    (name: string) => `// ${name}`,
    (name: string) => `/* ${name} */`,
    (name: string) => `/** ${name} */`,
    (name: string) => `/**\n * ${name}\n */`,
    (name: string) => `// ${name} because the linter disagrees`,
    (name: string) => `// ${name}: generated table`,
  ];

  for (const form of forms) {
    const ignored = `function f() {\n  first();\n\n  ${form("stanza-ignore")}\n  if (a) {\n    run();\n  }\n\n  last();\n}\n`;
    expect(fixedWithBraces(ignored)).toEqual({ code: 0, stderr: "", stdout: ignored });

    const frozen = `${form("stanza-off")}\n${braced("a")}${form("stanza-on")}\n\n${braced("b")}`;
    expect(fixedWithBraces(frozen)).toEqual({
      code: 0,
      stderr: "",
      stdout: frozen.replace(braced("b"), unbraced("b")),
    });
  }
});

test("--fix --stdin keeps findings off stdout, as text and as --json", () => {
  const dir = scratch("cli");
  const wall = `export function f() {\n${"  step();\n".repeat(6)}}\n`;

  const text = run({ cwd: dir, stdin: Buffer.from(wall) }, "--fix", "--stdin", "wall.ts");

  expect(text.code).toBe(1);
  expect(text.stdout).toBe(wall);
  expect(text.stderr).toStartWith("wall.ts:2:3: wall: ");

  const json = run({ cwd: dir, stdin: Buffer.from(wall) }, "--fix", "--json", "--stdin", "wall.ts");

  expect(json.code).toBe(1);
  expect(json.stdout).toBe(wall);
  expect(JSON.parse(json.stderr)).toEqual([
    expect.objectContaining({ path: "wall.ts", rule: "wall" }),
  ]);
});

test("--stdin takes the extension and config from the named path, not a symlink target", () => {
  const dir = scratch("cli");
  const other = scratch("cli");
  const typed =
    "export function f(a: number): number {\n  if (a) {\n    return 1;\n  }\n  return 2;\n}\n";

  writeFileSync(join(other, "view.js"), "");
  writeFileSync(
    join(other, "eslint.config.js"),
    'export default [{ rules: { curly: "error" } }];\n',
  );

  symlinkSync(join(other, "view.js"), join(dir, "view.ts"));

  const result = run({ cwd: dir, stdin: Buffer.from(typed) }, "--fix", "--stdin", "view.ts");

  expect(result.stdout).toBe(
    "export function f(a: number): number {\n  if (a) return 1;\n  return 2;\n}\n",
  );

  expect(result.stderr).toBe("");
});

test("--check --stdin matches --check on the same file", () => {
  const dir = scratch("cli");
  const cases = [
    ["clean.ts", "export const a = 1;\n", 0],
    ["dirty.ts", `export ${hunkFunction("f")}\n`, 1],
    ["broken.ts", "function (\n", 2],
  ] as const;

  for (const [name, text, code] of cases) {
    const file = join(dir, name);
    writeFileSync(file, text);

    const byPath = run("--check", file);
    const byStdin = run({ stdin: Buffer.from(text) }, "--check", "--stdin", file);

    expect(byPath.code).toBe(code);
    expect(byStdin.code).toBe(code);
    expect(byStdin.stdout).toBe(byPath.stdout);
  }
});

test("--check --braces names exactly the edits --fix makes", () => {
  const cases = [
    {
      source: "function f() {\n  if (a) {\n    x();\n  }\n  y();\n}\n",
      fixed: "function f() {\n  if (a) x();\n  y();\n}\n",
      findings: ["2:10 braces"],
    },
    {
      source: "function f() {\n  g();\n  if (a) { if (b) { if (c) { x(); } } }\n}\n",
      fixed: "function f() {\n  g();\n  if (a) if (b) if (c) x();\n}\n",
      findings: ["3:10 braces", "3:19 braces", "3:28 braces"],
    },
  ];

  for (const { source, fixed, findings } of cases) {
    const stdin = Buffer.from(source);
    const checked = run({ stdin }, "--check", "--braces", "--stdin", "a.ts");
    const reported = checked.stdout
      .trim()
      .split("\n")
      .map((line) => line.split(": ").slice(0, 2).join(" ").replace("a.ts:", ""));

    expect(checked.code).toBe(1);
    expect(reported).toEqual(findings);
    expect(run({ stdin }, "--fix", "--braces", "--stdin", "a.ts").stdout).toBe(fixed);
  }
});

test("--fix --stdin echoes generated and unparsable input unchanged", () => {
  const dir = scratch("cli");
  const source = `export ${hunkFunction("f")}\n`;

  const generated = `// @generated\n${source}`;
  const broken = "function (\n";
  const fixed = run({ cwd: dir, stdin: Buffer.from(source) }, "--fix", "--stdin", "x.ts");

  expect(fixed.stdout).not.toBe(source);
  expect(run({ cwd: dir, stdin: Buffer.from(generated) }, "--fix", "--stdin", "x.ts")).toEqual({
    code: 0,
    stderr: "",
    stdout: generated,
  });

  const parse = run({ cwd: dir, stdin: Buffer.from(broken) }, "--fix", "--stdin", "x.ts");

  expect(parse.code).toBe(2);
  expect(parse.stdout).toBe(broken);
});

test("--stdin honours linguist-generated for a path whose directory does not exist", () => {
  const dir = scratchGitRepository();
  const source = `export ${hunkFunction("f")}\n`;

  writeFileSync(join(dir, ".gitattributes"), "gen/** linguist-generated\n");

  expect(
    run({ cwd: dir, stdin: Buffer.from(source) }, "--fix", "--stdin", "gen/v2/api.ts"),
  ).toEqual({ code: 0, stderr: "", stdout: source });
});

test("--fix --stdin in a repository git refuses echoes the input and exits 2", () => {
  const dir = scratchGitRepository();
  const source = `export ${hunkFunction("f")}\n`;

  const result = run(
    { cwd: dir, env: gitRefusesOwnership, stdin: Buffer.from(source) },
    "--fix",
    "--stdin",
    "x.ts",
  );

  expect(result.code).toBe(2);
  expect(result.stdout).toBe(source);
  expect(result.stderr).toContain("dubious ownership");
});

test("--fix --stdin echoes the input on a target failure, and --json reports it in the array", () => {
  const empty = scratch("stdin-failure");
  const errorEntry = (path: string, message: string) => [
    { path, line: 1, col: 1, rule: "error", message, fixable: false },
  ];

  const unsupported = run(
    { cwd: empty, stdin: Buffer.from("text\n") },
    "--fix",
    "--json",
    "--stdin",
    "notes.md",
  );

  expect([unsupported.code, unsupported.stdout]).toEqual([2, "text\n"]);
  expect(JSON.parse(unsupported.stderr)).toEqual(
    errorEntry("notes.md", "not a TypeScript or JavaScript file: notes.md"),
  );

  const future = scratchGitRepository();
  git(future, "config", "core.repositoryformatversion", "99");

  const refused = run(
    { cwd: future, stdin: Buffer.from("text\n") },
    "--fix",
    "--json",
    "--stdin",
    "a.ts",
  );

  expect([refused.code, refused.stdout]).toEqual([2, "text\n"]);
  const [entry] = JSON.parse(refused.stderr);
  expect(JSON.parse(refused.stderr)).toEqual(errorEntry("a.ts", entry.message));
  expect(entry.message).toStartWith("git rev-parse failed:");

  const directory = shell(`cd "${empty}" && exec "$1" --fix --json --stdin a.ts < .`, "");
  expect([directory.code, directory.stdout]).toEqual([2, ""]);
  expect(JSON.parse(directory.stderr)).toEqual(
    errorEntry("a.ts", "cannot read stdin: is a directory"),
  );
});

test("--fix --stdin echoes the input when the working directory is gone", () => {
  const directory = scratch("stdin-deleted-cwd");
  const source = Buffer.from([0xff, ...Buffer.from("text\n")]);
  const message = "cannot read the working directory: no such file or directory";

  const fromDeleted = (...flags: string[]) => {
    const gone = join(directory, `gone-${Math.random().toString(36).slice(2)}`);
    const out = `${gone}.out`;
    mkdirSync(gone);

    const script = `cd "${gone}" && rmdir "${gone}" && exec "$@" > "${out}"`;
    const { code, stderr } = run({ prefix: ["sh", "-c", script, "sh"], stdin: source }, ...flags);
    return { code, stdout: readFileSync(out), stderr };
  };

  const text = fromDeleted("--fix", "--stdin", "a.ts");
  expect([text.code, text.stdout.equals(source), text.stderr]).toEqual([
    2,
    true,
    `stanza: ${message}\n`,
  ]);

  const json = fromDeleted("--fix", "--json", "--stdin", "a.ts");
  expect([json.code, json.stdout.equals(source)]).toEqual([2, true]);
  expect(JSON.parse(json.stderr)).toEqual([
    { path: "a.ts", line: 1, col: 1, rule: "error", message, fixable: false },
  ]);
});

test("--stdin with --changed, a positional path or no path is a usage error", () => {
  for (const args of [
    ["--fix", "--stdin", "x.ts", "--changed"],
    ["--fix", "--stdin", "x.ts", "y.ts"],
    ["--fix", "--stdin"],
  ]) {
    const result = run(...args);
    expect(result.code).toBe(2);
    expect(result.stdout).toBe("");
  }
});

test("--staged works only with --check and as the only source", () => {
  for (const args of [
    ["--fix", "--staged"],
    ["--check", "--staged", "--changed"],
    ["--check", "--staged", "x.ts"],
    ["--check", "--staged", "--staged"],
  ]) {
    const result = run(...args);
    expect(result.code).toBe(2);
    expect(result.stdout).toBe("");
  }
});

test("paths after -- may start with a dash", () => {
  const dir = scratch("cli");
  writeFileSync(join(dir, "-x.ts"), "export const x = 1;\n");

  expect(run({ cwd: dir }, "--check", "-x.ts").code).toBe(2);
  expect(run({ cwd: dir }, "--check", "--", "-x.ts").code).toBe(0);
});

test("join findings name the binding and the statement that reads it", () => {
  const dir = scratch("cli");
  const file = join(dir, "joins.ts");
  writeFileSync(
    file,
    [
      "export function f(rows: number[]) {",
      "  const [head, tail] = split(rows);",
      "",
      "  if (tail) return head;",
      "",
      "  let total = 0;",
      "",
      "  for (const row of rows) total += row;",
      "  const kind = classify(total);",
      "",
      "  return kind;",
      "}",
      "",
    ].join("\n"),
  );

  const { stdout } = run("--check", file);
  expect(stdout).toContain(
    "4:3: guard-join: remove the blank line between `tail` and the `if` that reads it",
  );

  expect(stdout).toContain(
    "8:3: use-join: remove the blank line between `total` and the `for` that reads it",
  );

  expect(stdout).toContain(
    "11:3: consume-join: remove the blank line between `kind` and the `return` that reads it",
  );
});

test("a binding shadowed inside the next statement is not what that statement reads", () => {
  const dir = scratch("cli");
  const file = join(dir, "shadow.ts");
  writeFileSync(
    file,
    [
      "export function f() {",
      "  let outer = 0;",
      "",
      "  let used = 1;",
      "",
      "  for (let outer = 0; outer < 3; outer++) consume(used);",
      "",
      "  return outer;",
      "}",
      "",
    ].join("\n"),
  );

  const { stdout } = run("--check", file);
  expect(stdout).toContain(
    "6:3: use-join: remove the blank line between `used` and the `for` that reads it",
  );

  expect(stdout).not.toContain("`outer`");
});

test("a JavaScript hashbang parses and a TypeScript annotation in a .ts file is clean", () => {
  const directory = scratch("hashbang");
  writeFileSync(join(directory, "script.js"), "#!/usr/bin/env node\nif (ready) {\n  run();\n}\n");
  writeFileSync(join(directory, "script.ts"), "const value: string = 'hello';\n");

  const result = run({ cwd: directory }, "--check", "--json", ".");
  expect(result.stderr).toBe("");
  expect(JSON.parse(result.stdout)).toEqual([
    {
      path: "script.js",
      line: 2,
      col: 12,
      rule: "braces",
      message: "remove the braces around this single statement body",
      fixable: true,
    },
  ]);
});

function shell(script: string, stdin: string, ...args: string[]) {
  return run(
    { prefix: ["sh", "-c", script, "sh"], stdin: new TextEncoder().encode(stdin) },
    ...args,
  );
}

test("a deleted working directory fails only the inputs that resolve against it", () => {
  const directory = scratch("deleted-cwd");
  const file = join(directory, "a.ts");
  writeFileSync(file, "if (ok) {\n  run();\n}\nrun();\n");

  const fromDeleted = (stdin: string, ...args: string[]) => {
    const gone = join(directory, `gone-${Math.random().toString(36).slice(2)}`);
    mkdirSync(gone);
    return shell(`cd "${gone}" && rmdir "${gone}" && exec "$@"`, stdin, ...args);
  };

  const edit = (cwd: string) =>
    JSON.stringify({
      hook_event_name: "PostToolUse",
      ...(cwd === "" ? {} : { cwd }),
      tool_name: "Edit",
      tool_input: { file_path: file },
    });

  const absolute = fromDeleted("", "--check", file);
  expect(absolute.stdout).toStartWith(`${file}:1:9: braces: `);
  expect(absolute.stderr).toBe("1 finding in 1 file, fixable with --fix\n");
  expect(absolute.code).toBe(1);

  expect(fromDeleted("", "--check", "a.ts")).toEqual({
    code: 2,
    stderr: "stanza: cannot read the working directory: no such file or directory\n",
    stdout: "",
  });

  expect(fromDeleted(edit(directory), "hook")).toEqual({ code: 0, stderr: "", stdout: "" });

  for (const input of [edit("sub"), edit(""), "{}"])
    expect(fromDeleted(input, "hook")).toEqual({
      code: 1,
      stderr: "stanza hook: cannot read the working directory: no such file or directory\n",
      stdout: "",
    });
});

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

test.skipIf(process.platform !== "linux")(
  "an address space limit too small for the work thread stack still finishes the run",
  () => {
    const directory = scratch("address-space");
    const source = "if (ok) {\n  run();\n}\nrun();\n";
    writeFileSync(join(directory, "one.ts"), source);
    mkdirSync(join(directory, "many"));

    for (let index = 0; index < 300; index++)
      writeFileSync(join(directory, "many", `f${index}.ts`), source);

    const limited = (...args: string[]) => shell('ulimit -v 900000 && exec "$@"', "", ...args);

    expect(limited("--version").code).toBe(0);
    expect(limited("--help").code).toBe(0);
    expect(limited("--check", join(directory, "one.ts")).code).toBe(1);

    const many = limited("--check", join(directory, "many"));
    expect(many.stdout.trimEnd().split("\n")).toHaveLength(300);
    expect(many.code).toBe(1);

    const notices = many.stderr.split("\n").filter(Boolean);
    expect(notices.pop()).toStartWith("300 findings in ");
    expect(notices.length).toBeLessThanOrEqual(1);
  },
);

test("a failed result write exits 2 and names output", () => {
  const directory = scratch("output-failure");
  const file = join(directory, "many.ts");

  const source = Array.from(
    { length: 1000 },
    (_, index) => `function f${index}() { a(); b(); c(); d(); e(); f(); }\n`,
  ).join("");

  writeFileSync(file, source);

  const limited = shell(
    `cd "${directory}" && trap "" XFSZ && ulimit -f 2 && exec "$@" > out.json`,
    "",
    "--check",
    "--json",
    "many.ts",
  );

  expect(limited.stderr).toMatch(/^stanza: cannot write output: [^\n]+\n$/);
  expect(limited.code).toBe(2);
  expect(readFileSync(file, "utf8")).toBe(source);

  const unlimited = shell(`cd "${directory}" && exec "$@"`, "", "--check", "--json", "many.ts");
  expect(unlimited.stderr).toBe("");
  expect(unlimited.code).toBe(1);

  const findings = JSON.parse(unlimited.stdout);
  expect(findings).toBeArray();
  expect(findings.length).toBeGreaterThan(1000);
  expect(readFileSync(file, "utf8")).toBe(source);
});

const onLinux = test.skipIf(process.platform !== "linux");

const singleBlock = "if (a) {\n  b();\n}\n";

const raw = (path: string) => Buffer.from(path, "latin1");

const notUtf8 = (name: string) => `stanza: name is not valid UTF-8: "${name}"\n`;

const bracesAt = (path: string) =>
  `${path}:1:8: braces: remove the braces around this single statement body\n`;

const oneFixable = "1 finding in 1 file, fixable with --fix\n";

function inDirectory(cwd: string, script: string, stdin = "") {
  return shell(`cd "${cwd}" && ${script}`, stdin);
}

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

test.each([
  ["--no-braces --no-braces", "--no-braces given twice"],
  ["--bogus", "unknown flag --bogus"],
  [`"$(printf '%s\\377' --x)"`, 'unknown flag "--x\\377"'],
  ["--braces --no-braces", "use one of --braces or --no-braces"],
])("stanza, explain and hook name the same problem for %s", (flags, problem) => {
  const cwd = scratch("argv-grammars");
  writeFileSync(join(cwd, "a.ts"), singleBlock);

  for (const [script, program, code] of [
    [`exec "$1" --check ${flags} a.ts`, "stanza", 2],
    [`exec "$1" explain ${flags} a.ts:1`, "stanza", 2],
    [`exec "$1" hook ${flags}`, "stanza hook", 1],
  ] as const) {
    const result = inDirectory(cwd, script, "{}");
    expect(result.stderr).toStartWith(`${program}: ${problem}\n`);
    expect(result.stdout).toBe("");
    expect(result.code).toBe(code);
  }
});

test("the first -- ends switches for stanza, explain and hook", () => {
  const cwd = scratch("argv-dashdash");

  expect(inDirectory(cwd, `exec "$1" hook --`, "{}")).toEqual({ code: 0, stderr: "", stdout: "" });

  for (const [script, stderr] of [
    [`exec "$1" --check -- -x.ts`, "stanza: no such file: -x.ts\n"],
    [`exec "$1" explain -- -x.ts:1`, "stanza: no such file: -x.ts\n"],
  ] as const)
    expect(inDirectory(cwd, script)).toEqual({ code: 2, stderr, stdout: "" });
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

test("check-attr failure wins over invalid names", () => {
  const cwd = scratchGitRepository();
  git(cwd, "commit", "--allow-empty", "-qm", "base");

  const stdin = Buffer.from("export {};\n");
  const blob = git(cwd, { stdin }, "hash-object", "-w", "--stdin").trim();
  const entry = Buffer.concat([Buffer.from(`100644 ${blob}\t`), raw("bad\xff.ts\0")]);
  git(cwd, { stdin: entry }, "update-index", "-z", "--index-info");

  const env = { ...process.env, PATH: `${failingCheckAttr()}:${process.env.PATH}` };
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

test("--check prints one finding row for each finding --json reports", () => {
  const cwd = scratch("rows");
  for (const dir of ["fixtures", "parse-cases"])
    cpSync(join(import.meta.dir, dir), join(cwd, dir), { recursive: true });

  const text = run({ cwd }, "--check", "fixtures", "parse-cases");
  const json = run({ cwd }, "--check", "--json", "fixtures", "parse-cases");
  expect(text.code).toBe(json.code);

  const findings: {
    path: string;
    line: number;
    col: number;
    rule: string;
    message: string;
    fixable: boolean;
  }[] = JSON.parse(json.stdout);

  const rows = text.stdout.trimEnd().split("\n");
  expect(rows).toHaveLength(findings.length);

  const byHand = (finding: (typeof findings)[number]) =>
    !finding.fixable && !["parse", "write", "error"].includes(finding.rule);

  expect(findings.some(byHand)).toBe(true);
  expect(findings.some((finding) => finding.rule === "parse")).toBe(true);

  for (const [index, finding] of findings.entries())
    expect<(string | undefined)[] | undefined>(findingRow.exec(rows[index]!)?.slice(1)).toEqual([
      finding.path,
      String(finding.line),
      String(finding.col),
      finding.rule,
      finding.message,
      byHand(finding) ? " (by hand)" : undefined,
    ]);
});

test("a path that would split a row or restyle the terminal prints quoted", () => {
  const cwd = scratch("quoted");
  for (const name of ["esc\x1b[31m.ts", "n\nl.ts", 'q"t.ts'])
    writeFileSync(join(cwd, name), "if (a) {\n  b();\n}\n");

  const result = run({ cwd }, "--check", ".");
  expect(result.code).toBe(1);
  expect(result.stdout.split("\n").map((line) => line.split(":")[0])).toEqual([
    '"esc\\033[31m.ts"',
    '"n\\nl.ts"',
    '"q\\"t.ts"',
    "",
  ]);
});
