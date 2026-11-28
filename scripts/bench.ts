import {
  cpSync,
  existsSync,
  mkdirSync,
  mkdtempSync,
  readFileSync,
  rmSync,
  writeFileSync,
} from "node:fs";
import { tmpdir } from "node:os";
import type { SyncSubprocess } from "bun";
import { dirname, join, resolve } from "node:path";
import { commitTree, exportHead, shownCall, takeTrace, traceShim } from "./git-tools.ts";

type Result = SyncSubprocess<"pipe", "pipe">;

const root = join(import.meta.dir, "..");
const stanza = resolve(process.env.STANZA_BIN ?? join(root, "target", "release", "stanza"));
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
const writeRepo = join(work, "write");

const shim = join(work, "shim");

mkdirSync(shim);
const trace = traceShim(shim, git);

const cleanUp = () => rmSync(work, { recursive: true, force: true });

process.on("SIGINT", () => {
  cleanUp();
  process.exit(130);
});

function ending(result: { exitCode: number | null; signalCode?: string | number | null }): string {
  return String(result.signalCode ?? `exit ${result.exitCode}`);
}

function isFindingList(text: string): boolean {
  try {
    return Array.isArray(JSON.parse(text));
  } catch {
    return false;
  }
}

interface Call {
  label: string;
  args: string[];
  cwd?: string;
  stdin?: string;
  prepare?: () => void;
  problem: (result: Result) => string | undefined;
}

interface Contender {
  binary: string;
  suffix: string;
}

const contenders: Contender[] = [
  ...(baseStanza === undefined ? [] : [{ binary: baseStanza, suffix: " (base)" }]),
  { binary: stanza, suffix: "" },
];

function failure(result: Result): string {
  return `${ending(result)}\n${result.stderr.toString()}${result.stdout.toString()}`;
}

function exitsZero(result: Result): string | undefined {
  return result.exitCode === 0 ? undefined : `failed with ${failure(result)}`;
}

function checkCall(label: string, path: string): Call {
  return {
    label,
    args: ["--check", "--json", path],
    problem: (result) =>
      (result.exitCode === 0 || result.exitCode === 1) && isFindingList(result.stdout.toString())
        ? undefined
        : `failed with ${failure(result)}`,
  };
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
  const result = Bun.spawnSync([binary, ...call.args], { cwd: call.cwd, env, stdin });
  const elapsed = performance.now() - start;

  const problem = call.problem(result);
  if (problem !== undefined) throw new Error(`${call.label} on ${binary} ${problem}`);

  return elapsed;
}

function spawnsOf(binary: string, call: Call): string[][] {
  invoke(binary, call, `${shim}:${process.env.PATH}`);
  return takeTrace(trace).map(({ args }) => args);
}

function measure(call: Call): void {
  const times = contenders.map((): number[] => []);

  for (let run = 0; run < runs; run++)
    contenders.forEach(({ binary }, index) =>
      times[index]!.push(invoke(binary, call, process.env.PATH ?? "")),
    );

  contenders.forEach(({ binary, suffix }, index) => {
    const label = `${call.label}${suffix}`;
    const spawns = spawnsOf(binary, call);
    const middle = median(times[index]!).toFixed(1);

    console.log(
      `${label.padEnd(35)} median ${middle.padStart(6)} ms  git spawns ${String(spawns.length).padStart(2)}  (${runs} runs)`,
    );

    if (showSpawns) for (const spawn of spawns) console.log(`    ${shownCall(spawn)}`);
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

  const written = join(writeRepo, "written.ts");
  const unformatted = "if (a) {\n  b();\n}\n";
  const formatted = "if (a) b();\n";

  const hook = (
    event: Omit<Call, "args" | "cwd" | "problem"> & Partial<Pick<Call, "cwd" | "problem">>,
  ): Call => ({
    args: ["hook"],
    cwd: exportDir,
    problem: exitsZero,
    ...event,
  });

  return [
    { label: "--version", args: ["--version"], cwd: exportDir, problem: exitsZero },
    hook({
      label: "hook PostToolUse Edit",
      stdin: postEdit(first),
      prepare: () => edit(first),
    }),
    hook({
      label: "hook Edit package.json",
      stdin: postEdit(join(exportDir, "package.json")),
    }),
    hook({
      label: `hook Stop ${stopped.length} written`,
      stdin: JSON.stringify({
        hook_event_name: "Stop",
        cwd: exportDir,
        transcript_path: transcript,
      }),
      prepare: () => stopped.forEach(edit),
    }),
    hook({
      label: "hook PostToolUse Write",
      cwd: writeRepo,
      stdin: JSON.stringify({
        hook_event_name: "PostToolUse",
        cwd: writeRepo,
        tool_name: "Write",
        tool_input: { file_path: written, content: unformatted },
      }),
      prepare: () => writeFileSync(written, unformatted),
      problem: (result) => {
        const failed = exitsZero(result);
        if (failed !== undefined) return failed;

        const text = readFileSync(written, "utf8");
        return text === formatted
          ? undefined
          : `left ${JSON.stringify(text)}, expected ${JSON.stringify(formatted)}`;
      },
    }),
  ];
}

try {
  for (const dir of [exportDir, fifty, one, writeRepo]) mkdirSync(dir, { recursive: true });

  const sample = exportHead(repo, exportDir)
    .filter((file) => /\.tsx?$/.test(file) && !/\.d\.ts$|\.gen\.ts$|\/migrations\//.test(file))
    .slice(0, 50);

  if (!existsSync(join(exportDir, "package.json")))
    writeFileSync(join(exportDir, "package.json"), "{}\n");

  for (const file of sample) {
    mkdirSync(join(fifty, dirname(file)), { recursive: true });
    cpSync(join(exportDir, file), join(fifty, file));
  }

  commitTree(exportDir);
  writeFileSync(join(writeRepo, "package.json"), "{}\n");
  commitTree(writeRepo);
  writeFileSync(join(one, "one.ts"), "export function one(a: number) {\n  return a;\n}\n");

  const calls = [
    checkCall("startup (one tiny file)", one),
    checkCall("50 files", fifty),
    checkCall("whole repo export", exportDir),
    ...hookCalls(sample),
  ];

  for (const call of calls) measure(call);
} finally {
  cleanUp();
}
