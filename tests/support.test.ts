import { afterEach, expect, test } from "bun:test";
import {
  chmodSync,
  existsSync,
  readlinkSync,
  realpathSync,
  symlinkSync,
  writeFileSync,
} from "node:fs";
import { join } from "node:path";
import {
  chooseGit,
  committedSource,
  git,
  gitBinary,
  gitLinks,
  gitShim,
  run,
  scratch,
  scratchGitRepository,
  superproject,
} from "./support.ts";

const saved = Object.fromEntries(
  [
    "STANZA_BIN",
    "PROBE",
    "AGENT_HOOKS",
    "CLAUDECODE",
    "FORCE_COLOR",
    "GIT_CONFIG_GLOBAL",
    "GIT_CONFIG_COUNT",
    "GIT_CONFIG_KEY_0",
    "GIT_CONFIG_VALUE_0",
  ].map((key) => [key, process.env[key]]),
);

afterEach(() => {
  for (const [key, value] of Object.entries(saved))
    if (value === undefined) delete process.env[key];
    else process.env[key] = value;
});

function scriptBinary(body: string): string {
  const path = join(scratch("script-bin"), "stanza");
  writeFileSync(path, `#!/bin/sh\n${body}`);
  chmodSync(path, 0o755);
  return path;
}

function fakeBinary(body = ""): string {
  return scriptBinary(
    `${body}for arg in "$@"; do printf 'arg %s\\n' "$arg"; done\nprintf 'cwd %s\\n' "$(pwd -P)"\nprintf 'probe %s\\n' "\${PROBE-unset}"\nprintf 'color %s\\n' "\${FORCE_COLOR-unset}"\nprintf 'agent %s\\n' "\${CLAUDECODE-unset}"\nprintf 'stdin %s\\n' "$(cat)"\necho oops >&2\nexit 7\n`,
  );
}

const stdin = new TextEncoder().encode("piped");
const execPrefix = ["sh", "-c", 'exec "$@"', "sh"];
const env = { PROBE: "set", FORCE_COLOR: "3", CLAUDECODE: "1" };

test("run spawns STANZA_BIN with the same arguments, cwd, environment and stdin", () => {
  process.env.STANZA_BIN = fakeBinary();
  const cwd = scratch("fake-cwd");

  const expected = {
    code: 7,
    stdout: `arg --check\narg a b.ts\narg c.ts\ncwd ${cwd}\nprobe set\ncolor 3\nagent 1\nstdin piped\n`,
    stderr: "oops\n",
  };

  expect(run({ cwd, env, stdin }, "--check", "a b.ts", "c.ts")).toEqual(expected);
  expect(run({ cwd, env, stdin, prefix: execPrefix }, "--check", "a b.ts", "c.ts")).toEqual(
    expected,
  );
});

test("run defaults to this process's cwd and environment and an empty stdin", () => {
  process.env.STANZA_BIN = fakeBinary();
  process.env.PROBE = "inherited";
  process.env.FORCE_COLOR = "3";
  process.env.CLAUDECODE = "1";

  const expected = {
    code: 7,
    stdout: `arg --check\ncwd ${realpathSync(process.cwd())}\nprobe inherited\ncolor unset\nagent unset\nstdin \n`,
    stderr: "oops\n",
  };

  expect(run("--check")).toEqual(expected);
  expect(run({}, "--check")).toEqual(expected);
});

test("run drops inherited hook and color controls and keeps explicit overrides", () => {
  process.env.STANZA_BIN = scriptBinary(
    `printf 'hooks %s\\ncolor %s\\nprobe %s\\n' "\${AGENT_HOOKS-unset}" "\${FORCE_COLOR-unset}" "\${PROBE-unset}"\n`,
  );

  process.env.AGENT_HOOKS = "0";
  process.env.FORCE_COLOR = "1";
  process.env.PROBE = "inherited";
  const before = { ...process.env };

  expect(run("--check").stdout).toBe("hooks unset\ncolor unset\nprobe inherited\n");
  expect(process.env).toEqual(before);

  const explicit = { env: { AGENT_HOOKS: "0", FORCE_COLOR: "1" } };
  expect(run(explicit, "--check").stdout).toBe("hooks 0\ncolor 1\nprobe inherited\n");
  expect(process.env).toEqual(before);

  expect(run({ env: { PROBE: undefined } }, "--check").stdout).toBe(
    "hooks unset\ncolor unset\nprobe unset\n",
  );

  expect(process.env).toEqual(before);
});

test("run keeps a leading byte order mark on stdout and stderr", () => {
  const line = "printf '\\357\\273\\277x\\n'";
  process.env.STANZA_BIN = scriptBinary(`${line}\n${line} >&2\n`);
  expect(run("--check")).toEqual({ code: 0, stdout: "\u{feff}x\n", stderr: "\u{feff}x\n" });
});

test("a STANZA_BIN killed by a signal fails the helper instead of returning an exit code", () => {
  process.env.STANZA_BIN = fakeBinary("kill -SEGV $$\n");
  expect(() => run("--check")).toThrow("was killed by SIGSEGV");
  expect(() => run({ prefix: execPrefix }, "--check")).toThrow("was killed by SIGSEGV");
});

test("run preserves argv and exit status through a shell prefix", () => {
  process.env.STANZA_BIN = fakeBinary(`printf 'limit %s\\n' "$(ulimit -f)"\n`);
  process.env.PROBE = "inherited";

  const result = run(
    { prefix: ["sh", "-c", 'ulimit -f 2; exec "$@"', "sh"] },
    "--check",
    "a b.ts",
    "",
  );

  const lines = result.stdout.split("\n");
  expect(result.code).toBe(7);
  expect(lines.filter((line) => line.startsWith("arg "))).toEqual([
    "arg --check",
    "arg a b.ts",
    "arg ",
  ]);

  expect(lines).toContain("limit 2");
  expect(lines).toContain("probe inherited");
});

test("run rejects a signal from a shell prefix", () => {
  process.env.STANZA_BIN = fakeBinary();
  expect(() => run({ prefix: ["sh", "-c", "kill -SEGV $$"] }, "--check")).toThrow(
    "stanza under sh -c kill -SEGV $$ was killed by SIGSEGV",
  );
});

test("a STANZA_BIN naming no file fails the helper before it spawns", () => {
  process.env.STANZA_BIN = join(scratch("missing-bin"), "stanza");
  expect(() => run("--check")).toThrow("STANZA_BIN names no file");
});

test("run takes its options only before the arguments", () => {
  process.env.STANZA_BIN = join(scratch("missing-bin"), "stanza");
  // @ts-expect-error
  expect(() => run("--check", { cwd: "." })).toThrow("STANZA_BIN names no file");
});

const onMac = process.platform === "darwin";
const homebrewGit = "/opt/homebrew/bin/git";
const pathWithoutLinks = (process.env.PATH ?? "")
  .split(":")
  .filter((entry) => entry !== gitLinks)
  .join(":");

test.skipIf(!onMac || Bun.which("git", { PATH: pathWithoutLinks }) !== homebrewGit)(
  "the preload runs Homebrew's git when PATH selects it and leaves PATH alone",
  () => {
    expect(gitBinary).toBe(homebrewGit);
    expect(process.env.PATH).toBe(pathWithoutLinks);
    expect(chooseGit(pathWithoutLinks)).toEqual({ binary: homebrewGit, path: pathWithoutLinks });
  },
);

test.skipIf(!onMac || !existsSync(homebrewGit))(
  "Homebrew's git first on PATH is chosen as is",
  () => {
    const path = `/opt/homebrew/bin:/usr/bin:/bin`;
    expect(chooseGit(path, scratch("git-links"))).toEqual({ binary: homebrewGit, path });
  },
);

const xcrunAnswer = onMac
  ? Bun.spawnSync(["/usr/bin/xcrun", "--find", "git"]).stdout.toString().trim()
  : "";

function appleShimFolder(): string {
  const shim = scratch("git-shim");
  symlinkSync("/usr/bin/git", join(shim, "git"));
  return shim;
}

test.skipIf(!xcrunAnswer)("Apple's git shim first on PATH gives way to the git xcrun names", () => {
  const shim = appleShimFolder();
  const links = scratch("git-links");
  symlinkSync("/bin/sh", join(links, "git"));

  const chosen = chooseGit(`${shim}:${process.env.PATH}`, links);

  expect(chosen).toEqual({ binary: xcrunAnswer, path: `${links}:${shim}:${process.env.PATH}` });
  expect(readlinkSync(join(links, "git"))).toBe(xcrunAnswer);
  const lookup = Bun.spawnSync(["/bin/sh", "-c", "command -v git"], { env: { PATH: chosen.path } });
  expect(realpathSync(lookup.stdout.toString().trim())).toBe(realpathSync(xcrunAnswer));
});

test.skipIf(!onMac)("a failed xcrun answer keeps Apple's git shim from PATH", () => {
  const shim = appleShimFolder();
  const links = scratch("git-links");
  const path = `${shim}:${process.env.PATH}`;
  const saved = process.env.DEVELOPER_DIR;
  process.env.DEVELOPER_DIR = join(links, "missing");

  try {
    expect(chooseGit(path, links)).toEqual({ binary: join(shim, "git"), path });
  } finally {
    if (saved === undefined) delete process.env.DEVELOPER_DIR;
    else process.env.DEVELOPER_DIR = saved;
  }

  expect(existsSync(join(links, "git"))).toBe(false);
});

test("git returns what git prints on stdout", () => {
  const cwd = scratchGitRepository();
  expect(git(cwd, "rev-parse", "--is-inside-work-tree")).toBe("true\n");
});

test("git throws the stderr that git prints for a failed call", () => {
  const cwd = scratchGitRepository();
  const args = ["rev-parse", "--verify", "refs/heads/missing"];
  const direct = Bun.spawnSync([gitBinary, ...args], { cwd });
  expect(direct.exitCode).not.toBe(0);

  expect(() => git(cwd, ...args)).toThrow(new Error(direct.stderr.toString()));
});

test("git runs with a caller's environment in place of this process's, the test identity on top", () => {
  const cwd = scratchGitRepository();
  const env = { ...process.env, GIT_AUTHOR_NAME: "someone", GIT_AUTHOR_DATE: "@0 +0000" };
  process.env.GIT_CONFIG_COUNT = "1";
  process.env.GIT_CONFIG_KEY_0 = "stanza.probe";
  process.env.GIT_CONFIG_VALUE_0 = "set";

  expect(git(cwd, "config", "stanza.probe")).toBe("set\n");
  expect(() => git(cwd, { env }, "config", "stanza.probe")).toThrow();
  expect(git(cwd, { env }, "var", "GIT_AUTHOR_IDENT")).toBe("t <t@t> 0 +0000\n");
});

test("git takes its options only before the arguments", () => {
  const cwd = scratchGitRepository();
  expect(git(cwd, "var", "GIT_AUTHOR_IDENT")).toStartWith("t <t@t> ");
  // @ts-expect-error
  expect(() => git(cwd, "var", "GIT_AUTHOR_IDENT", { env: process.env })).toThrow();
});

const bytes = (...values: number[]) => new Uint8Array(values);

function throughShim(shim: string, calls: [string[], Uint8Array?][]): Promise<number[]> {
  return Promise.all(
    calls.map(
      ([args, stdin]) => Bun.spawn([join(shim, "git"), ...args], { stdin, stderr: "pipe" }).exited,
    ),
  );
}

test("the git shim keeps each concurrent call's arguments and stdin bytes apart", async () => {
  const shim = gitShim({ stdin: true, meet: { commands: ["a b.ts", "n\nl.ts"], count: 2 } });
  const calls: [string[], Uint8Array][] = [
    [["a b.ts", ""], bytes(0x61, 0x00, 0x62, 0x0a)],
    [["n\nl.ts"], bytes(0x63, 0x0a)],
  ];

  expect(await throughShim(shim.bin, calls)).toEqual([1, 1]);

  const records = shim.records().toSorted((a, b) => a.args.length - b.args.length);
  expect(records).toEqual([
    { args: ["n\nl.ts"], stdin: bytes(0x63, 0x0a) },
    { args: ["a b.ts", ""], stdin: bytes(0x61, 0x00, 0x62, 0x0a) },
  ]);
});

test("the git shim hands git the stdin bytes it captured", () => {
  const shim = gitShim({ stdin: true });
  const stdin = bytes(0x61, 0x00, 0x62, 0x0a);
  const hashed = (binary: string) => {
    const result = Bun.spawnSync([binary, "hash-object", "--stdin"], { stdin });
    expect(result.exitCode).toBe(0);
    return result.stdout.toString();
  };

  const expected = hashed(gitBinary);
  expect(expected).toMatch(/^([0-9a-f]{40}|[0-9a-f]{64})\n$/);
  expect(hashed(join(shim.bin, "git"))).toBe(expected);
  expect(shim.records()).toEqual([{ args: ["hash-object", "--stdin"], stdin }]);
});

test("the git shim fails every call that meets no other and lets concurrent calls through", async () => {
  const shim = gitShim({ meet: { commands: ["version"], count: 2 } });
  const alone = () => Bun.spawnSync([join(shim.bin, "git"), "version"]);

  const first = alone();
  expect(first.exitCode).toBe(125);
  expect(first.stderr.toString()).toBe("git shim: no other call met this one\n");

  expect(await throughShim(shim.bin, [[["version"]], [["version"]]])).toEqual([0, 0]);
  expect(alone().exitCode).toBe(125);
  expect(await throughShim(shim.bin, [[["-C", "/", "version"]], [["version"]]])).toEqual([0, 0]);
  expect(shim.calls()).toHaveLength(6);
  expect(() => gitShim({ meet: { commands: ["version"], count: 1 } })).toThrow("two to 64 calls");
}, 15_000);

test("the git shim hides attribute metadata from var -l and passes the rest", () => {
  const cwd = scratchGitRepository();
  const shim = gitShim({ hideAttributeMetadata: true });
  const listed = (binary: string) => {
    const result = Bun.spawnSync([binary, "-c", "stanza.probe=GIT_ATTR_kept", "var", "-l"], {
      cwd,
    });

    expect(result.exitCode).toBe(0);
    return result.stdout
      .toString()
      .split("\n")
      .filter((line) => !line.includes("_IDENT="));
  };

  const shown = listed(join(shim.bin, "git"));
  expect(shown).toContain("stanza.probe=GIT_ATTR_kept");
  expect(shown).toEqual(listed(gitBinary).filter((line) => !line.startsWith("GIT_ATTR_")));
});

test("the git shim refuses a global flag the way an older git does and runs every other call", () => {
  const cwd = scratchGitRepository();
  const shim = gitShim({ refuse: "--attr-source" });
  const shimmed = (...args: string[]) => Bun.spawnSync([join(shim.bin, "git"), ...args], { cwd });

  const refused = shimmed("--attr-source=HEAD", "version");
  expect(refused.exitCode).toBe(129);
  expect(refused.stderr.toString()).toBe("unknown option: --attr-source=HEAD\n");

  expect(shimmed("version").exitCode).toBe(0);
  expect(shimmed("-c", "a.b=--attr-source=x", "version").exitCode).toBe(0);
  expect(shimmed("ls-files", "--", "--attr-source=HEAD").exitCode).toBe(0);
  expect(shim.calls()).toEqual([
    "--attr-source=HEAD version",
    "version",
    "-c a.b=--attr-source=x version",
    "ls-files -- --attr-source=HEAD",
  ]);
});

function signingTrap(): { config: string; log: string } {
  const folder = scratch("signing");
  const log = join(folder, "signed.log");
  const program = join(folder, "sign");
  const config = join(folder, "gitconfig");
  writeFileSync(program, `#!/bin/sh\necho "$@" >> "$(dirname "$0")/signed.log"\nexit 1\n`, {
    mode: 0o755,
  });

  writeFileSync(
    config,
    `[commit]\n\tgpgsign = true\n[gpg]\n\tformat = ssh\n[gpg "ssh"]\n\tprogram = "${program}"\n[user]\n\tsigningkey = key::ssh-ed25519 AAAA\n`,
  );

  return { config, log };
}

test("every builder commits unsigned under a global config that signs commits", () => {
  const trap = signingTrap();
  process.env.GIT_CONFIG_GLOBAL = trap.config;
  process.env.GIT_CONFIG_COUNT = "0";

  const staged = scratchGitRepository({ files: { "a.ts": "export {};\n" }, staged: true });
  const committed = [superproject({ "a.ts": "export {};\n" }), committedSource("export {};\n")];
  for (const cwd of committed) expect(git(cwd, "log", "--format=%s")).toBe("init\n");

  expect(existsSync(trap.log)).toBe(false);

  const signed = Bun.spawnSync(
    [gitBinary, "-c", "user.name=t", "-c", "user.email=t@t", "commit", "-qm", "signed"],
    { cwd: staged, env: process.env },
  );

  expect(signed.exitCode).not.toBe(0);
  expect(existsSync(trap.log)).toBe(true);
});
