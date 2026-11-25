import { expect, test } from "bun:test";
import { existsSync, mkdirSync, readdirSync, readFileSync, writeFileSync } from "node:fs";
import { join } from "node:path";
import { PackagePublishTask } from "tegami";
import type { PackagePublishTaskResult } from "tegami";
import { GitCreateTagsTask, git } from "tegami/plugins/git";
import { categories, parseNote, ReleaseTask, stanza, versionPrBody } from "../scripts/tegami.ts";
import { cpuOf } from "./budget.ts";
import { git as scratchGit, scratch, scratchGitRepository } from "./support.ts";

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

function commit(cwd: string, title: string): void {
  scratchGit(cwd, { env }, "add", ".");
  scratchGit(cwd, { env }, "commit", "-q", "-m", title);
}

function fixture(notes: Note[], changelog?: string): string {
  const packageJson = JSON.stringify({
    name: "@ryuugg/stanza",
    version: "0.1.0",
    repository: { url: "git+https://github.com/ryuudotgg/stanza.git" },
  });

  const cwd = scratchGitRepository({
    env,
    files: {
      "package.json": packageJson,
      ...(changelog === undefined ? {} : { "CHANGELOG.md": changelog }),
    },
  });

  mkdirSync(join(cwd, ".tegami"));
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
  const [full, short] = scratchGit(
    cwd,
    { env },
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

test("version bumps every platform package with the npm package and pins them exactly", () => {
  const cwd = fixture([fixed]);
  const platforms = ["darwin-arm64", "linux-x64-musl"];
  const manifest = JSON.parse(readFileSync(join(cwd, "package.json"), "utf8"));
  manifest.workspaces = ["npm/*"];
  manifest.optionalDependencies = Object.fromEntries(
    platforms.map((platform) => [`@ryuugg/stanza-${platform}`, "0.1.0"]),
  );

  writeFileSync(join(cwd, "package.json"), JSON.stringify(manifest));

  for (const platform of platforms) {
    mkdirSync(join(cwd, "npm", platform), { recursive: true });
    writeFileSync(
      join(cwd, "npm", platform, "package.json"),
      JSON.stringify({ name: `@ryuugg/stanza-${platform}`, version: "0.1.0" }),
    );
  }

  commit(cwd, "build: add platform packages");
  const result = tegamiRun(cwd, "version");
  expect(result.code, result.output).toBe(0);

  const versioned = JSON.parse(readFileSync(join(cwd, "package.json"), "utf8"));
  expect(versioned.version).toBe("0.1.1");
  expect(versioned.optionalDependencies).toEqual({
    "@ryuugg/stanza-darwin-arm64": "0.1.1",
    "@ryuugg/stanza-linux-x64-musl": "0.1.1",
  });

  for (const platform of platforms) {
    const pkg = JSON.parse(readFileSync(join(cwd, "npm", platform, "package.json"), "utf8"));
    expect(pkg.version).toBe("0.1.1");
    expect(existsSync(join(cwd, "npm", platform, "CHANGELOG.md"))).toBe(false);
  }
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

function refusalOf(name: string, content: string): string {
  try {
    parseNote(name, content);
  } catch (error) {
    return (error as Error).message;
  }

  throw new Error(`${name} parsed without a refusal`);
}

for (const { problem, says, name, content } of refusals)
  test(`parseNote refuses a note with ${problem}, naming its file`, () => {
    const message = refusalOf(name, content);
    expect(message.startsWith(`.tegami/${name}: `), message).toBe(true);
    expect(message).toContain(says);
  });

test("parseNote joins a note's body lines into one paragraph", () => {
  expect(parseNote(improved.name, improved.content)).toEqual({
    filename: improved.name,
    entries: [
      {
        category: categories[2],
        title: "Release binaries start faster",
        body: "Each run skips parsing Stanza's own code and starts a few milliseconds sooner.",
      },
    ],
  });
});

test("parseNote reads a body that opens with an autolink as one plain paragraph", () => {
  const content = note(
    "### ✨ Docs cover the hook",
    "<https://stanza.ryuu.gg> documents the hook,\n<kbd>Ctrl</kbd> included.",
  );

  expect(parseNote("2026-10-02-docs-link.md", content).entries).toEqual([
    {
      category: categories[1],
      title: "Docs cover the hook",
      body: "<https://stanza.ryuu.gg> documents the hook, <kbd>Ctrl</kbd> included.",
    },
  ]);
});

test("a dry run prints a body that opens with an autolink as one plain paragraph", () => {
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

for (const category of categories)
  test(`parseNote files a ${category.emoji} heading under ${category.title}`, () => {
    const content = note(`### ${category.emoji} Starts faster`, body);
    expect(parseNote("2026-10-02-faster-start.md", content).entries).toEqual([
      { category, title: "Starts faster", body },
    ]);
  });

const invalid = { name: "2026-10-02-bare.md", content: `${heading}\n\n${body}\n` };

function snapshot(cwd: string): Record<string, string> {
  const notes = readdirSync(join(cwd, ".tegami")).map((name) => join(".tegami", name));
  return Object.fromEntries(
    ["package.json", "CHANGELOG.md", ...notes]
      .filter((path) => existsSync(join(cwd, path)))
      .map((path) => [path, readFileSync(join(cwd, path), "utf8")]),
  );
}

for (const command of ["version", "version --dry-run", "ci"])
  test(`${command} refuses an invalid note, naming it, and writes nothing`, () => {
    const cwd = fixture([fixed, invalid]);
    const before = snapshot(cwd);
    const result = tegamiRun(cwd, ...command.split(" "));

    expect(result.code).not.toBe(0);
    expect(result.output).toContain(`.tegami/${invalid.name}: start the note with frontmatter`);
    expect(snapshot(cwd)).toEqual(before);
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

test("only the npm package carries the tag, so a platform publish alone never pushes it", () => {
  const graph = {
    get: (id: string) => ({ name: id.slice("npm:".length), version: "0.1.1" }),
  };

  const plan = {
    packages: new Map<string, { git?: { tag?: string } }>([
      ["npm:@ryuugg/stanza", {}],
      ["npm:@ryuugg/stanza-linux-x64", {}],
    ]),
  };

  stanza().initPublishPlan?.call({ graph } as never, { plan } as never);
  git().initPublishPlan?.call({ graph } as never, { plan } as never);

  expect(plan.packages.get("npm:@ryuugg/stanza")?.git?.tag).toBe("v0.1.1");
  expect(plan.packages.get("npm:@ryuugg/stanza-linux-x64")?.git?.tag).toBe("");
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

  const plan = {
    options: {},
    getPackagesToPublish: () => [{ name: "@ryuugg/stanza", version: "0.1.1" }],
  };

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

test("a real publish is refused while a platform package has never reached npm", () => {
  const ci = process.env.CI;
  process.env.CI = "true";

  try {
    const plan = {
      options: {},
      getPackagesToPublish: () => [
        { name: "@ryuugg/stanza-darwin-arm64", version: "0.1.1" },
        { name: "@ryuugg/stanza-linux-x64", version: "0.1.1" },
      ],
    };

    const registered = (name: string) => name.endsWith("darwin-arm64");
    const plugin = stanza({ registered });
    expect(() =>
      plugin.beforePublishAll?.call({ cwd: "/nonexistent" } as never, { plan } as never),
    ).toThrow(
      new Error(
        "@ryuugg/stanza-linux-x64 is not on npm yet: publish it once by hand, then add the Publish workflow as its trusted publisher",
      ),
    );
  } finally {
    if (ci === undefined) delete process.env.CI;
    else process.env.CI = ci;
  }
});

test("the npm package publishes only once every platform package has", () => {
  const platform = (name: string, status: "success" | "failed") => {
    const task = new PublishFake({ id: `npm:${name}`, name } as never);
    task.$state =
      status === "success"
        ? { status, result: { type: "skipped" } }
        : { status, error: new Error("npm") };

    return task;
  };

  const check = (statuses: ("success" | "failed")[]) => {
    const plugin = stanza({ registered: () => true });
    const tasks = statuses.map((status, index) => platform(`@ryuugg/stanza-p${index}`, status));
    plugin.publishTasks?.call({} as never, { plan: { tasks } } as never);
    return () =>
      plugin.willPublish?.call({} as never, { pkg: { name: "@ryuugg/stanza" } } as never);
  };

  expect(check(["success", "success"])).not.toThrow();
  expect(check(["success", "failed"])).toThrow(
    "@ryuugg/stanza stays unpublished, since @ryuugg/stanza-p1 did not publish",
  );
});

test("the publish dry run prints the new section as the release notes once, for the npm package", () => {
  const { cwd, sections } = versioned();
  const logged: string[] = [];
  const log = console.log;
  console.log = (line: string) => logged.push(line);

  try {
    const plan = {
      options: { dryRun: true },
      getPackagesToPublish: () => [
        { name: "@ryuugg/stanza-linux-x64", version: "0.1.1" },
        { name: "@ryuugg/stanza", version: "0.1.1" },
      ],
    };

    stanza().beforePublishAll?.call({ cwd } as never, { plan } as never);
  } finally {
    console.log = log;
  }

  expect(logged).toHaveLength(2);
  expect(logged[0]).toBe("dry run: would tag v0.1.1 and release it with these notes:");
  expect(sections.map((section) => section.replace("## 0.1.1\n\n", ""))).toContain(logged[1] ?? "");
}, 60_000);

test("a dry run refuses a CHANGELOG without the version's section", () => {
  const cwd = scratch("tegami-changelog");
  writeFileSync(join(cwd, "CHANGELOG.md"), "## 0.1.0\n\nOld notes.\n");

  const plan = {
    options: { dryRun: true },
    getPackagesToPublish: () => [{ name: "@ryuugg/stanza", version: "0.1.1" }],
  };

  expect(() => stanza().beforePublishAll?.call({ cwd } as never, { plan } as never)).toThrow(
    "0.1.1",
  );
});

const [headsUp, , improvedCategory, fixedCategory] = categories;
const bump = "| `@ryuugg/stanza` | `0.1.0` | `0.1.1` |";
const pull = (pr: number) => `[#${pr}](https://github.com/ryuudotgg/stanza/pull/${pr})`;
function listed(category: (typeof categories)[number], title: string, body: string, pr: number) {
  return { category, title, body, link: pull(pr) };
}

test("the version PR body lists the bump and each change with its text", () => {
  const body = versionPrBody(
    [bump],
    [
      listed(fixedCategory, "Explain counts lines like an editor", "It counts lines.", 57),
      listed(headsUp, "Rules renamed", "Rename them in your config.", 58),
    ],
  );

  expect(body).toBe(
    [
      "| Package | From | To |",
      "| --- | --- | --- |",
      bump,
      "",
      "### 🚨 Heads up",
      "",
      `- **Rules renamed** (${pull(58)})\\`,
      "  Rename them in your config.",
      "",
      "### 🐛 Fixed",
      "",
      `- **Explain counts lines like an editor** (${pull(57)})\\`,
      "  It counts lines.",
    ].join("\n"),
  );
});

test("a version PR body past GitHub's limit keeps the text of its first categories", () => {
  const text = (word: string) => Array(5000).fill(word).join(" ");
  const body = versionPrBody(
    [bump],
    [
      listed(headsUp, "Set the new flag", text("alpha"), 5),
      listed(improvedCategory, "Check runs faster", text("bravo"), 6),
      listed(fixedCategory, "Explain counts lines", text("delta"), 7),
    ],
  );

  expect(Buffer.byteLength(body)).toBeLessThanOrEqual(65_536);
  expect(body).toContain(`- **Set the new flag** (${pull(5)})\\\n  alpha`);
  expect(body).toContain(`- **Check runs faster** (${pull(6)})\\\n  bravo`);
  expect(body.endsWith(`### 🐛 Fixed\n\n- **Explain counts lines** (${pull(7)})`)).toBe(true);
});

test("a version PR body whose titles pass GitHub's limit counts the changes it leaves out", () => {
  const title = (index: number) => `Fix ${index} ${Array(400).fill("word").join(" ")}`;
  const entries = Array.from({ length: 40 }, (_, index) =>
    listed(fixedCategory, title(index + 10), "Notes.", index + 10),
  );

  const body = versionPrBody([bump], entries);
  const shown = body.split("\n").filter((line) => line.startsWith("- **Fix ")).length;

  expect(Buffer.byteLength(body)).toBeLessThanOrEqual(65_536);
  expect(shown).toBeGreaterThan(0);
  expect(shown).toBeLessThan(40);
  expect(body).toContain(`- **${title(10)}** (${pull(10)})\n`);
  expect(
    body.endsWith(`\n\nAnd ${40 - shown} more in the changelog this pull request writes.`),
  ).toBe(true);
});

const fakeFetch = `import { appendFileSync } from "node:fs";

globalThis.fetch = async (input, init = {}) => {
  const method = init.method ?? "GET";
  const body = typeof init.body === "string" ? JSON.parse(init.body) : undefined;
  appendFileSync(process.env.STANZA_TEST_REQUESTS, \`\${JSON.stringify({ method, url: String(input), body })}\\n\`);
  return method === "GET" ? Response.json([]) : Response.json({ number: 1 }, { status: 201 });
};
`;

function versionRequest(cwd: string): { code: number; output: string; bodies: string[] } {
  const sandbox = scratch("tegami-github");
  const origin = join(sandbox, "origin.git");
  const requests = join(sandbox, "requests.jsonl");
  writeFileSync(join(sandbox, "fetch.mjs"), fakeFetch);

  scratchGit(sandbox, { env }, "init", "-q", "--bare", origin);
  scratchGit(cwd, { env }, "remote", "add", "origin", origin);
  scratchGit(
    cwd,
    { env },
    "config",
    `url.${origin}.insteadOf`,
    "https://x-access-token:test@github.com/ryuudotgg/stanza.git",
  );

  const result = Bun.spawnSync(
    [process.execPath, "--preload", join(sandbox, "fetch.mjs"), script, "version"],
    {
      cwd,
      env: {
        ...env,
        CI: "true",
        GITHUB_REPOSITORY: "ryuudotgg/stanza",
        GITHUB_TOKEN: "test",
        STANZA_TEST_REQUESTS: requests,
      },
    },
  );

  const sent: { method: string; url: string; body?: { body?: string } }[] = existsSync(requests)
    ? readFileSync(requests, "utf8")
        .trim()
        .split("\n")
        .map((line) => JSON.parse(line))
    : [];

  return {
    code: result.exitCode,
    output: decoder.decode(result.stdout) + decoder.decode(result.stderr),
    bodies: sent
      .filter(
        ({ method, url }) =>
          method === "POST" && url === "https://api.github.com/repos/ryuudotgg/stanza/pulls",
      )
      .map(({ body }) => body?.body ?? ""),
  };
}

test("version opens the version PR with the bump and each note under its category", () => {
  const cwd = fixture([fixed, improved], older);
  const notes = expectedSection(cwd, "").split("\n").slice(3);
  const result = versionRequest(cwd);

  expect(result.code, result.output).toBe(0);
  expect(result.bodies).toEqual([
    [
      "| Package | From | To |",
      "| --- | --- | --- |",
      "| `@ryuugg/stanza` | `0.1.0` | `0.1.1` |",
      ...notes,
    ].join("\n"),
  ]);
}, 60_000);
