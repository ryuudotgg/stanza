import { cpSync, mkdirSync, rmSync, writeFileSync } from "node:fs";
import { dirname, join } from "node:path";

const root = join(import.meta.dir, "..");
const stanza = join(root, "bin", "stanza");

const repo = process.env.STANZA_REPO;
if (!repo) throw new Error("STANZA_REPO: path to a repo to benchmark against");

const runs = Number(process.env.RUNS ?? 10);

const work = "/tmp/stanza-bench";
const exportDir = join(work, "repo");
const fifty = join(work, "fifty");
const one = join(work, "one");

rmSync(work, { recursive: true, force: true });
for (const dir of [exportDir, fifty, one]) mkdirSync(dir, { recursive: true });

await Bun.$`git -C ${repo} archive HEAD | tar -x -C ${exportDir}`;

const tracked = (await Bun.$`git -C ${repo} ls-files`.text()).split("\n");
const sample = tracked
  .filter((file) => /\.tsx?$/.test(file) && !/\.d\.ts$|\.gen\.ts$|\/migrations\//.test(file))
  .slice(0, 50);

for (const file of sample) {
  mkdirSync(join(fifty, dirname(file)), { recursive: true });
  cpSync(join(repo, file), join(fifty, file));
}

writeFileSync(join(one, "one.ts"), "export function one(a: number) {\n  return a;\n}\n");

function measure(label: string, path: string): void {
  const times: number[] = [];
  for (let run = 0; run < runs; run++) {
    const start = performance.now();
    Bun.spawnSync([stanza, "--check", path], {
      stdout: "ignore",
      stderr: "ignore",
    });

    times.push(performance.now() - start);
  }

  const best = Math.min(...times).toFixed(1);
  const mean = (times.reduce((sum, time) => sum + time, 0) / times.length).toFixed(1);

  console.log(
    `${label.padEnd(28)} best ${best.padStart(6)} ms  mean ${mean.padStart(6)} ms  (${runs} runs)`,
  );
}

measure("startup (one tiny file)", one);
measure("50 files", fifty);
measure("whole repo export", exportDir);
