import { mkdirSync, rmSync, writeFileSync } from "node:fs";
import { join } from "node:path";

const root = join(import.meta.dir, "..");
const stanza = join(root, "bin", "stanza");

const repo = process.env.STANZA_REPO;
const base = process.env.STANZA_BASE;
const target = process.env.STANZA_TARGET;
if (!repo || !base || !target)
  throw new Error(
    "STANZA_REPO, STANZA_BASE (commit) and STANZA_TARGET (commit with the style applied by hand) are required",
  );

const work = process.env.STANZA_WORK ?? "/tmp/stanza-check";
const tool = join(work, "tool");
const wanted = join(work, "target");

rmSync(work, { recursive: true, force: true });
mkdirSync(tool, { recursive: true });
mkdirSync(wanted, { recursive: true });

await Bun.$`git -C ${repo} archive ${base} | tar -x -C ${tool}`;
await Bun.$`git -C ${repo} archive ${target} | tar -x -C ${wanted}`;

const start = performance.now();

const fix = Bun.spawnSync([stanza, "--fix", "."], { cwd: tool });
writeFileSync(join(work, "tool-findings.txt"), fix.stdout);
console.log(`stanza --fix on the export took ${(performance.now() - start).toFixed(0)} ms`);

const diff = Bun.spawnSync(["diff", "-ru", wanted, tool]);
const text = new TextDecoder().decode(diff.stdout);
writeFileSync(join(work, "tool-vs-target.diff"), text);

const files = text.split("\n").filter((line) => line.startsWith("diff -ru ")).length;
console.log(`files differing from the target commit: ${files}`);
console.log(
  `diff written to ${join(work, "tool-vs-target.diff")}, remaining findings in ${join(work, "tool-findings.txt")}`,
);
