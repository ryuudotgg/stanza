import type { Root } from "mdast";
import type {} from "mdast-util-mdx-jsx";
import { visit } from "unist-util-visit";
import type { VFile } from "vfile";
import catalog from "./catalog.json";

type Json = null | boolean | number | string | Json[] | { [key: string]: Json };

const WIDTH = 100;

const INDENT = 2;

function flat(value: Json): string {
  if (Array.isArray(value)) return `[${value.map(flat).join(", ")}]`;
  if (value === null || typeof value !== "object") return JSON.stringify(value);

  const entries = Object.entries(value);
  if (entries.length === 0) return "{}";

  return `{ ${entries.map(([key, item]) => `${JSON.stringify(key)}: ${flat(item)}`).join(", ")} }`;
}

function layout(value: Json, indent: number, before: number, after: number): string {
  const line = flat(value);
  if (before + line.length + after <= WIDTH) return line;

  const items: [string, Json][] = Array.isArray(value)
    ? value.map((item) => ["", item])
    : value !== null && typeof value === "object"
      ? Object.entries(value).map(([key, item]) => [`${JSON.stringify(key)}: `, item])
      : [];

  if (items.length === 0) return line;

  const [open, close] = Array.isArray(value) ? ["[", "]"] : ["{", "}"];
  const inner = indent + INDENT;
  const lines = items.map(([key, item], index) => {
    const comma = index < items.length - 1 ? "," : "";
    return `${" ".repeat(inner)}${key}${layout(item, inner, inner + key.length, comma.length)}${comma}`;
  });

  return `${open}\n${lines.join("\n")}\n${" ".repeat(indent)}${close}`;
}

export function printJson(value: Json): string {
  return layout(value, 0, 0, 0);
}

export function hookSettings(title: string, hunks: boolean): Json {
  const registration = Object.values(catalog.registrations).find(({ files }) =>
    files.includes(title),
  );

  if (!registration) throw new Error(`HookRegistration: no registration is written to ${title}`);

  const text = JSON.stringify(registration.settings);
  return JSON.parse(text, (key, value) =>
    hunks && key === "command" ? `${value} --hunks` : value,
  ) as Json;
}

export function remarkHooks() {
  return (tree: Root, file: VFile) => {
    visit(tree, "mdxJsxFlowElement", (node, index, parent) => {
      if (node.name !== "HookRegistration") return;

      const attributes: Record<string, string | null> = {};
      for (const attribute of node.attributes) {
        const { value } = attribute;
        if (attribute.type !== "mdxJsxAttribute" || (value !== null && typeof value === "object"))
          throw new Error(`HookRegistration: only plain attributes are read, in ${file.path}`);

        if (Object.hasOwn(attributes, attribute.name))
          throw new Error(`HookRegistration: ${attribute.name} given twice in ${file.path}`);

        attributes[attribute.name] = value ?? null;
      }

      const { title, tab, hunks, ...rest } = attributes;
      if (Object.keys(rest).length > 0)
        throw new Error(
          `HookRegistration: unknown ${Object.keys(rest).join(", ")} in ${file.path}`,
        );

      const plainTab = tab === undefined || (typeof tab === "string" && /^[^"]+$/.test(tab));
      if (typeof title !== "string" || !plainTab || (hunks !== undefined && hunks !== null))
        throw new Error(
          `HookRegistration: expected title="...", an optional tab="..." and a bare hunks in ${file.path}`,
        );

      const meta = tab === undefined ? `title="${title}"` : `tab="${tab}" title="${title}"`;
      parent!.children[index!] = {
        type: "code",
        lang: "json",
        meta,
        value: printJson(hookSettings(title, hunks === null)),
      };
    });
  };
}
