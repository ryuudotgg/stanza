import { expect, test } from "bun:test";
import {
  cpSync,
  chmodSync,
  linkSync,
  lstatSync,
  mkdirSync,
  readFileSync,
  readdirSync,
  rmSync,
  statSync,
  symlinkSync,
  writeFileSync,
} from "node:fs";
import { join } from "node:path";
import { version } from "../package.json" with { type: "json" };
import { cpuSpent } from "./budget.ts";
import {
  findingRow,
  git,
  gitRefusesOwnership,
  hunkFunction,
  inDirectory,
  run,
  scratch,
  scratchGitRepository,
  shell,
} from "./support.ts";

const asRoot = process.getuid?.() === 0;

test("--version prints the source version", () => {
  const result = run("--version");
  expect(result.code).toBe(0);
  expect(result.stderr).toBe("");
  expect(result.stdout).toBe(`stanza ${version}\n`);
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

test("--hunks keeps an opted in file below a skipped folder to its changed lines", () => {
  const source = (f: number, g: number) => `${hunkFunction("f", f)}\n\n${hunkFunction("g", g)}\n`;

  const cwd = scratchGitRepository({
    files: {
      ".gitattributes": "dist/** stanza\n",
      "dist/a.ts": source(2, 2),
      "src/z.ts": source(2, 2),
    },
    staged: true,
  });

  git(cwd, "commit", "-qm", "init");
  writeFileSync(join(cwd, "dist", "a.ts"), source(2, 3));
  writeFileSync(join(cwd, "src", "z.ts"), source(3, 2));
  git(cwd, "add", "dist/a.ts", "src/z.ts");

  const braces = "braces: remove the braces around this single statement body\n";
  const findings = `dist/a.ts:9:10: ${braces}src/z.ts:2:10: ${braces}`;
  for (const source of ["--changed", "--staged"])
    expect(run({ cwd }, "--check", source, "--hunks").stdout).toBe(findings);
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

test("a file nested a million levels deep is reported and left untouched", () => {
  const dir = scratch("deep");
  const file = join(dir, "arrays.ts");
  const source = `const a = ${"[".repeat(1_000_000)}0${"]".repeat(1_000_000)};\n`;
  writeFileSync(file, source);

  const { value: alone, ms } = cpuSpent(() => run({ cwd: dir }, "--fix", "arrays.ts"));
  expect(alone.code).toBe(2);
  expect(alone.stdout).toMatch(
    /^arrays\.ts:\d+:\d+: parse: nests more than 10000 levels deep, left untouched\n$/,
  );

  expect(ms).toBeLessThan(1_000);
  expect(readFileSync(file, "utf8")).toBe(source);

  for (const fixable of ["a.ts", "b.ts", "c.ts"])
    writeFileSync(join(dir, fixable), "if (a) {\n  x();\n}\n");

  const fixed = run({ cwd: dir }, "--fix", "--braces", ".");
  expect(fixed.code).toBe(2);
  expect(fixed.stdout).toContain("arrays.ts:");
  expect(readFileSync(file, "utf8")).toBe(source);

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

test("--fix writes a file whose name is at the length limit", () => {
  const dir = scratch("cli");
  const name = `${"a".repeat(252)}.ts`;
  writeFileSync(join(dir, name), bracedReturn);

  expect(run({ cwd: dir }, "--fix", "--braces", name).code).toBe(0);
  expect(readFileSync(join(dir, name), "utf8")).toBe(unbracedReturn);
  expect(readdirSync(dir)).toEqual([name]);
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

test.skipIf(asRoot)("--fix continues after an unreadable file", () => {
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

test.skipIf(asRoot)("unreadable and read only files report the reason", () => {
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

test("--fix --stdin in a repository git refuses echoes the input and exits 2", () => {
  const dir = scratchGitRepository();
  const source = `export ${hunkFunction("f")}\n`;

  const result = run(
    { cwd: dir, env: gitRefusesOwnership(), stdin: Buffer.from(source) },
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

const singleBlock = "if (a) {\n  b();\n}\n";

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
