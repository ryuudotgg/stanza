import { cpSync, mkdirSync, mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join } from "node:path";

const root = join(import.meta.dir, "..");
const stanza = join(root, "bin", "stanza");

const repo = process.env.STANZA_REPO;
if (!repo) throw new Error("STANZA_REPO: path to a repo to benchmark against");

const runs = Number(process.env.RUNS ?? 10);

const work = mkdtempSync(join(tmpdir(), "stanza-bench-"));
const exportDir = join(work, "repo");
const fifty = join(work, "fifty");
const one = join(work, "one");

const cleanUp = () => rmSync(work, { recursive: true, force: true });

process.on("SIGINT", () => {
  cleanUp();
  process.exit(130);
});

function ending(result: { exitCode: number | null; signalCode?: string | null }): string {
  return result.signalCode ?? `exit ${result.exitCode}`;
}

function run(command: string[]): Buffer {
  const result = Bun.spawnSync(command, { stderr: "inherit" });
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

try {
  for (const dir of [exportDir, fifty, one]) mkdirSync(dir, { recursive: true });

  const sample = exportHead(repo, exportDir)
    .filter((file) => /\.tsx?$/.test(file) && !/\.d\.ts$|\.gen\.ts$|\/migrations\//.test(file))
    .slice(0, 50);

  for (const file of sample) {
    mkdirSync(join(fifty, dirname(file)), { recursive: true });
    cpSync(join(exportDir, file), join(fifty, file));
  }

  writeFileSync(join(one, "one.ts"), "export function one(a: number) {\n  return a;\n}\n");

  measure("startup (one tiny file)", one);
  measure("50 files", fifty);
  measure("whole repo export", exportDir);
} finally {
  cleanUp();
}
