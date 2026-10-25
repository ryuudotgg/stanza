import { spawnSync } from "node:child_process";
import { existsSync, readdirSync, readFileSync } from "node:fs";
import { join } from "node:path";
import { PackagePublishTask, PublishTask, tegami } from "tegami";
import type { Draft, PackageDraft, PublishTaskContext, Tegami, TegamiPlugin } from "tegami";
import { runCli } from "tegami/cli";
import { GitCreateTagsTask } from "tegami/plugins/git";
import { github } from "tegami/plugins/github";
import { isCI } from "tegami/utils";
import {
  assertReleasable,
  changelogSection,
  distAssets,
  existingRelease,
  judge,
  publishRelease,
  repo,
} from "./release.ts";

const categories = [
  { emoji: "🚨", title: "Heads up" },
  { emoji: "✨", title: "New" },
  { emoji: "🎨", title: "Improved" },
  { emoji: "🐛", title: "Fixed" },
] as const;

type Category = (typeof categories)[number];

interface Entry {
  category: Category;
  title: string;
  body: string;
}

interface Note {
  filename: string;
  entries: Entry[];
}

const noteName = /^\d{4}-(?:0[1-9]|1[0-2])-(?:0[1-9]|[12]\d|3[01])-[a-z0-9]+(?:-[a-z0-9]+)*\.md$/;
const frontmatter = /^---\r?\n.+?\r?\n---\r?\n?/s;
const heading = /^ {0,3}(#{1,6})(?:[ \t]+|$)(.*)$/;
const fence = /^ {0,3}(?:`{3,}|~{3,})/;
const block =
  /^ {0,3}(?:[-*+](?:[ \t]|$)|\d+[.)](?:[ \t]|$)|>|\||\[[^\]]+\]:|[-=*_][-=*_ \t]*$)|^(?=[^-]*-)[ \t|:-]*\|[ \t|:-]*$/;

const html =
  /^ {0,3}(?:<[!?]|<\/?(?:address|article|aside|blockquote|details|dialog|div|dl|fieldset|figure|footer|form|h[1-6]|header|hr|main|nav|ol|p|pre|script|section|style|summary|table|textarea|ul)(?:[\s/>]|$)|<\/?[a-z][a-z0-9-]*(?:\s[^>]*)?\/?>\s*$)/i;

function parseNote(filename: string, content: string): Note {
  const refuse = (problem: string) => new Error(`.tegami/${filename}: ${problem}`);
  if (!noteName.test(filename))
    throw refuse(
      "name the note <YYYY-MM-DD>-<topic>.md, the topic in lowercase words joined by hyphens, such as 2026-10-26-explain-line-count.md",
    );

  const matter = frontmatter.exec(content)?.[0];
  if (!matter) throw refuse("start the note with frontmatter naming @ryuugg/stanza and its bump");

  const sections: { depth: number; title: string; lines: string[] }[] = [];

  let fenced = false;
  let preface = false;
  for (const line of content.slice(matter.length).split(/\r\n|\r|\n/)) {
    if (fence.test(line)) fenced = !fenced;
    const match = fenced ? null : heading.exec(line);
    if (match?.[1] && match[2] !== undefined)
      sections.push({
        depth: match[1].length,
        title: match[2].replace(/[ \t]+#+[ \t]*$/, "").trim(),
        lines: [],
      });
    else if (sections.length) sections.at(-1)?.lines.push(line);
    else if (line.trim()) preface = true;
  }

  if (!sections.length)
    throw refuse(
      'add a ### heading with its category and title, such as "### 🐛 Explain counts lines like an editor"',
    );

  if (preface) throw refuse("move the text above the first heading under a ### heading");

  return {
    filename,
    entries: sections.map(({ depth, title, lines }) => {
      if (depth !== 3)
        throw refuse(
          `"${title}" is a ${"#".repeat(depth)} heading: give each change one ### heading and no other headings`,
        );

      const category = categories.find(({ emoji }) => title.startsWith(emoji));
      const name = category && /^\uFE0F? +(\S.*)$/.exec(title.slice(category.emoji.length))?.[1];

      if (category && !title.slice(category.emoji.length).replace("\uFE0F", "").trim())
        throw refuse(`give the heading "${title}" a title after its emoji`);

      if (!category || !name)
        throw refuse(
          `start the heading "${title}" with one of ${categories.map(({ emoji }) => emoji).join(" ")}, a space and the title`,
        );

      const start = lines.findIndex((line) => line.trim());
      const body =
        start === -1 ? [] : lines.slice(start, lines.findLastIndex((line) => line.trim()) + 1);

      if (!body.length) throw refuse(`write one plain paragraph under "${title}"`);
      if (
        /^(?: {4}|\t)/.test(body[0] ?? "") ||
        body.some((line) => !line.trim() || fence.test(line) || block.test(line) || html.test(line))
      )
        throw refuse(
          `keep the body under "${title}" to one plain paragraph, with no list, table, code block or second paragraph`,
        );

      return {
        category,
        title: name.trim(),
        body: body.map((line) => line.trim().replace(/\s*\\$/, "")).join(" "),
      };
    }),
  };
}

function checkNotes(dir: string): void {
  if (!existsSync(dir)) return;

  for (const name of readdirSync(dir)
    .filter((name) => name.endsWith(".md"))
    .sort())
    parseNote(name, readFileSync(join(dir, name), "utf8"));
}

function reference(dir: string, repository: string, filename: string): string {
  const added = spawnSync(
    "git",
    ["log", "-n", "1", "--diff-filter=A", "--format=%H%x09%h%x09%s", "--", filename],
    { cwd: dir, encoding: "utf8" },
  );

  if (added.error) throw added.error;

  const [full, short, ...subject] = added.status === 0 ? added.stdout.trim().split("\t") : [];
  if (!full || !short) return "";

  const pr = /\(#(\d+)\)$/.exec(subject.join("\t"))?.[1];
  return pr
    ? `[#${pr}](https://github.com/${repository}/pull/${pr})`
    : `[${short}](https://github.com/${repository}/commit/${full})`;
}

function renderSection(cwd: string, dir: string, version: string, notes: Note[]): string {
  const repository = repo(cwd);
  const entries = notes
    .toSorted((a, b) => (a.filename < b.filename ? -1 : 1))
    .flatMap((note) => {
      const link = reference(dir, repository, note.filename);
      return note.entries.map((entry) => ({ ...entry, link }));
    });

  const date = new Date().toLocaleDateString("en-US", { dateStyle: "long", timeZone: "UTC" });
  const lines = [`## ${version}`, "", `_${date}_`];
  for (const category of categories) {
    const listed = entries.filter((entry) => entry.category === category);
    if (listed.length)
      lines.push(
        "",
        `### ${category.emoji} ${category.title}`,
        "",
        ...listed.map(
          ({ title, body, link }) => `- **${title}**${link ? ` (${link})` : ""}\\\n  ${body}`,
        ),
      );
  }

  return lines.join("\n");
}

function draftNotes(packageDraft: PackageDraft): Note[] {
  return (packageDraft.changelogs ?? []).map((entry) =>
    parseNote(entry.filename, entry.getRawContent()),
  );
}

function assertPending(draft: Draft): void {
  if (!draft.hasPending())
    throw new Error("no pending notes in .tegami/: add one naming @ryuugg/stanza and its bump");
}

export class ReleaseTask extends PublishTask<void> {
  name = "stanza:release";

  override link({ plan }: PublishTaskContext): void {
    for (const task of plan.tasks)
      if (task instanceof PackagePublishTask || task instanceof GitCreateTagsTask)
        this.optionalWait.push(task);
  }

  override status({ context, plan }: PublishTaskContext): "done" | "pending" {
    if (plan.options.dryRun) return "pending";

    for (const pkg of plan.getPackagesToPublish()) {
      const tag = plan.packages.get(pkg.id)?.git?.tag;
      if (!tag) continue;

      const existing = existingRelease(context.cwd, tag);
      if (!existing || existing.draft || existing.assets.some((asset) => asset.digest === ""))
        return "pending";

      const dist = join(context.cwd, "dist");
      if (existsSync(dist) && judge(distAssets(dist), existing).kind !== "matches")
        return "pending";
    }

    return "done";
  }

  override run({ context, plan }: PublishTaskContext): void {
    if (plan.options.dryRun) return;
    if (this.optionalWait.some((task) => task.getResult()?.status === "failed")) return;

    for (const pkg of plan.getPackagesToPublish()) {
      if (!pkg.version) continue;
      publishRelease({ cwd: context.cwd, dist: join(context.cwd, "dist"), version: pkg.version });
    }
  }
}

let applied = false;

export function stanza(): TegamiPlugin {
  return {
    name: "stanza",
    enforce: "pre",
    initPublishPlan({ plan }) {
      for (const [id, packagePlan] of plan.packages) {
        const pkg = this.graph.get(id);
        if (pkg?.version) (packagePlan.git ??= {}).tag = `v${pkg.version}`;
      }
    },
    applyCliDraft() {
      applied = true;
    },
    initCliDraft(draft) {
      assertPending(draft);
    },
    beforePublishAll({ plan }) {
      if (!plan.options.dryRun && !isCI())
        throw new Error(
          "publish runs only in CI, where the tag is pushed before the release is created",
        );

      for (const pkg of plan.getPackagesToPublish()) {
        if (!pkg.version) continue;

        const notes = changelogSection(
          readFileSync(join(this.cwd, "CHANGELOG.md"), "utf8"),
          pkg.version,
        );

        if (plan.options.dryRun) {
          console.log(`dry run: would tag v${pkg.version} and release it with these notes:`);
          console.log(notes);
          continue;
        }

        const built = distAssets(join(this.cwd, "dist"));
        const tag = `v${pkg.version}`;
        const existing = existingRelease(this.cwd, tag);
        assertReleasable(judge(built, existing), tag);
      }
    },
    publishTasks() {
      return new ReleaseTask();
    },
  };
}

async function preview(release: Tegami): Promise<void> {
  const draft = await release.draft();
  assertPending(draft);

  const { cwd, changelogDir, graph } = await release._internal.context();
  for (const pkg of graph.getByName("@ryuugg/stanza")) {
    const packageDraft = draft.getPackageDraft(pkg.id);
    const version = packageDraft?.bumpVersion(pkg);
    if (packageDraft && version)
      console.log(renderSection(cwd, changelogDir, version, draftNotes(packageDraft)));
  }
}

const [command, ...flags] = process.argv.slice(2);
const dryRun = command === "version" && flags.includes("--dry-run");
if (import.meta.main) {
  const release = tegami({
    ignore: ["docs"],
    generator: {
      generate({ pkg, packageDraft }) {
        return pkg.version
          ? renderSection(this.cwd, this.changelogDir, pkg.version, draftNotes(packageDraft))
          : "";
      },
    },
    plugins: [
      github({
        release: false,
        versionPr: {
          create() {
            return {
              title: `chore: release ${this.graph.getByName("@ryuugg/stanza")[0]?.version}`,
            };
          },
          commit({ type }) {
            if (type === "version-packages")
              return {
                title: `chore: release ${this.graph.getByName("@ryuugg/stanza")[0]?.version}`,
              };
          },
        },
      }),
      stanza(),
    ],
  });

  try {
    if (command === "version" || command === "ci") checkNotes(join(process.cwd(), ".tegami"));
    if (dryRun) await preview(release);
    else await runCli(release);
  } catch (error) {
    console.error(error instanceof Error ? error.message : String(error));
    process.exitCode = 1;
  }

  if (command === "version" && !dryRun && !applied && !process.exitCode) {
    console.error("tegami wrote no publish lock");
    process.exitCode = 1;
  }
}
