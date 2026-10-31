import { afterEach, expect, test } from "bun:test";
import { chmodSync, realpathSync, writeFileSync } from "node:fs";
import { join } from "node:path";
import { run, runMain, scratch, spawnCli } from "./support.ts";

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
    `#!/bin/sh\n${body}for arg in "$@"; do printf 'arg %s\\n' "$arg"; done\nprintf 'cwd %s\\n' "$(pwd -P)"\nprintf 'probe %s\\n' "\${PROBE-unset}"\nprintf 'color %s\\n' "\${FORCE_COLOR-unset}"\nprintf 'stdin %s\\n' "$(cat)"\necho oops >&2\nexit 7\n`,
  );

  chmodSync(path, 0o755);
  return path;
}

const stdin = new TextEncoder().encode("piped");
const env = { ...process.env, PROBE: "set", FORCE_COLOR: "3" };

const helpers = {
  run: (cwd: string) => run({ cwd, env, stdin }, "--check", "a b.ts", "c.ts"),
  runMain: (cwd: string) => runMain(["--check", "a b.ts", "c.ts"], { cwd, env, stdin }),
  spawnCli: (cwd: string) => spawnCli({ cwd, env, stdin }, "--check", "a b.ts", "c.ts"),
};

for (const [name, call] of Object.entries(helpers))
  test(`${name} spawns STANZA_BIN with the same arguments, cwd, environment and stdin`, () => {
    process.env.STANZA_BIN = fakeBinary();
    const cwd = scratch("fake-cwd");

    expect(call(cwd)).toEqual({
      code: 7,
      stdout: `arg --check\narg a b.ts\narg c.ts\ncwd ${cwd}\nprobe set\ncolor unset\nstdin piped\n`,
      stderr: "oops\n",
    });
  });

test("run and runMain default to this process's cwd and environment and an empty stdin", () => {
  process.env.STANZA_BIN = fakeBinary();
  process.env.PROBE = "inherited";
  process.env.FORCE_COLOR = "3";

  const expected = {
    code: 7,
    stdout: `arg --check\ncwd ${realpathSync(process.cwd())}\nprobe inherited\ncolor unset\nstdin \n`,
    stderr: "oops\n",
  };

  expect(run("--check")).toEqual(expected);
  expect(runMain(["--check"])).toEqual(expected);
});

test("a STANZA_BIN killed by a signal fails the helper instead of returning an exit code", () => {
  process.env.STANZA_BIN = fakeBinary("kill -SEGV $$\n");
  expect(() => run("--check")).toThrow("was killed by SIGSEGV");
});
