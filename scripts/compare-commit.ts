import { mkdirSync, mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";

const root = join(import.meta.dir, "..");

const repo = process.env.STANZA_REPO;
const base = process.env.STANZA_BASE;
const target = process.env.STANZA_TARGET;
if (!repo || !base || !target)
  throw new Error(
    "STANZA_REPO, STANZA_BASE (commit) and STANZA_TARGET (commit with the style applied by hand) are required",
  );

const parent = resolve(process.env.STANZA_WORK ?? tmpdir());
mkdirSync(parent, { recursive: true });

const work = mkdtempSync(join(parent, "stanza-compare-"));
const tool = join(work, "tool");
const wanted = join(work, "target");
const findingsFile = join(work, "tool-findings.json");
const diffFile = join(work, "tool-vs-target.diff");

function ending(result: { exitCode: number | null; signalCode?: string | null }): string {
  return result.signalCode ?? `exit ${result.exitCode}`;
}

function run(command: string[], okExits: number[] = [0]): Buffer {
  const result = Bun.spawnSync(command, { stderr: "inherit" });
  if (result.exitCode === null || !okExits.includes(result.exitCode))
    throw new Error(`${command.join(" ")} failed with ${ending(result)}`);
  return result.stdout;
}

function extractCommit(repoPath: string, commit: string, into: string): void {
  const tarFile = `${into}.tar`;

  mkdirSync(into, { recursive: true });
  run(["git", "-C", repoPath, "archive", "--output", tarFile, commit]);
  run(["tar", "-xf", tarFile, "-C", into]);
  rmSync(tarFile);
}

function isFindingList(text: string): boolean {
  try {
    return Array.isArray(JSON.parse(text));
  } catch {
    return false;
  }
}

extractCommit(repo, base, tool);
extractCommit(repo, target, wanted);

const start = performance.now();

const fix = Bun.spawnSync([process.execPath, join(root, "src", "cli.ts"), "--fix", "--json", "."], {
  cwd: tool,
  stderr: "inherit",
});

const findings = fix.stdout.toString();
writeFileSync(findingsFile, findings);

if ((fix.exitCode !== 0 && fix.exitCode !== 1) || !isFindingList(findings))
  throw new Error(`stanza --fix failed with ${ending(fix)}, its output is in ${findingsFile}`);

console.log(`stanza --fix on the export took ${(performance.now() - start).toFixed(0)} ms`);

writeFileSync(diffFile, run(["diff", "-ru", wanted, tool], [0, 1]));

const files = run(["diff", "-rq", wanted, tool], [0, 1])
  .toString()
  .split("\n")
  .filter((line) => line.length > 0).length;

console.log(`files differing from the target commit: ${files}`);
console.log(`target export: ${wanted}`);
console.log(`tool export: ${tool}`);
console.log(`diff written to ${diffFile}, remaining findings in ${findingsFile}`);
