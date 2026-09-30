import { existsSync, readFileSync } from "node:fs";
import { dirname, extname, resolve } from "node:path";
import type { Root } from "mdast";
import type {} from "mdast-util-mdx-jsx";
import type { ShikiTransformer } from "shiki";
import { visit } from "unist-util-visit";
import type { VFile } from "vfile";

export const diffExampleToken = "diff-example";

export type DiffLine = { kind: "same" | "add" | "remove"; text: string };

function lines(text: string): string[] {
  return text.replace(/\n$/, "").split("\n");
}

export function diffLines(before: string, after: string): DiffLine[] {
  const a = lines(before);
  const b = lines(after);
  const table = Array.from({ length: a.length + 1 }, () =>
    Array.from({ length: b.length + 1 }, () => 0),
  );

  for (let i = a.length - 1; i >= 0; i--)
    for (let j = b.length - 1; j >= 0; j--)
      table[i]![j] =
        a[i] === b[j] ? table[i + 1]![j + 1]! + 1 : Math.max(table[i + 1]![j]!, table[i]![j + 1]!);

  const result: DiffLine[] = [];
  let i = 0;
  let j = 0;

  while (i < a.length || j < b.length) {
    if (i < a.length && j < b.length && a[i] === b[j]) {
      result.push({ kind: "same", text: a[i]! });
      i++;
      j++;
    } else if (i < a.length && (j === b.length || table[i + 1]![j]! >= table[i]![j + 1]!)) {
      result.push({ kind: "remove", text: a[i]! });
      i++;
    } else {
      result.push({ kind: "add", text: b[j]! });
      j++;
    }
  }

  return result;
}

function repoRoot(path: string): string {
  let directory = resolve(path);

  while (true) {
    if (existsSync(resolve(directory, "tests", "fixtures"))) return directory;

    const parent = dirname(directory);
    if (parent === directory) throw new Error(`DiffExample: could not find repo root from ${path}`);
    directory = parent;
  }
}

export function remarkDiffExample() {
  return (tree: Root, file: VFile) => {
    visit(tree, "mdxJsxFlowElement", (node, index, parent) => {
      if (node.name !== "DiffExample") return;

      const fixture = node.attributes.find(
        (attribute) => attribute.type === "mdxJsxAttribute" && attribute.name === "fixture",
      )?.value;
      if (typeof fixture !== "string" || !fixture)
        throw new Error(`DiffExample: missing fixture in ${file.path ?? file.cwd}`);

      const extension = extname(fixture);
      if (!extension)
        throw new Error(
          `DiffExample: fixture "${fixture}" has no extension in ${file.path ?? file.cwd}`,
        );

      const root = repoRoot(file.path ? dirname(file.path) : (file.cwd ?? ""));
      const stem = fixture.slice(0, -extension.length);
      const beforePath = resolve(root, "tests", "fixtures", `${stem}.before${extension}`);
      const afterPath = resolve(root, "tests", "fixtures", `${stem}.after${extension}`);

      for (const path of [beforePath, afterPath]) {
        if (!existsSync(path))
          throw new Error(`DiffExample: missing ${path} in ${file.path ?? file.cwd}`);
        (file.data._compiler as { addDependency(path: string): void } | undefined)?.addDependency(
          path,
        );
      }

      const diff = diffLines(readFileSync(beforePath, "utf8"), readFileSync(afterPath, "utf8"));
      parent!.children[index!] = {
        type: "code",
        lang: extension.slice(1),
        meta: `${diffExampleToken} noCopy`,
        value: diff
          .map(({ kind, text }) => `${kind === "add" ? "+" : kind === "remove" ? "-" : " "}${text}`)
          .join("\n"),
      };
    });
  };
}

export function transformerDiffExample(): ShikiTransformer {
  return {
    name: "diff-example",
    preprocess(code, options) {
      if (
        !String(options.meta?.__raw ?? "")
          .split(/\s+/)
          .includes(diffExampleToken)
      )
        return code;

      const prefixed = code.split("\n");
      (this.meta as typeof this.meta & { diffKinds: DiffLine["kind"][] }).diffKinds = prefixed.map(
        (line) => (line[0] === "+" ? "add" : line[0] === "-" ? "remove" : "same"),
      );
      return prefixed.map((line) => line.slice(1)).join("\n");
    },
    line(node, line) {
      const kind = (this.meta as typeof this.meta & { diffKinds?: DiffLine["kind"][] }).diffKinds?.[
        line - 1
      ];
      if (kind === "add" || kind === "remove") this.addClassToHast(node, ["diff", kind]);
    },
  };
}
