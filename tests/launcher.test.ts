import { expect, test } from "bun:test";
import { chmodSync, copyFileSync, mkdirSync, symlinkSync, writeFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { hostPlatform } from "../scripts/platform.ts";
import { scratch, stanzaBinary } from "./support.ts";

const root = join(import.meta.dir, "..");
const launcher = join(root, "src", "launcher");
const smoke = join(root, "scripts", "smoke.sh");

const host = hostPlatform();
const releases = "https://github.com/ryuudotgg/stanza/releases/latest";
const otherLibc = host.endsWith("-musl") ? host.slice(0, -"-musl".length) : `${host}-musl`;

const stub = `#!/bin/sh
for arg in "$@"; do printf '[%s]\\n' "$arg"; done
[ -n "\${STUB_WAIT:-}" ] && echo "$$" && exec sleep 30
cat
[ -n "\${STUB_SIGNAL:-}" ] && kill -s "$STUB_SIGNAL" $$
exit "\${STUB_EXIT:-0}"
`;

interface Install {
  binary?: string;
  nested?: boolean;
}

function install(platforms: string[], { binary, nested = false }: Install = {}): string {
  const modules = join(scratch("launcher"), "node_modules");
  const main = join(modules, "@ryuugg", "stanza");

  mkdirSync(join(main, "src"), { recursive: true });
  writeFileSync(join(main, "package.json"), JSON.stringify({ name: "@ryuugg/stanza" }));
  copyFileSync(launcher, join(main, "src", "launcher"));
  chmodSync(join(main, "src", "launcher"), 0o755);

  for (const platform of platforms) {
    const directory = join(
      nested ? join(main, "node_modules") : modules,
      "@ryuugg",
      `stanza-${platform}`,
    );

    mkdirSync(directory, { recursive: true });
    writeFileSync(
      join(directory, "package.json"),
      JSON.stringify({ name: `@ryuugg/stanza-${platform}` }),
    );

    const target = join(directory, "stanza");
    if (binary) copyFileSync(binary, target);
    else writeFileSync(target, stub);

    chmodSync(target, 0o755);
  }

  mkdirSync(join(modules, ".bin"));
  symlinkSync(join("..", "@ryuugg", "stanza", "src", "launcher"), join(modules, ".bin", "stanza"));
  return join(modules, ".bin", "stanza");
}

function withoutRuntimes(): string {
  const path = scratch("no-runtime");
  for (const tool of ["cat", "readlink", "uname"]) symlinkSync(Bun.which(tool)!, join(path, tool));
  return path;
}

const runtimes = ["bun", "node"].filter((runtime) => Bun.which(runtime));

interface Launch {
  env?: Record<string, string>;
  stdin?: string;
}

function launch(runtime: string, bin: string, args: string[], options: Launch = {}) {
  const command = runtime === "sh" ? ["/bin/sh", bin] : [Bun.which(runtime)!, bin];
  const env = runtime === "sh" ? { PATH: withoutRuntimes() } : process.env;
  const result = Bun.spawnSync([...command, ...args], {
    env: { ...env, ...options.env },
    stdin: options.stdin === undefined ? "ignore" : new TextEncoder().encode(options.stdin),
  });

  return {
    code: result.exitCode,
    signal: result.signalCode ?? null,
    stdout: result.stdout.toString(),
    stderr: result.stderr.toString(),
  };
}

const passes = [...runtimes, "sh"];

test.each(passes)(
  "under %s the launcher runs the platform binary with the same arguments, stdin and exit code",
  (runtime) => {
    const bin = install([host]);
    const result = launch(runtime, bin, ["--check", "a b.ts", ""], {
      stdin: "piped\n",
      env: { STUB_EXIT: "3" },
    });

    expect(result).toEqual({
      code: 3,
      signal: null,
      stdout: "[--check]\n[a b.ts]\n[]\npiped\n",
      stderr: "",
    });
  },
);

test.each(passes)("under %s the launcher dies of the signal that killed the binary", (runtime) => {
  const bin = install([host]);
  const result = launch(runtime, bin, [], { env: { STUB_SIGNAL: "TERM" } });
  expect(result.signal).toBe("SIGTERM");
});

test.skipIf(process.platform !== "linux").each(passes)(
  "on Linux under %s the launcher takes the other libc's package when its own is missing",
  (runtime) => {
    const bin = install([otherLibc]);
    expect(launch(runtime, bin, ["ok"]).stdout).toBe("[ok]\n");
  },
);

test.each(passes)(
  "under %s the launcher runs the platform package nested under its own before a sibling",
  (runtime) => {
    const bin = install([host], { nested: true });
    const sibling = join(dirname(dirname(bin)), "@ryuugg", `stanza-${host}`);

    mkdirSync(sibling, { recursive: true });
    writeFileSync(
      join(sibling, "package.json"),
      JSON.stringify({ name: `@ryuugg/stanza-${host}` }),
    );

    writeFileSync(join(sibling, "stanza"), "#!/bin/sh\necho sibling\n");
    chmodSync(join(sibling, "stanza"), 0o755);

    expect(launch(runtime, bin, ["ok"]).stdout).toBe("[ok]\n");
  },
);

test.each(runtimes)(
  "under %s a signal sent to the launcher reaches the binary",
  async (runtime) => {
    const bin = install([host]);
    const launched = Bun.spawn([Bun.which(runtime)!, bin], {
      env: { ...process.env, STUB_WAIT: "1" },
      stdout: "pipe",
    });

    const reader = launched.stdout.getReader();
    const pid = Number(new TextDecoder().decode((await reader.read()).value).trim());

    launched.kill("SIGINT");
    await launched.exited;

    expect(launched.signalCode).toBe("SIGINT");
    expect(() => process.kill(pid, 0)).toThrow();
  },
);

test.each(runtimes)(
  "under %s the launcher names the platform and the release archives when no package matches",
  (runtime) => {
    const bin = install([]);
    const expected = `stanza: no binary package for ${host}: install @ryuugg/stanza-${host}, or download stanza-${host} from ${releases}\n`;

    expect(launch(runtime, bin, ["--version"])).toEqual({
      code: 2,
      signal: null,
      stdout: "",
      stderr: expected,
    });

    expect(launch(runtime, bin, ["hook"])).toEqual({
      code: 1,
      signal: null,
      stdout: "",
      stderr: expected,
    });
  },
);

test("with no package and no Node or Bun, the launcher says what it needs", () => {
  const bin = install([]);
  const message = `stanza: needs Node.js or Bun to start, or a release binary (${releases})\n`;

  expect(launch("sh", bin, ["--version"])).toEqual({
    code: 2,
    signal: null,
    stdout: "",
    stderr: message,
  });

  expect(launch("sh", bin, ["hook"])).toEqual({
    code: 1,
    signal: null,
    stdout: "",
    stderr: message,
  });
});

const binary = stanzaBinary();
const modes = ["--without-bun", "--without-node"].filter((mode) =>
  Bun.which(mode === "--without-bun" ? "node" : "bun"),
);

test.each(modes)(
  "the installed launcher passes the smoke test %s",
  (mode) => {
    const bin = install([host], { binary });
    const result = Bun.spawnSync([smoke, mode, bin], { env: process.env });
    expect(result.exitCode, result.stderr.toString()).toBe(0);
  },
  60_000,
);

test.each(runtimes)("under %s the launcher runs the real binary", (runtime) => {
  const bin = install([host], { binary, nested: true });
  const direct = Bun.spawnSync([binary, "--version"]);

  expect(launch(runtime, bin, ["--version"])).toEqual({
    code: 0,
    signal: null,
    stdout: direct.stdout.toString(),
    stderr: "",
  });
});
