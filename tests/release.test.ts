import { expect, test } from "bun:test";
import { chmodSync, existsSync, mkdirSync, readFileSync, writeFileSync } from "node:fs";
import { join } from "node:path";
import { scratch } from "./support.ts";

const script = join(import.meta.dir, "..", "scripts", "release.sh");
const artifacts = { "stanza-a": "a binary\n", "stanza-b": "b binary\n" };

const gh = `#!/bin/sh
IFS='\t'
printf '%s\\n' "$*" >>"$STUB/log"

case "$1 $2" in
  "release view")
    if [ -f "$STUB/assets" ]; then
      cat "$STUB/draft" "$STUB/assets"
      exit 0
    fi

    echo "release not found" >&2
    exit 1
    ;;
  "release download")
    cat "$STUB/manifest"
    ;;
esac
`;

interface ExistingRelease {
  assets: string[];
  draft?: boolean;
  manifest: string;
}

interface ReleaseRun {
  calls: string[][];
  code: number;
  dist: string;
  stderr: string;
}

function manifest(files: Record<string, string>): string {
  return Object.entries(files)
    .map(
      ([name, text]) => `${new Bun.CryptoHasher("sha256").update(text).digest("hex")}  ${name}\n`,
    )
    .join("");
}

function git(cwd: string, ...args: string[]): void {
  const config = ["-c", "user.name=t", "-c", "user.email=t@t", "-c", "commit.gpgsign=false"];
  const result = Bun.spawnSync(["git", ...config, "-c", "tag.gpgsign=false", ...args], { cwd });
  if (result.exitCode !== 0) throw new Error(new TextDecoder().decode(result.stderr));
}

function release(tag: string, tags: string[], existing?: ExistingRelease): ReleaseRun {
  const cwd = scratch("release-repo");
  git(cwd, "init", "-q");
  git(cwd, "commit", "-q", "--allow-empty", "-m", "release");
  for (const name of tags) git(cwd, "tag", name);

  const dist = scratch("release-dist");
  for (const [name, text] of Object.entries(artifacts)) writeFileSync(join(dist, name), text);
  writeFileSync(join(dist, "SHA256SUMS"), manifest(artifacts));

  const stub = scratch("release-gh");
  mkdirSync(join(stub, "bin"));
  writeFileSync(join(stub, "bin", "gh"), gh);
  chmodSync(join(stub, "bin", "gh"), 0o755);

  if (existing) {
    writeFileSync(join(stub, "draft"), `${existing.draft ?? false}\n`);
    writeFileSync(join(stub, "assets"), existing.assets.map((name) => `${name}\n`).join(""));
    writeFileSync(join(stub, "manifest"), existing.manifest);
  }

  const result = Bun.spawnSync([script, dist], {
    cwd,
    env: {
      ...process.env,
      GITHUB_REF_NAME: tag,
      PATH: `${join(stub, "bin")}:${process.env.PATH}`,
      STUB: stub,
    },
  });

  const log = join(stub, "log");
  const calls = existsSync(log)
    ? readFileSync(log, "utf8")
        .trimEnd()
        .split("\n")
        .map((line) => line.split("\t"))
    : [];

  return { calls, code: result.exitCode, dist, stderr: new TextDecoder().decode(result.stderr) };
}

function writes(calls: string[][]): string[][] {
  return calls.filter(
    ([group, command]) => group === "release" && (command === "create" || command === "upload"),
  );
}

function create(calls: string[][]): string[] | undefined {
  return calls.find(([group, command]) => group === "release" && command === "create");
}

const assets = ["SHA256SUMS", ...Object.keys(artifacts)];

test("an existing release with matching checksums uploads nothing", () => {
  const result = release("v0.1.0", ["v0.1.0"], { assets, manifest: manifest(artifacts) });
  expect(result.code, result.stderr).toBe(0);
  expect(writes(result.calls)).toEqual([]);
});

test("a draft left by an interrupted run fails instead of passing as released", () => {
  const existing = { assets, draft: true, manifest: manifest(artifacts) };
  const result = release("v0.1.0", ["v0.1.0"], existing);

  expect(result.code).not.toBe(0);
  expect(result.stderr).toContain("draft");
  expect(writes(result.calls)).toEqual([]);
});

test("an existing release with a different checksum names the asset", () => {
  const rebuilt = manifest({ ...artifacts, "stanza-a": "rebuilt\n" });
  const result = release("v0.1.0", ["v0.1.0"], { assets, manifest: rebuilt });

  expect(result.code).not.toBe(0);
  expect(result.stderr).toContain("stanza-a");
  expect(result.stderr).not.toContain("stanza-b");
  expect(writes(result.calls)).toEqual([]);
});

test("the first release has no notes start tag", () => {
  const result = release("v0.1.0", ["v0.1.0"]);

  expect(result.code, result.stderr).toBe(0);
  expect(create(result.calls)).toEqual([
    "release",
    "create",
    "v0.1.0",
    `${result.dist}/SHA256SUMS`,
    `${result.dist}/stanza-a`,
    `${result.dist}/stanza-b`,
    "--verify-tag",
    "--generate-notes",
  ]);
});

test("a later release starts its notes at the previous stable tag", () => {
  const result = release("v0.2.0", ["v0.1.0", "v0.2.0-rc.1", "v0.2.0"]);

  expect(result.code, result.stderr).toBe(0);
  expect(create(result.calls)).toEqual([
    "release",
    "create",
    "v0.2.0",
    `${result.dist}/SHA256SUMS`,
    `${result.dist}/stanza-a`,
    `${result.dist}/stanza-b`,
    "--verify-tag",
    "--generate-notes",
    "--notes-start-tag",
    "v0.1.0",
  ]);
});

test("a prerelease is marked as one without a notes start tag", () => {
  const result = release("v0.2.0-rc.1", ["v0.1.0", "v0.2.0-rc.1"]);
  const args = create(result.calls);

  expect(result.code, result.stderr).toBe(0);
  expect(args).toContain("--prerelease");
  expect(args).not.toContain("--notes-start-tag");
});
