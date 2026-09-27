import { afterEach } from "bun:test";
import { mkdirSync, mkdtempSync, realpathSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join } from "node:path";
import { main } from "../src/cli.ts";

export const cli = join(import.meta.dir, "..", "src", "cli.ts");

const directories: string[] = [];

afterEach(() => {
  for (const path of directories.splice(0)) rmSync(path, { recursive: true, force: true });
});

export function scratch(suffix = ""): string {
  const path = realpathSync(mkdtempSync(join(tmpdir(), `stanza-${suffix ? `${suffix}-` : ""}`)));
  directories.push(path);
  return path;
}

interface ScratchGitRepositoryOptions {
  files?: Record<string, string>;
  staged?: boolean | string[];
}

export function scratchGitRepository(options: ScratchGitRepositoryOptions = {}): string {
  const cwd = scratch("repo");
  const result = Bun.spawnSync(["git", "init", "-q"], { cwd });
  if (result.exitCode !== 0) throw new Error(new TextDecoder().decode(result.stderr));

  for (const [name, text] of Object.entries(options.files ?? {})) {
    const path = join(cwd, name);
    mkdirSync(dirname(path), { recursive: true });
    writeFileSync(path, text);
  }

  if (options.staged) {
    const staged = Array.isArray(options.staged)
      ? options.staged
      : Object.keys(options.files ?? {});

    const added = Bun.spawnSync(["git", "add", "--", ...staged], { cwd });
    if (added.exitCode !== 0) throw new Error(new TextDecoder().decode(added.stderr));
  }

  return cwd;
}

interface RunOptions {
  cwd?: string;
  env?: NodeJS.ProcessEnv;
  stdin?: Uint8Array;
}

interface RunResult {
  code: number;
  stderr: string;
  stdout: string;
}

function parse(input: (RunOptions | string)[]): { options: RunOptions; args: string[] } {
  const [first] = input;
  const options = typeof first === "object" ? first : {};
  const args = input.filter((item): item is string => typeof item === "string");
  return { options, args };
}

export function spawnCli(...input: (RunOptions | string)[]): RunResult {
  const { options, args } = parse(input);
  const env = { ...(options.env ?? process.env), FORCE_COLOR: undefined };
  const result = Bun.spawnSync(["bun", "run", cli, ...args], { ...options, env });

  const decoder = new TextDecoder();
  return {
    code: result.exitCode,
    stderr: decoder.decode(result.stderr),
    stdout: decoder.decode(result.stdout),
  };
}

function withEnvironment<T>(env: NodeJS.ProcessEnv, body: () => T): T {
  const saved = { ...process.env };
  for (const key of Object.keys(process.env)) if (!(key in env)) delete process.env[key];

  Object.assign(process.env, env);

  try {
    return body();
  } finally {
    for (const key of Object.keys(process.env)) if (!(key in saved)) delete process.env[key];
    Object.assign(process.env, saved);
  }
}

export function runMain(argv: string[], options: RunOptions = {}): RunResult {
  const env = Object.fromEntries(
    Object.entries(options.env ?? process.env).filter(([key]) => key !== "FORCE_COLOR"),
  );

  const decoder = new TextDecoder();
  let stdout = "";
  let stderr = "";

  const code = withEnvironment(env, () =>
    main(argv, {
      cwd: options.cwd ?? process.cwd(),
      env,
      stdin: () => options.stdin ?? new Uint8Array(),
      stdout: (chunk) => {
        stdout += typeof chunk === "string" ? chunk : decoder.decode(chunk);
      },
      stderr: (chunk) => {
        stderr += typeof chunk === "string" ? chunk : decoder.decode(chunk);
      },
    }),
  );

  return { code, stderr, stdout };
}

export function run(...input: (RunOptions | string)[]): RunResult {
  const { options, args } = parse(input);
  return runMain(args, options);
}
