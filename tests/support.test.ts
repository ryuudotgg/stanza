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
  run,
  scratch,
  scratchGitRepository,
  superproject,
} from "./support.ts";

const saved = Object.fromEntries(
  [
    "STANZA_BIN",
    "PROBE",
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

function fakeBinary(body = ""): string {
  const path = join(scratch("fake-bin"), "stanza");
  writeFileSync(
    path,
    `#!/bin/sh\n${body}for arg in "$@"; do printf 'arg %s\\n' "$arg"; done\nprintf 'cwd %s\\n' "$(pwd -P)"\nprintf 'probe %s\\n' "\${PROBE-unset}"\nprintf 'color %s\\n' "\${FORCE_COLOR-unset}"\nprintf 'agent %s\\n' "\${CLAUDECODE-unset}"\nprintf 'stdin %s\\n' "$(cat)"\necho oops >&2\nexit 7\n`,
  );

  chmodSync(path, 0o755);
  return path;
}

const stdin = new TextEncoder().encode("piped");
const env = { ...process.env, PROBE: "set", FORCE_COLOR: "3", CLAUDECODE: "1" };

test("run spawns STANZA_BIN with the same arguments, cwd, environment and stdin", () => {
  process.env.STANZA_BIN = fakeBinary();
  const cwd = scratch("fake-cwd");

  expect(run({ cwd, env, stdin }, "--check", "a b.ts", "c.ts")).toEqual({
    code: 7,
    stdout: `arg --check\narg a b.ts\narg c.ts\ncwd ${cwd}\nprobe set\ncolor unset\nagent unset\nstdin piped\n`,
    stderr: "oops\n",
  });
});

test("run defaults to this process's cwd and environment and an empty stdin", () => {
  process.env.STANZA_BIN = fakeBinary();
  process.env.PROBE = "inherited";
  process.env.FORCE_COLOR = "3";

  const expected = {
    code: 7,
    stdout: `arg --check\ncwd ${realpathSync(process.cwd())}\nprobe inherited\ncolor unset\nagent unset\nstdin \n`,
    stderr: "oops\n",
  };

  expect(run("--check")).toEqual(expected);
  expect(run({}, "--check")).toEqual(expected);
});

test("a STANZA_BIN killed by a signal fails the helper instead of returning an exit code", () => {
  process.env.STANZA_BIN = fakeBinary("kill -SEGV $$\n");
  expect(() => run("--check")).toThrow("was killed by SIGSEGV");
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
