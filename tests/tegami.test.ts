import { expect, test } from "bun:test";
import { existsSync, mkdirSync, readFileSync, writeFileSync } from "node:fs";
import { join } from "node:path";
import { PackagePublishTask } from "tegami";
import type { PackagePublishTaskResult } from "tegami";
import { GitCreateTagsTask, git } from "tegami/plugins/git";
import { ReleaseTask, stanza } from "../scripts/tegami.ts";
import { cpuOf } from "./budget.ts";
import { gitBinary, scratch } from "./support.ts";

const script = join(import.meta.dirname, "..", "scripts", "tegami.ts");
const env = { ...process.env };
for (const key of ["CI", "GITHUB_TOKEN", "GH_TOKEN", "GITHUB_REPOSITORY"]) delete env[key];

const decoder = new TextDecoder();

interface Note {
  name: string;
  content: string;
  commit?: string;
}

function note(heading: string, body: string): string {
  return `---\npackages:\n  "@ryuugg/stanza": patch\n---\n\n${heading}\n\n${body}\n`;
}

function inRepo(cwd: string, ...args: string[]): string {
  const result = Bun.spawnSync([gitBinary, ...args], { cwd, env });
  expect(result.exitCode, decoder.decode(result.stderr)).toBe(0);
  return decoder.decode(result.stdout);
}

function commit(cwd: string, title: string): void {
  inRepo(cwd, "add", ".");
  inRepo(
    cwd,
    "-c",
    "user.name=t",
    "-c",
    "user.email=t@t",
    "-c",
    "commit.gpgsign=false",
    "commit",
    "-q",
    "-m",
    title,
  );
}

function fixture(notes: Note[], changelog?: string): string {
  const cwd = scratch("tegami");
  writeFileSync(
    join(cwd, "package.json"),
    JSON.stringify({
      name: "@ryuugg/stanza",
      version: "0.1.0",
      repository: { url: "git+https://github.com/ryuudotgg/stanza.git" },
    }),
  );

  mkdirSync(join(cwd, ".tegami"));
  if (changelog !== undefined) writeFileSync(join(cwd, "CHANGELOG.md"), changelog);

  inRepo(cwd, "init", "-q");
  commit(cwd, "test: seed scratch repo");

  for (const { name, content, commit: title = "docs: add a note" } of notes) {
    writeFileSync(join(cwd, ".tegami", name), content);
    commit(cwd, title);
  }

  return cwd;
}

function tegamiRun(cwd: string, ...args: string[]): { code: number; cpu: number; output: string } {
  const result = Bun.spawnSync([process.execPath, script, ...args], { cwd, env });
  return {
    code: result.exitCode,
    cpu: cpuOf(result),
    output: decoder.decode(result.stdout) + decoder.decode(result.stderr),
  };
}

function utcDate(): string {
  return new Date().toLocaleDateString("en-US", { dateStyle: "long", timeZone: "UTC" });
}

const older = "## 0.1.0\n\n### Older\n\nOlder notes, as Tegami wrote them.\n";
const fixed = {
  name: "2026-10-01-explain-lines.md",
  content: note(
    "### 🐛 Explain counts lines like an editor",
    "`stanza explain` treats a final newline as the end of the last line.",
  ),
  commit: "fix: count lines like an editor (#57)",
};

const improved = {
  name: "2026-10-02-faster-start.md",
  content: note(
    "### 🎨 Release binaries start faster",
    "Each run skips parsing Stanza's own code\nand starts a few milliseconds sooner.",
  ),
  commit: "perf: ship precompiled bytecode",
};

function expectedSection(cwd: string, date: string): string {
  const [full, short] = inRepo(
    cwd,
    "log",
    "-n",
    "1",
    "--format=%H %h",
    "--",
    `.tegami/${improved.name}`,
  )
    .trim()
    .split(" ");

  return [
    "## 0.1.1",
    "",
    `_${date}_`,
    "",
    "### 🎨 Improved",
    "",
    `- **Release binaries start faster** ([${short}](https://github.com/ryuudotgg/stanza/commit/${full}))\\`,
    "  Each run skips parsing Stanza's own code and starts a few milliseconds sooner.",
    "",
    "### 🐛 Fixed",
    "",
    "- **Explain counts lines like an editor** ([#57](https://github.com/ryuudotgg/stanza/pull/57))\\",
    "  `stanza explain` treats a final newline as the end of the last line.",
  ].join("\n");
}

function versioned(): { cwd: string; sections: string[] } {
  const cwd = fixture([fixed, improved], older);
  const before = utcDate();
  const result = tegamiRun(cwd, "version");
  expect(result.code, result.output).toBe(0);
  expect(result.cpu).toBeLessThan(5_000);

  return { cwd, sections: [before, utcDate()].map((date) => expectedSection(cwd, date)) };
}

test("version consumes a patch note and writes the changelog and publish lock", () => {
  const cwd = fixture([fixed]);
  const result = tegamiRun(cwd, "version");

  expect(result.code, result.output).toBe(0);
  expect(result.cpu).toBeLessThan(5_000);

  expect(JSON.parse(readFileSync(join(cwd, "package.json"), "utf8")).version).toBe("0.1.1");
  expect(readFileSync(join(cwd, "CHANGELOG.md"), "utf8").startsWith("## 0.1.1\n")).toBe(true);
  expect(existsSync(join(cwd, ".tegami", "publish-lock.yaml"))).toBe(true);
  expect(existsSync(join(cwd, ".tegami", fixed.name))).toBe(false);
}, 60_000);

test("version groups notes by category with a date line and a reference on each entry", () => {
  const { cwd, sections } = versioned();
  const changelog = readFileSync(join(cwd, "CHANGELOG.md"), "utf8");
  expect(sections.map((section) => `${section}\n\n${older}`)).toContain(changelog);
}, 60_000);

test("version --dry-run prints the section version would write and writes nothing", () => {
  const cwd = fixture([fixed, improved], older);
  const before = utcDate();
  const result = tegamiRun(cwd, "version", "--dry-run");

  expect(result.code, result.output).toBe(0);
  expect([before, utcDate()].map((date) => `${expectedSection(cwd, date)}\n`)).toContain(
    result.output,
  );

  expect(readFileSync(join(cwd, "CHANGELOG.md"), "utf8")).toBe(older);
  expect(existsSync(join(cwd, ".tegami", fixed.name))).toBe(true);
  expect(JSON.parse(readFileSync(join(cwd, "package.json"), "utf8")).version).toBe("0.1.0");
}, 60_000);

const body = "Text.";
const heading = "### 🎨 Starts faster";
const refusals: (Note & { problem: string; says: string })[] = [
  {
    problem: "no date in its name",
    name: "faster-start.md",
    content: improved.content,
    says: "name the note",
  },
  {
    problem: "uppercase in its name",
    name: "2026-10-02-Faster-Start.md",
    content: improved.content,
    says: "name the note",
  },
  {
    problem: "a date that does not exist in its name",
    name: "2026-13-45-faster-start.md",
    content: improved.content,
    says: "name the note",
  },
  {
    problem: "no frontmatter",
    name: "2026-10-02-bare.md",
    content: `${heading}\n\n${body}\n`,
    says: "start the note with frontmatter",
  },
  {
    problem: "no heading",
    name: "2026-10-02-no-heading.md",
    content: '---\npackages:\n  "@ryuugg/stanza": patch\n---\n\nJust text.\n',
    says: "add a ### heading",
  },
  {
    problem: "text above its heading",
    name: "2026-10-02-preface.md",
    content: note("Intro.", `${heading}\n\n${body}`),
    says: "move the text above the first heading",
  },
  {
    problem: "no category",
    name: "2026-10-02-plain.md",
    content: note("### Starts faster", body),
    says: "with one of 🚨 ✨ 🎨 🐛",
  },
  {
    problem: "no title after its emoji",
    name: "2026-10-02-untitled.md",
    content: note("### 🎨", body),
    says: "a title after its emoji",
  },
  {
    problem: "a ## heading",
    name: "2026-10-02-level.md",
    content: note("## 🎨 Starts faster", body),
    says: "is a ## heading",
  },
  {
    problem: "a #### heading",
    name: "2026-10-02-deep.md",
    content: note("#### 🎨 Starts faster", body),
    says: "is a #### heading",
  },
  {
    problem: "a list",
    name: "2026-10-02-list.md",
    content: note(heading, "- one\n- two"),
    says: "one plain paragraph",
  },
  {
    problem: "a table",
    name: "2026-10-02-table.md",
    content: note(heading, "| a | b |\n| - | - |"),
    says: "one plain paragraph",
  },
  {
    problem: "a table with no outer pipes",
    name: "2026-10-02-bare-table.md",
    content: note(heading, "Name | Value\n--- | ---\nbefore | after"),
    says: "one plain paragraph",
  },
  {
    problem: "a tab after a list marker",
    name: "2026-10-02-tab-list.md",
    content: note(heading, "-\tone\n-\ttwo"),
    says: "one plain paragraph",
  },
  {
    problem: "a link reference definition",
    name: "2026-10-02-reference.md",
    content: note(heading, "[docs]: https://stanza.ryuu.gg\n[Read more][docs] about it."),
    says: "one plain paragraph",
  },
  {
    problem: "a code block",
    name: "2026-10-02-code.md",
    content: note(heading, "Text.\n```sh\n# run it\nstanza\n```"),
    says: "one plain paragraph",
  },
  {
    problem: "a second paragraph",
    name: "2026-10-02-paragraphs.md",
    content: note(heading, "One.\n\nTwo."),
    says: "one plain paragraph",
  },
];

for (const { problem, says, ...refused } of refusals)
  for (const args of [["version"], ["version", "--dry-run"]])
    test(`${args.join(" ")} refuses a note with ${problem}, naming its file`, () => {
      const cwd = fixture([fixed, refused]);
      const result = tegamiRun(cwd, ...args);

      expect(result.code).not.toBe(0);
      expect(result.output).toContain(`.tegami/${refused.name}: `);
      expect(result.output).toContain(says);
      expect(existsSync(join(cwd, "CHANGELOG.md"))).toBe(false);
      expect(existsSync(join(cwd, ".tegami", refused.name))).toBe(true);
    }, 60_000);

test("ci refuses a bad note before it versions anything", () => {
  const cwd = fixture([fixed, { name: "bad.md", content: fixed.content }]);
  const result = tegamiRun(cwd, "ci");

  expect(result.code).not.toBe(0);
  expect(result.output).toContain(".tegami/bad.md: name the note");
  expect(JSON.parse(readFileSync(join(cwd, "package.json"), "utf8")).version).toBe("0.1.0");
  expect(existsSync(join(cwd, ".tegami", fixed.name))).toBe(true);
}, 60_000);

test("a body that opens with an autolink is one plain paragraph", () => {
  const linked = {
    name: "2026-10-02-docs-link.md",
    content: note(
      "### ✨ Docs cover the hook",
      "<https://stanza.ryuu.gg> documents the hook,\n<kbd>Ctrl</kbd> included.",
    ),
  };

  const result = tegamiRun(fixture([linked]), "version", "--dry-run");
  expect(result.code, result.output).toBe(0);
  expect(result.output).toContain(
    "  <https://stanza.ryuu.gg> documents the hook, <kbd>Ctrl</kbd> included.",
  );
}, 60_000);

test("version rejects a repo with no pending notes", () => {
  const result = tegamiRun(fixture([]), "version");
  expect(result.code).not.toBe(0);
  expect(result.output).toContain("no pending notes");
  expect(result.cpu).toBeLessThan(5_000);
}, 60_000);

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

test("the publish dry run prints the new section as the release notes", () => {
  const { cwd, sections } = versioned();
  const logged: string[] = [];
  const log = console.log;
  console.log = (line: string) => logged.push(line);

  try {
    const plan = { options: { dryRun: true }, getPackagesToPublish: () => [{ version: "0.1.1" }] };
    stanza().beforePublishAll?.call({ cwd } as never, { plan } as never);
  } finally {
    console.log = log;
  }

  expect(logged[0]).toBe("dry run: would tag v0.1.1 and release it with these notes:");
  expect(sections.map((section) => section.replace("## 0.1.1\n\n", ""))).toContain(logged[1] ?? "");
}, 60_000);

test("a dry run refuses a CHANGELOG without the version's section", () => {
  const cwd = scratch("tegami-changelog");
  writeFileSync(join(cwd, "CHANGELOG.md"), "## 0.1.0\n\nOld notes.\n");

  const plan = { options: { dryRun: true }, getPackagesToPublish: () => [{ version: "0.1.1" }] };
  expect(() => stanza().beforePublishAll?.call({ cwd } as never, { plan } as never)).toThrow(
    "0.1.1",
  );
});
