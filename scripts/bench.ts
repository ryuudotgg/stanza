import {
  chmodSync,
  cpSync,
  existsSync,
  mkdirSync,
  mkdtempSync,
  readFileSync,
  rmSync,
  writeFileSync,
} from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join, resolve } from "node:path";

const root = join(import.meta.dir, "..");
const stanza = resolve(process.env.STANZA ?? join(root, "bin", "stanza"));
const baseStanza = process.env.STANZA_BASE ? resolve(process.env.STANZA_BASE) : undefined;
const showSpawns = process.env.SPAWNS === "1";

const repo = process.env.STANZA_REPO;
if (!repo) throw new Error("STANZA_REPO: path to a repo to benchmark against");

const runs = Number(process.env.RUNS ?? 10);

const git = Bun.which("git");
if (!git) throw new Error("git not found on PATH");

const work = mkdtempSync(join(tmpdir(), "stanza-bench-"));
const exportDir = join(work, "repo");
const fifty = join(work, "fifty");
const one = join(work, "one");

const shim = join(work, "shim");
const spawnLog = join(work, "spawns.log");

const cleanUp = () => rmSync(work, { recursive: true, force: true });

process.on("SIGINT", () => {
  cleanUp();
  process.exit(130);
});

function ending(result: { exitCode: number | null; signalCode?: string | null }): string {
  return result.signalCode ?? `exit ${result.exitCode}`;
}

function run(command: string[], cwd?: string): Buffer {
  const result = Bun.spawnSync(command, { cwd, stderr: "inherit" });
  if (!result.success) throw new Error(`${command.join(" ")} failed with ${ending(result)}`);
  return result.stdout;
}

function exportHead(repoPath: string, into: string): string[] {
  const tarFile = join(work, "export.tar");

  run(["git", "-C", repoPath, "archive", "--output", tarFile, "HEAD"]);
  run(["tar", "-xf", tarFile, "-C", into]);

  const files = run(["tar", "-tf", tarFile]).toString().split("\n");
  rmSync(tarFile);
  return files;
}

function commitExport(dir: string): void {
  const identity = [
    "-c",
    "user.name=bench",
    "-c",
    "user.email=bench@localhost",
    "-c",
    "commit.gpgsign=false",
  ];

  run(["git", "init", "-q"], dir);
  run(["git", "add", "-A"], dir);
  run(["git", ...identity, "commit", "-q", "--no-verify", "-m", "export"], dir);
}

function isFindingList(text: string): boolean {
  try {
    return Array.isArray(JSON.parse(text));
  } catch {
    return false;
  }
}

function measure(label: string, path: string): void {
  const times: number[] = [];
  for (let run = 0; run < runs; run++) {
    const start = performance.now();
    const result = Bun.spawnSync([stanza, "--check", "--json", path]);
    const elapsed = performance.now() - start;

    const output = result.stdout.toString();
    if ((result.exitCode !== 0 && result.exitCode !== 1) || !isFindingList(output))
      throw new Error(
        `stanza --check ${path} failed with ${ending(result)}\n${result.stderr.toString()}${output}`,
      );

    times.push(elapsed);
  }

  const best = Math.min(...times).toFixed(1);
  const mean = (times.reduce((sum, time) => sum + time, 0) / times.length).toFixed(1);

  console.log(
    `${label.padEnd(28)} best ${best.padStart(6)} ms  mean ${mean.padStart(6)} ms  (${runs} runs)`,
  );
}

interface Call {
  label: string;
  args: string[];
  stdin?: string;
  prepare?: () => void;
}

function median(times: number[]): number {
  const ordered = times.toSorted((left, right) => left - right);
  const middle = Math.floor(ordered.length / 2);
  return ordered.length % 2 === 1
    ? ordered[middle]!
    : (ordered[middle - 1]! + ordered[middle]!) / 2;
}

function invoke(binary: string, call: Call, path: string): number {
  call.prepare?.();

  const env = { ...process.env, PATH: path, AGENT_HOOKS: undefined };
  const stdin = call.stdin === undefined ? undefined : Buffer.from(call.stdin);
  const start = performance.now();
  const result = Bun.spawnSync([binary, ...call.args], { cwd: exportDir, env, stdin });
  const elapsed = performance.now() - start;

  if (result.exitCode !== 0)
    throw new Error(`${call.label} failed with ${ending(result)}\n${result.stderr.toString()}`);

  return elapsed;
}

function spawnsOf(binary: string, call: Call): string[] {
  writeFileSync(spawnLog, "");
  invoke(binary, call, `${shim}:${process.env.PATH}`);
  return readFileSync(spawnLog, "utf8").split("\n").filter(Boolean);
}

function measureCall(call: Call): void {
  const binaries = baseStanza === undefined ? [stanza] : [baseStanza, stanza];
  const times = binaries.map((): number[] => []);

  for (let run = 0; run < runs; run++)
    binaries.forEach((binary, index) =>
      times[index]!.push(invoke(binary, call, process.env.PATH ?? "")),
    );

  binaries.forEach((binary, index) => {
    const label = binary === baseStanza ? `${call.label} (base)` : call.label;
    const spawns = spawnsOf(binary, call);
    const middle = median(times[index]!).toFixed(1);

    console.log(
      `${label.padEnd(35)} median ${middle.padStart(6)} ms  git spawns ${String(spawns.length).padStart(2)}  (${runs} runs)`,
    );

    if (showSpawns) for (const spawn of spawns) console.log(`    git ${spawn}`);
  });
}

function hookCalls(sample: string[]): Call[] {
  const paths = sample.map((file) => join(exportDir, file));
  const pristine = new Map(paths.map((path) => [path, readFileSync(path, "utf8")]));
  const edit = (path: string) => writeFileSync(path, `${pristine.get(path)}\nexport {};\n`);
  const [first] = paths;
  if (first === undefined) throw new Error(`${repo} has no TypeScript files to sample`);

  const stopped = paths.slice(0, 20);
  const transcript = join(work, "transcript.jsonl");
  const records = stopped.map((path, index) =>
    JSON.stringify({
      type: "assistant",
      message: {
        content: [
          { type: "tool_use", id: `write-${index}`, name: "Write", input: { file_path: path } },
        ],
      },
    }),
  );

  writeFileSync(transcript, `${records.join("\n")}\n`);

  const postEdit = (path: string) =>
    JSON.stringify({
      hook_event_name: "PostToolUse",
      cwd: exportDir,
      tool_name: "Edit",
      tool_input: { file_path: path, old_string: "a", new_string: "a" },
    });

  return [
    { label: "--version", args: ["--version"] },
    {
      label: "hook PreToolUse Write",
      args: ["hook"],
      stdin: JSON.stringify({
        hook_event_name: "PreToolUse",
        cwd: exportDir,
        tool_name: "Write",
        tool_input: { file_path: first, content: pristine.get(first) },
      }),
    },
    {
      label: "hook PostToolUse Edit",
      args: ["hook"],
      stdin: postEdit(first),
      prepare: () => edit(first),
    },
    {
      label: "hook Edit package.json",
      args: ["hook"],
      stdin: postEdit(join(exportDir, "package.json")),
    },
    {
      label: `hook Stop ${stopped.length} written`,
      args: ["hook"],
      stdin: JSON.stringify({
        hook_event_name: "Stop",
        cwd: exportDir,
        transcript_path: transcript,
      }),
      prepare: () => stopped.forEach(edit),
    },
  ];
}

try {
  for (const dir of [exportDir, fifty, one, shim]) mkdirSync(dir, { recursive: true });

  writeFileSync(join(shim, "git"), `#!/bin/sh\necho "$*" >> '${spawnLog}'\nexec '${git}' "$@"\n`);
  chmodSync(join(shim, "git"), 0o755);

  const sample = exportHead(repo, exportDir)
    .filter((file) => /\.tsx?$/.test(file) && !/\.d\.ts$|\.gen\.ts$|\/migrations\//.test(file))
    .slice(0, 50);

  if (!existsSync(join(exportDir, "package.json")))
    writeFileSync(join(exportDir, "package.json"), "{}\n");

  for (const file of sample) {
    mkdirSync(join(fifty, dirname(file)), { recursive: true });
    cpSync(join(exportDir, file), join(fifty, file));
  }

  commitExport(exportDir);
  writeFileSync(join(one, "one.ts"), "export function one(a: number) {\n  return a;\n}\n");

  measure("startup (one tiny file)", one);
  measure("50 files", fifty);
  measure("whole repo export", exportDir);

  for (const call of hookCalls(sample)) measureCall(call);
} finally {
  cleanUp();
}
