import { expect, test } from "bun:test";
import { existsSync, mkdirSync, readFileSync, writeFileSync } from "node:fs";
import { join } from "node:path";
import { PackagePublishTask } from "tegami";
import type { PackagePublishTaskResult } from "tegami";
import { GitCreateTagsTask, git } from "tegami/plugins/git";
import { ReleaseTask, stanza } from "../scripts/tegami.ts";
import { scratch } from "./support.ts";

const script = join(import.meta.dirname, "..", "scripts", "tegami.ts");

function version(withNote: boolean): { cwd: string; code: number; output: string } {
  const cwd = scratch("tegami");
  const env = { ...process.env };
  for (const key of ["CI", "GITHUB_TOKEN", "GH_TOKEN", "GITHUB_REPOSITORY"]) delete env[key];

  writeFileSync(
    join(cwd, "package.json"),
    JSON.stringify({
      name: "@ryuugg/stanza",
      version: "0.1.0",
      repository: { url: "git+https://github.com/ryuudotgg/stanza.git" },
    }),
  );

  mkdirSync(join(cwd, ".tegami"));

  if (withNote)
    writeFileSync(
      join(cwd, ".tegami", "note.md"),
      '---\npackages:\n  "@ryuugg/stanza": patch\n---\n\n### Title\n\nOne line of text.\n',
    );

  for (const args of [
    ["git", "init", "-q"],
    ["git", "add", "."],
    [
      "git",
      "-c",
      "user.name=t",
      "-c",
      "user.email=t@t",
      "-c",
      "commit.gpgsign=false",
      "commit",
      "-m",
      "test: seed scratch repo",
    ],
  ]) {
    const result = Bun.spawnSync(args, { cwd, env });
    expect(result.exitCode, new TextDecoder().decode(result.stderr)).toBe(0);
  }

  const result = Bun.spawnSync([process.execPath, script, "version"], { cwd, env });
  const decoder = new TextDecoder();
  return {
    cwd,
    code: result.exitCode,
    output: decoder.decode(result.stdout) + decoder.decode(result.stderr),
  };
}

test("version consumes a patch note and writes the changelog and publish lock", () => {
  const result = version(true);

  expect(result.code, result.output).toBe(0);
  expect(JSON.parse(readFileSync(join(result.cwd, "package.json"), "utf8")).version).toBe("0.1.1");

  const changelog = readFileSync(join(result.cwd, "CHANGELOG.md"), "utf8");
  expect(changelog.startsWith("## 0.1.1")).toBe(true);
  expect(changelog).toContain("### Title");
  expect(existsSync(join(result.cwd, ".tegami", "publish-lock.yaml"))).toBe(true);
  expect(existsSync(join(result.cwd, ".tegami", "note.md"))).toBe(false);
});

test("version rejects a repo with no pending notes", () => {
  const result = version(false);
  expect(result.code).not.toBe(0);
  expect(result.output).toContain("no pending notes");
});

class PublishFake extends PackagePublishTask {
  override publish(): PackagePublishTaskResult {
    return { type: "published" };
  }
}

function packagePlans(): Map<string, { git?: { tag?: string } }> {
  return new Map([["npm:@ryuugg/stanza", {}]]);
}

const context = { graph: { get: () => ({ name: "@ryuugg/stanza", version: "0.1.1" }) } };

test("the tag is v<version>, set before the git plugin's default", () => {
  expect(stanza().enforce).toBe("pre");

  const plan = { packages: packagePlans() };
  stanza().initPublishPlan?.call(context as never, { plan } as never);
  git().initPublishPlan?.call(context as never, { plan } as never);
  expect(plan.packages.get("npm:@ryuugg/stanza")?.git?.tag).toBe("v0.1.1");

  const defaults = { packages: packagePlans() };
  git().initPublishPlan?.call(context as never, { plan: defaults } as never);
  expect(defaults.packages.get("npm:@ryuugg/stanza")?.git?.tag).toBe("@ryuugg/stanza@0.1.1");
});

test("the release waits on the npm publish and the tag push", () => {
  const publish = new PublishFake({ id: "npm:@ryuugg/stanza" } as never);
  const tags = new GitCreateTagsTask();
  const release = new ReleaseTask();
  release.link({ plan: { tasks: [publish, tags, release] } } as never);
  expect(release.optionalWait).toEqual([publish, tags]);
});

test("a dry run reports the release pending without asking gh", () => {
  const release = new ReleaseTask();
  const status = release.status({
    context: { cwd: "/nonexistent" },
    plan: { options: { dryRun: true } },
  } as never);

  expect(status).toBe("pending");
});

test("the release is skipped when the npm publish or the tag push failed", () => {
  const publish = new PublishFake({ id: "npm:@ryuugg/stanza" } as never);
  publish.$state = { status: "failed", error: new Error("npm") };

  const release = new ReleaseTask();
  release.optionalWait.push(publish);

  const plan = { options: {}, getPackagesToPublish: () => [{ version: "0.1.1" }] };
  expect(() => release.run({ context: { cwd: "/nonexistent" }, plan } as never)).not.toThrow();
});

test("a real publish outside CI is refused before anything runs", () => {
  const ci = process.env.CI;
  delete process.env.CI;

  try {
    const plan = { options: {}, getPackagesToPublish: () => [] };
    expect(() =>
      stanza().beforePublishAll?.call({ cwd: "/nonexistent" } as never, { plan } as never),
    ).toThrow("only in CI");
  } finally {
    if (ci !== undefined) process.env.CI = ci;
  }
});

test("a dry run refuses a CHANGELOG without the version's section", () => {
  const cwd = scratch("tegami-changelog");
  writeFileSync(join(cwd, "CHANGELOG.md"), "## 0.1.0\n\nOld notes.\n");

  const plan = { options: { dryRun: true }, getPackagesToPublish: () => [{ version: "0.1.1" }] };
  expect(() => stanza().beforePublishAll?.call({ cwd } as never, { plan } as never)).toThrow(
    "0.1.1",
  );
});
