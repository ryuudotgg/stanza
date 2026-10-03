import { expect, test } from "bun:test";
import { createHash } from "node:crypto";
import {
  appendFileSync,
  chmodSync,
  existsSync,
  mkdirSync,
  readdirSync,
  readFileSync,
  rmSync,
  statSync,
  writeFileSync,
} from "node:fs";
import { join } from "node:path";
import { platforms } from "../scripts/platform.ts";
import {
  archiveName,
  changelogSection,
  distAssets,
  expectedAssetNames,
  judge,
} from "../scripts/release.ts";
import type { Existing } from "../scripts/release.ts";
import { scratch } from "./support.ts";

const script = join(import.meta.dirname, "..", "scripts", "release.ts");
const notes = "### Generated variants\n\nSkip generated files (#106).";
const artifacts = Object.fromEntries(
  expectedAssetNames().map((name) => [name, `${name} contents\n`]),
);

const gh = `#!/usr/bin/env bun
const { appendFileSync, existsSync, readFileSync } = require("node:fs");
const args = process.argv.slice(2);
const stdin = args[0] === "release" && args[1] === "create" ? readFileSync(0, "utf8") : "";
appendFileSync(process.env.STUB + "/log", JSON.stringify({ args, stdin }) + "\\n");
if (args[0] === "release" && args[1] === "view") {
  if (existsSync(process.env.STUB + "/view.json")) {
    console.log(readFileSync(process.env.STUB + "/view.json", "utf8"));
  } else {
    console.error("release not found");
    process.exit(1);
  }
}
`;

interface Call {
  args: string[];
  stdin: string;
}

interface ReleaseRun {
  calls: Call[];
  code: number;
  dist: string;
  stderr: string;
}

interface ReleaseOptions {
  version?: string;
  files?: Record<string, string>;
  existing?: Existing;
  changelog?: string;
  viewError?: string;
  incompleteAsset?: { name: string; digest: string | null; state: string };
}

function published(files = artifacts, draft = false): Existing {
  return {
    draft,
    assets: Object.entries(files).map(([name, text]) => ({
      name,
      digest: createHash("sha256").update(text).digest("hex"),
    })),
  };
}

function release(options: ReleaseOptions = {}): ReleaseRun {
  const cwd = scratch("release");
  const dist = join(cwd, "dist");
  const stub = join(cwd, "stub");
  const version = options.version ?? "0.1.1";

  mkdirSync(dist);
  mkdirSync(stub);

  writeFileSync(
    join(cwd, "package.json"),
    JSON.stringify({
      name: "@ryuugg/stanza",
      version,
      repository: { url: "git+https://github.com/ryuudotgg/stanza.git" },
    }),
  );

  writeFileSync(
    join(cwd, "CHANGELOG.md"),
    options.changelog ?? `## ${version}\n\n${notes}\n\n## 0.1.0\n\nOld notes.\n`,
  );

  for (const [name, contents] of Object.entries(options.files ?? artifacts))
    writeFileSync(join(dist, name), contents);

  writeFileSync(
    join(stub, "gh"),
    options.viewError
      ? `#!/usr/bin/env bun\nconsole.error(${JSON.stringify(options.viewError)}); process.exit(1);\n`
      : gh,
  );

  chmodSync(join(stub, "gh"), 0o755);

  if (options.existing)
    writeFileSync(
      join(stub, "view.json"),
      JSON.stringify({
        isDraft: options.existing.draft,
        assets: options.existing.assets.map((asset) => ({
          ...(options.incompleteAsset?.name === asset.name
            ? options.incompleteAsset
            : { name: asset.name, digest: `sha256:${asset.digest}`, state: "uploaded" }),
        })),
      }),
    );

  const env: NodeJS.ProcessEnv = {
    ...process.env,
    PATH: `${stub}:${process.env.PATH}`,
    STUB: stub,
  };

  delete env.GITHUB_REPOSITORY;

  const result = Bun.spawnSync([process.execPath, script, dist], { cwd, env });
  const log = join(stub, "log");
  const calls: Call[] = existsSync(log)
    ? readFileSync(log, "utf8")
        .trim()
        .split("\n")
        .map((line) => JSON.parse(line))
    : [];

  return { calls, code: result.exitCode, dist, stderr: new TextDecoder().decode(result.stderr) };
}

function writes(calls: Call[]): Call[] {
  return calls.filter(
    ({ args }) => args[0] === "release" && (args[1] === "create" || args[1] === "upload"),
  );
}

test("archives are reproducible and contain only an executable stanza matching the raw binary", () => {
  const dist = scratch("archives");
  for (const platform of platforms) {
    const file = join(dist, `stanza-${platform}`);
    writeFileSync(file, `#!/bin/sh\nprintf '%s\\n' '${platform}'\n`);
    chmodSync(file, 0o755);
  }

  const archived = Bun.spawnSync([join(import.meta.dirname, "..", "scripts", "archive.sh"), dist]);
  expect(archived.exitCode, archived.stderr.toString()).toBe(0);

  const assets = distAssets(dist);
  expect(assets.map(({ name }) => name)).toEqual(expectedAssetNames());

  const rerun = Bun.spawnSync([join(import.meta.dirname, "..", "scripts", "archive.sh"), dist]);
  expect(rerun.exitCode, rerun.stderr.toString()).toBe(0);
  expect(distAssets(dist)).toEqual(assets);

  const checksums = readFileSync(join(dist, "SHA256SUMS"), "utf8").trim().split("\n");
  const checksummed = assets.filter(({ name }) => name !== "SHA256SUMS");
  expect(checksums).toEqual(checksummed.map(({ name, digest }) => `${digest}  ${name}`));

  const platform = platforms[0]!;
  const extracted = scratch("extracted");
  const unpacked = Bun.spawnSync([
    "tar",
    "-xJf",
    join(dist, archiveName(platform)),
    "-C",
    extracted,
  ]);

  expect(unpacked.exitCode, unpacked.stderr.toString()).toBe(0);
  expect(readdirSync(extracted)).toEqual(["stanza"]);

  const binary = join(extracted, "stanza");
  expect(statSync(binary).isFile()).toBe(true);
  expect(statSync(binary).mode & 0o777).toBe(0o755);
  expect(readFileSync(binary)).toEqual(readFileSync(join(dist, `stanza-${platform}`)));
});

const root = join(import.meta.dirname, "..");
function archivedShims(): string {
  const dist = scratch("smoke-archives");
  for (const platform of platforms) {
    const file = join(dist, `stanza-${platform}`);
    writeFileSync(
      file,
      `#!/bin/sh\nexec "${process.execPath}" "${join(root, "src", "cli.ts")}" "$@"\n`,
    );

    chmodSync(file, 0o755);
  }

  const archived = Bun.spawnSync([join(root, "scripts", "archive.sh"), dist]);
  expect(archived.exitCode, archived.stderr.toString()).toBe(0);
  return dist;
}

function repack(dist: string, platform: string, entries: Record<string, number>): void {
  const stage = scratch("repack");
  for (const [name, mode] of Object.entries(entries)) {
    writeFileSync(join(stage, name), `${name} ${mode}\n`);
    chmodSync(join(stage, name), mode);
  }

  const packed = Bun.spawnSync(
    [
      "tar",
      "--format",
      "ustar",
      "-cJf",
      join(dist, archiveName(platform)),
      "-C",
      stage,
      ...Object.keys(entries),
    ],
    { env: { ...process.env, COPYFILE_DISABLE: "1" } },
  );

  expect(packed.exitCode, packed.stderr.toString()).toBe(0);

  const sums = distAssets(dist)
    .filter(({ name }) => name !== "SHA256SUMS")
    .map(({ name, digest }) => `${digest}  ${name}\n`);

  writeFileSync(join(dist, "SHA256SUMS"), sums.join(""));
}

function smokeArchives(
  dist: string,
  platform: string,
): { code: number; stdout: string; stderr: string } {
  const env: NodeJS.ProcessEnv = { ...process.env };
  delete env.VERSION;
  delete env.COMMIT;

  const result = Bun.spawnSync([join(root, "scripts", "smoke-archives.sh"), dist, platform], {
    env,
  });

  return {
    code: result.exitCode,
    stdout: result.stdout.toString(),
    stderr: result.stderr.toString(),
  };
}

test("smoke-archives runs the extracted binary from intact archives", () => {
  const result = smokeArchives(archivedShims(), "linux-x64");
  expect(result.code, result.stderr).toBe(0);
  expect(result.stdout.trimEnd().split("\n").at(-1)).toBe("smoke-archives: ok linux-x64");
}, 30_000);

test("smoke-archives rejects damaged, malformed, mismatched and missing archives", () => {
  const cases: { damage: (dist: string) => void; platform?: string; error: string }[] = [
    {
      damage: (dist) => appendFileSync(join(dist, archiveName("linux-x64")), "x"),
      error: "checksum verification failed",
    },
    {
      damage: (dist) => repack(dist, "linux-x64", { stanza: 0o755, extra: 0o644 }),
      error: `${archiveName("linux-x64")} must contain only an executable stanza file`,
    },
    {
      damage: (dist) => repack(dist, "linux-x64", { stanza: 0o644 }),
      error: `${archiveName("linux-x64")} must contain only an executable stanza file`,
    },
    {
      damage: (dist) => repack(dist, "linux-x64", { stanza: 0o755 }),
      error: `${archiveName("linux-x64")} differs from stanza-linux-x64`,
    },
    { damage: () => {}, platform: "freebsd-x64", error: "missing archive for freebsd-x64" },
  ];

  for (const { damage, platform, error } of cases) {
    const dist = archivedShims();
    damage(dist);

    const result = smokeArchives(dist, platform ?? "linux-x64");
    expect(result.code).not.toBe(0);
    expect(result.stderr).toContain(`smoke-archives: ${error}`);
  }
}, 30_000);

test("a missing release is created once with all assets and the version's notes", () => {
  const result = release();

  expect(result.code, result.stderr).toBe(0);
  expect(writes(result.calls)).toEqual([
    {
      args: [
        "release",
        "create",
        "v0.1.1",
        ...expectedAssetNames().map((name) => join(result.dist, name)),
        "-R",
        "ryuudotgg/stanza",
        "--verify-tag",
        "--notes-file",
        "-",
      ],
      stdin: notes,
    },
  ]);

  expect(result.calls[0]?.args).toEqual([
    "release",
    "view",
    "v0.1.1",
    "-R",
    "ryuudotgg/stanza",
    "--json",
    "isDraft,assets",
  ]);
});

test("a prerelease is marked as one", () => {
  const result = release({ version: "0.2.0-rc.1" });
  expect(result.code, result.stderr).toBe(0);
  expect(writes(result.calls)[0]?.args).toContain("--prerelease");
});

test("an existing published release with matching digests uploads nothing", () => {
  const result = release({ existing: published() });
  expect(result.code, result.stderr).toBe(0);
  expect(writes(result.calls)).toEqual([]);
});

test("a different binary digest names only that binary", () => {
  const name = expectedAssetNames().find((asset) => asset !== "SHA256SUMS")!;
  const result = release({ existing: published({ ...artifacts, [name]: "swapped\n" }) });

  expect(result.code).not.toBe(0);
  expect(result.stderr.trim()).toBe(`release: ${name} does not match the existing v0.1.1 release`);
  expect(writes(result.calls)).toEqual([]);
});

test("an extra release asset is named", () => {
  const result = release({ existing: published({ ...artifacts, "unexpected.txt": "extra\n" }) });

  expect(result.code).not.toBe(0);
  expect(result.stderr.trim()).toBe(
    "release: unexpected.txt does not match the existing v0.1.1 release",
  );

  expect(writes(result.calls)).toEqual([]);
});

test("an asset without a digest or still uploading is named", () => {
  const name = "stanza-linux-x64";
  for (const asset of [
    { digest: null, state: "uploaded" },
    {
      digest: `sha256:${published().assets.find((asset) => asset.name === name)!.digest}`,
      state: "starter",
    },
  ]) {
    const result = release({ existing: published(), incompleteAsset: { name, ...asset } });

    expect(result.code).not.toBe(0);
    expect(result.stderr.trim()).toBe(
      `release: ${name} does not match the existing v0.1.1 release`,
    );

    expect(writes(result.calls)).toEqual([]);
  }
});

test("the CLI prefixes every error line for two differing binaries", () => {
  const names = ["stanza-linux-x64", "stanza-linux-x64-musl"];
  const files = { ...artifacts, ...Object.fromEntries(names.map((name) => [name, "swapped\n"])) };
  const result = release({ existing: published(files) });

  expect(result.code).not.toBe(0);
  expect(result.stderr.trim().split("\n")).toEqual(
    names.map((name) => `release: ${name} does not match the existing v0.1.1 release`),
  );

  expect(writes(result.calls)).toEqual([]);
});

test("a draft fails without creating a release", () => {
  const result = release({ existing: published(artifacts, true) });

  expect(result.code).not.toBe(0);
  expect(result.stderr).toContain("draft");
  expect(writes(result.calls)).toEqual([]);
});

test("a missing binary fails before any gh call", () => {
  const name = expectedAssetNames().find((asset) => asset !== "SHA256SUMS")!;
  const files = { ...artifacts };
  delete files[name];

  const result = release({ files });

  expect(result.code).not.toBe(0);
  expect(result.stderr).toContain(name);
  expect(result.calls).toEqual([]);
});

test("an extra dist file fails before any gh call", () => {
  const result = release({ files: { ...artifacts, "unexpected.txt": "extra" } });

  expect(result.code).not.toBe(0);
  expect(result.stderr).toContain("unexpected.txt");
  expect(result.calls).toEqual([]);
});

test("a missing changelog section fails before any gh call", () => {
  const result = release({ changelog: "## 0.1.0\n\nOld notes.\n" });

  expect(result.code).not.toBe(0);
  expect(result.stderr).toContain("0.1.1");
  expect(result.calls).toEqual([]);
});

test("a gh failure other than release not found is reported", () => {
  const result = release({ viewError: "authentication failed" });

  expect(result.code).not.toBe(0);
  expect(result.stderr).toContain("authentication failed");
  expect(writes(result.calls)).toEqual([]);
});

test("judge distinguishes creation, drafts, matching assets and all differences", () => {
  const built = [
    { name: "same", digest: "a" },
    { name: "changed", digest: "b" },
    { name: "missing", digest: "c" },
  ];

  expect(judge(built, undefined)).toEqual({ kind: "create" });
  expect(judge(built, { draft: true, assets: [] })).toEqual({ kind: "draft" });
  expect(judge(built, { draft: false, assets: [...built].reverse() })).toEqual({ kind: "matches" });
  expect(
    judge(built, {
      draft: false,
      assets: [
        { name: "same", digest: "a" },
        { name: "changed", digest: "d" },
        { name: "extra", digest: "e" },
      ],
    }),
  ).toEqual({ kind: "differs", names: ["changed", "extra", "missing"] });
});

test("changelog sections preserve headings inside backtick and tilde fences", () => {
  for (const fence of ["```", "~~~"]) {
    const section = `Notes.\n\n${fence}md\n## 0.1.0\n${fence}\n\nMore notes.`;

    expect(changelogSection(`## 0.1.1\n\n${section}\n\n## 0.1.0\n\nOld notes.`, "0.1.1")).toBe(
      section,
    );

    expect(changelogSection(`${fence}\n## 0.1.1\n${fence}\n## 0.1.1\n\n${notes}`, "0.1.1")).toBe(
      notes,
    );
  }
});

test("missing and blank changelog sections are rejected", () => {
  expect(() => changelogSection("## 0.1.0\n\nOld notes.", "0.1.1")).toThrow("0.1.1");
  expect(() => changelogSection("## 0.1.1\n\n## 0.1.0\nOld notes.", "0.1.1")).toThrow("0.1.1");
});

test("changelog fences close with trailing text and indented headings end sections", () => {
  const section = "Notes.\n\n```sh\n## 0.0.0\n```sh\n\nMore notes.";

  expect(changelogSection(`   ## 0.1.1 \t\n\n${section}\n\n ## 0.1.0\n\nOld notes.`, "0.1.1")).toBe(
    section,
  );

  expect(changelogSection(`## 0.1.1\n\n${notes}\n\n   # Older releases`, "0.1.1")).toBe(notes);
});

test("dist validation names every missing and extra file", () => {
  const dist = scratch("assets");
  writeFileSync(join(dist, "extra"), "extra");

  let message = "";
  try {
    distAssets(dist);
  } catch (error) {
    if (!(error instanceof Error)) throw error;
    message = error.message;
  }

  expect(message.split("\n")).toEqual([
    ...expectedAssetNames().map((name) => `missing ${name}`),
    "extra extra",
  ]);

  rmSync(join(dist, "extra"));

  for (const [name, contents] of Object.entries(artifacts))
    writeFileSync(join(dist, name), contents);

  expect(distAssets(dist)).toEqual(published().assets);
});
