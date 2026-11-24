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
import { chooseGit, gitBinary, gitLinks, run, runMain, scratch } from "./support.ts";

const saved = {
  bin: process.env.STANZA_BIN,
  probe: process.env.PROBE,
  color: process.env.FORCE_COLOR,
};

afterEach(() => {
  for (const [key, value] of [
    ["STANZA_BIN", saved.bin],
    ["PROBE", saved.probe],
    ["FORCE_COLOR", saved.color],
  ] as const)
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

const helpers = {
  run: (cwd: string) => run({ cwd, env, stdin }, "--check", "a b.ts", "c.ts"),
  runMain: (cwd: string) => runMain(["--check", "a b.ts", "c.ts"], { cwd, env, stdin }),
};

for (const [name, call] of Object.entries(helpers))
  test(`${name} spawns STANZA_BIN with the same arguments, cwd, environment and stdin`, () => {
    process.env.STANZA_BIN = fakeBinary();
    const cwd = scratch("fake-cwd");

    expect(call(cwd)).toEqual({
      code: 7,
      stdout: `arg --check\narg a b.ts\narg c.ts\ncwd ${cwd}\nprobe set\ncolor unset\nagent unset\nstdin piped\n`,
      stderr: "oops\n",
    });
  });

test("run and runMain default to this process's cwd and environment and an empty stdin", () => {
  process.env.STANZA_BIN = fakeBinary();
  process.env.PROBE = "inherited";
  process.env.FORCE_COLOR = "3";

  const expected = {
    code: 7,
    stdout: `arg --check\ncwd ${realpathSync(process.cwd())}\nprobe inherited\ncolor unset\nagent unset\nstdin \n`,
    stderr: "oops\n",
  };

  expect(run("--check")).toEqual(expected);
  expect(runMain(["--check"])).toEqual(expected);
});

test("a STANZA_BIN killed by a signal fails the helper instead of returning an exit code", () => {
  process.env.STANZA_BIN = fakeBinary("kill -SEGV $$\n");
  expect(() => run("--check")).toThrow("was killed by SIGSEGV");
});

test("a STANZA_BIN naming no file fails the helper before it spawns", () => {
  process.env.STANZA_BIN = join(scratch("missing-bin"), "stanza");
  expect(() => run("--check")).toThrow("STANZA_BIN names no file");
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
