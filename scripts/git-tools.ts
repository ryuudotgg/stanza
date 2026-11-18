import { existsSync, mkdirSync, readdirSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { join } from "node:path";

function shellQuoted(text: string): string {
  return `'${text.replaceAll("'", `'\\''`)}'`;
}

function run(argv: string[], cwd?: string): Buffer {
  const result = Bun.spawnSync(argv, { cwd, stderr: "pipe" });
  if (!result.success) throw new Error(`${argv.join(" ")} failed: ${result.stderr.toString()}`);
  return result.stdout;
}

export function traceShim(dir: string, git: string): string {
  const trace = join(dir, "calls");
  mkdirSync(trace);

  // A shell may split one printf into several writes, so each process appends to its own file, named by its PID, and no two live calls share a writer.
  writeFileSync(
    join(dir, "git"),
    `#!/bin/sh\nprintf '%s\\000' "$#" "$@" >> ${shellQuoted(trace)}/$$\nexec ${shellQuoted(git)} "$@"\n`,
    { mode: 0o755 },
  );

  return trace;
}

function records(path: string): string[][] {
  const text = readFileSync(path, "utf8");
  if (!text.endsWith("\0")) throw new Error(`${path}: torn git record at the end`);

  const fields = text.slice(0, -1).split("\0");
  const calls: string[][] = [];

  let at = 0;
  while (at < fields.length) {
    const count = fields[at]!;
    const end = at + 1 + Number(count);
    if (!/^\d+$/.test(count) || end > fields.length)
      throw new Error(`${path}: torn git record at field ${at}`);

    calls.push(fields.slice(at + 1, end));
    at = end;
  }

  return calls;
}

export function takeTrace(trace: string): string[][] {
  if (!existsSync(trace)) return [];

  return readdirSync(trace).flatMap((name) => {
    const path = join(trace, name);
    const calls = records(path);
    rmSync(path);
    return calls;
  });
}

export function shownCall(args: string[]): string {
  const shown = args.map((arg) => (/^[^\s"\\]+$/.test(arg) ? arg : JSON.stringify(arg)));
  return ["git", ...shown].join(" ");
}

export function exportHead(repository: string, into: string): string[] {
  const archive = `${into}.tar`;

  run(["git", "-C", repository, "archive", "--output", archive, "HEAD"]);
  run(["tar", "-xf", archive, "-C", into]);

  const files = run(["tar", "-tf", archive]).toString().split("\n");
  rmSync(archive);
  return files;
}

export function commitTree(dir: string): void {
  if (!existsSync(join(dir, ".git"))) run(["git", "init", "-q"], dir);

  const identity = [
    "-c",
    "user.name=scratch",
    "-c",
    "user.email=scratch@stanza.invalid",
    "-c",
    "commit.gpgsign=false",
  ];

  run(["git", "add", "-A"], dir);
  run(["git", ...identity, "commit", "-q", "--no-verify", "--allow-empty", "-m", "scratch"], dir);
}
