import { existsSync, readFileSync } from "node:fs";
import { join } from "node:path";
import { PackagePublishTask, PublishTask, tegami } from "tegami";
import type { PublishTaskContext, TegamiPlugin } from "tegami";
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
} from "./release.ts";

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
      if (!draft.hasPending())
        throw new Error("no pending notes in .tegami/: add one naming @ryuugg/stanza and its bump");
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

if (import.meta.main)
  await runCli(
    tegami({
      ignore: ["docs"],
      generator: {
        generate({ pkg, packageDraft }) {
          const lines = [`## ${pkg.version}`, ""];
          for (const entry of packageDraft.changelogs ?? []) {
            if (entry.subject) lines.push(`### ${entry.subject}`, "");

            for (const section of entry.sections)
              lines.push(
                `${entry.subject ? "####" : "###"} ${section.title}`,
                "",
                section.content,
                "",
              );
          }

          return lines.join("\n").trim();
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
    }),
  );

if (import.meta.main && process.argv[2] === "version" && !applied) {
  console.error("tegami wrote no publish lock");
  process.exitCode = 1;
}
