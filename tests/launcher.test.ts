import { expect, test } from "bun:test";
import {
  chmodSync,
  copyFileSync,
  existsSync,
  mkdirSync,
  readFileSync,
  symlinkSync,
  writeFileSync,
} from "node:fs";
import { dirname, join, relative } from "node:path";
import { platforms } from "../scripts/packages.ts";
import { hostPlatform } from "../scripts/platform.ts";
import { scratch, stanzaBinary } from "./support.ts";

const root = join(import.meta.dir, "..");
const launcher = join(root, "src", "launcher");
const smoke = join(root, "scripts", "smoke.sh");

const host = hostPlatform();
const releases = "https://github.com/ryuudotgg/stanza/releases/latest";
const otherLibc = host.endsWith("-musl") ? host.slice(0, -"-musl".length) : `${host}-musl`;

const loaders = {
  x64: ["/lib64/ld-linux-x86-64.so.2", "/lib/ld-musl-x86_64.so.1"],
  arm64: ["/lib/ld-linux-aarch64.so.1", "/lib/ld-musl-aarch64.so.1"],
};

const otherArch = process.arch === "x64" ? "arm64" : "x64";
const ownMusl = existsSync(loaders[process.arch as "x64" | "arm64"]?.[1] ?? "");
const preferred =
  process.platform === "linux" ? `linux-${process.arch}${ownMusl ? "-musl" : ""}` : host;

const decides =
  host === "darwin-arm64"
    ? existsSync(
        "/System/Volumes/Preboot/Cryptexes/OS/System/Library/dyld/dyld_shared_cache_arm64e",
      ) || existsSync("/System/Library/dyld/dyld_shared_cache_arm64e")
    : process.platform === "linux" && !loaders[otherArch].some((loader) => existsSync(loader));

const stub = `#!/bin/sh
for arg in "$@"; do printf '[%s]\\n' "$arg"; done
[ -n "\${STUB_WAIT:-}" ] && echo "$$" && exec sleep 30
cat
[ -n "\${STUB_SIGNAL:-}" ] && kill -s "$STUB_SIGNAL" $$
exit "\${STUB_EXIT:-0}"
`;

const layouts = {
  local: { modules: "node_modules", bin: "node_modules/.bin" },
  "npm global": { modules: "lib/node_modules", bin: "bin" },
  "bun global": { modules: "install/global/node_modules", bin: "bin" },
};

interface Install {
  binary?: string;
  nested?: boolean;
  named?: boolean;
  layout?: keyof typeof layouts;
}

function install(
  platforms: string[],
  { binary, nested = false, named = false, layout = "local" }: Install = {},
): string {
  const prefix = scratch("launcher");
  const modules = join(prefix, layouts[layout].modules);
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
    else writeFileSync(target, named ? `#!/bin/sh\necho ${platform}\n` : stub);

    chmodSync(target, 0o755);
  }

  const bin = join(prefix, layouts[layout].bin);
  mkdirSync(bin, { recursive: true });
  symlinkSync(relative(bin, join(main, "src", "launcher")), join(bin, "stanza"));
  return join(bin, "stanza");
}

function withoutRuntimes(spawns?: string): string {
  const path = scratch("no-runtime");
  symlinkSync(Bun.which("cat")!, join(path, "cat"));

  for (const tool of ["readlink", "uname"]) {
    const shim = join(path, tool);
    const log = spawns ? `echo ${tool} >> '${spawns}'\n` : "";
    writeFileSync(shim, `#!/bin/sh\n${log}exec '${Bun.which(tool)}' "$@"\n`);
    chmodSync(shim, 0o755);
  }

  return path;
}

const runtimes = ["bun", "node"].filter((runtime) => Bun.which(runtime));

interface Launch {
  env?: Record<string, string>;
  stdin?: string;
  spawns?: string;
}

function launch(runtime: string, bin: string, args: string[], options: Launch = {}) {
  const command = runtime === "sh" ? ["/bin/sh", bin] : [Bun.which(runtime)!, bin];
  const env = runtime === "sh" ? { PATH: withoutRuntimes(options.spawns) } : process.env;
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

test.each(passes.flatMap((runtime) => Object.keys(layouts).map((layout) => [runtime, layout])))(
  "under %s the launcher runs the platform binary from the %s layout",
  (runtime, layout) => {
    const bin = install([host], { layout: layout as keyof typeof layouts });
    expect(launch(runtime, bin, ["ok"]).stdout).toBe("[ok]\n");
  },
);

test.skipIf(!decides).each(Object.keys(layouts))(
  "from the %s layout the launcher starts neither readlink nor uname",
  (layout) => {
    const bin = install([host], { layout: layout as keyof typeof layouts });
    const spawns = join(scratch("spawns"), "log");

    expect(launch("sh", bin, ["ok"], { spawns }).stdout).toBe("[ok]\n");
    expect(existsSync(spawns)).toBe(false);
  },
);

function chain(bin: string): string {
  const links = scratch("links");
  symlinkSync(bin, join(links, "first"));
  symlinkSync("first", join(links, "stanza"));
  return join(links, "stanza");
}

test.each(passes)("under %s the launcher follows a chain of symlinks to its package", (runtime) => {
  const bin = chain(install([host]));
  expect(launch(runtime, bin, ["ok"]).stdout).toBe("[ok]\n");
});

test("through a chain of symlinks outside an install layout the launcher falls back to readlink", () => {
  const bin = chain(install([host]));
  const spawns = join(scratch("spawns"), "log");

  expect(launch("sh", bin, ["ok"], { spawns }).stdout).toBe("[ok]\n");
  expect(readFileSync(spawns, "utf8")).toStartWith("readlink\nreadlink\nreadlink\n");
});

test.each(passes)(
  "under %s the launcher picks the host package from several installed side by side",
  (runtime) => {
    const bin = install(platforms, { named: true });
    expect(launch(runtime, bin, []).stdout).toBe(`${preferred}\n`);
  },
);

test.skipIf(process.platform !== "linux").each(passes)(
  "on Linux under %s the launcher prefers the package for its own libc",
  (runtime) => {
    const bin = install([otherLibc, host], { named: true });
    expect(launch(runtime, bin, []).stdout).toBe(`${preferred}\n`);
  },
);

test.skipIf(host !== "darwin-arm64").each(passes)(
  "on Apple silicon under %s the launcher runs the x64 package under Rosetta when it is the only one",
  (runtime) => {
    const bin = install(["darwin-x64"], { named: true });
    expect(launch(runtime, bin, []).stdout).toBe("darwin-x64\n");
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
    const expected = `no binary package for ${host}: install @ryuugg/stanza-${host}, or download stanza-${host} from ${releases}\n`;

    expect(launch(runtime, bin, ["--version"])).toEqual({
      code: 2,
      signal: null,
      stdout: "",
      stderr: `stanza: ${expected}`,
    });

    expect(launch(runtime, bin, ["hook"])).toEqual({
      code: 1,
      signal: null,
      stdout: "",
      stderr: `stanza hook: ${expected}`,
    });
  },
);

test.skipIf(process.getuid?.() === 0).each(runtimes)(
  "under %s the launcher names the package and the system's reason when the binary cannot start",
  (runtime) => {
    const bin = install([preferred]);
    chmodSync(join(dirname(dirname(bin)), "@ryuugg", `stanza-${preferred}`, "stanza"), 0o644);
    const expected = `cannot start @ryuugg/stanza-${preferred}: permission denied\n`;

    expect(launch(runtime, bin, ["--version"])).toEqual({
      code: 2,
      signal: null,
      stdout: "",
      stderr: `stanza: ${expected}`,
    });

    expect(launch(runtime, bin, ["hook"])).toEqual({
      code: 1,
      signal: null,
      stdout: "",
      stderr: `stanza hook: ${expected}`,
    });
  },
);

test("with no package and no Node or Bun, the launcher says what it needs", () => {
  const bin = install([]);
  const message = `needs Node.js or Bun to start, or a release binary (${releases})\n`;

  expect(launch("sh", bin, ["--version"])).toEqual({
    code: 2,
    signal: null,
    stdout: "",
    stderr: `stanza: ${message}`,
  });

  expect(launch("sh", bin, ["hook"])).toEqual({
    code: 1,
    signal: null,
    stdout: "",
    stderr: `stanza hook: ${message}`,
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
