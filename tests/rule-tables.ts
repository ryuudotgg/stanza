import { RULES } from "../src/engine/rules.ts";

export function row(line: string): [string, string] {
  const [id = "", summary = ""] = line
    .split(/(?<!\\)\|/)
    .slice(1, -1)
    .map((cell) => cell.trim().replaceAll("\\|", "|"));

  return [id.replace(/^\[`(.*)`\]\([^)]*\)$/, "$1").replace(/^`(.*)`$/, "$1"), summary];
}

export function catalog(fixable: boolean): [string, string][] {
  return Object.entries(RULES)
    .filter(([, rule]) => rule.fixable === fixable)
    .map(([id, rule]) => [id, rule.summary]);
}
