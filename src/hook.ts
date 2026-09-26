import { resolve } from "node:path";
import { RULES } from "./rules.ts";
import type { Finding } from "./types.ts";

export function hookInput(
  text: string,
  fallbackCwd: string,
): { cwd: string; stopHookActive: boolean } | { error: string } {
  let input: unknown;
  try {
    input = JSON.parse(text);
  } catch {
    return { error: "input must be valid JSON" };
  }

  if (input === null || typeof input !== "object" || Array.isArray(input))
    return { error: "input must be a JSON object" };

  const cwd = "cwd" in input ? input.cwd : undefined;
  if (cwd != null && typeof cwd !== "string") return { error: "cwd must be a string" };

  const stopHookActive = "stop_hook_active" in input ? input.stop_hook_active : undefined;
  if (stopHookActive != null && typeof stopHookActive !== "boolean")
    return { error: "stop_hook_active must be a boolean" };

  return {
    cwd: cwd == null ? fallbackCwd : resolve(fallbackCwd, cwd),
    stopHookActive: stopHookActive ?? false,
  };
}

const failures: Partial<Record<Finding["rule"], string>> = {
  parse: "could not read or parse this file",
  error: "stanza failed on this file",
  write: "could not write the fixes to this file",
};

export function blockReason(findings: Finding[], rewritten: string[]): string | undefined {
  if (findings.length === 0) return undefined;

  const shown = findings.slice(0, 12);
  const lines = shown.map((finding) => {
    const failure = failures[finding.rule];
    const message = failure
      ? `${failure}: ${finding.message}`
      : `${finding.rule} ${finding.message}`;

    return `  ${finding.path}:${finding.line}:${finding.col} ${message}`;
  });

  if (findings.length > shown.length)
    lines.push(`  ... and ${findings.length - shown.length} more`);

  const present = new Set<string>(findings.map((finding) => finding.rule));
  const rules = Object.entries(RULES)
    .filter(([id]) => present.has(id))
    .map(([id, rule]) => `${id}: ${rule.summary}`);

  const sections = [["stanza could not fix these in the files you changed:", ...lines].join("\n")];
  if (rules.length > 0) sections.push(rules.join("\n"));

  sections.push("Fix those findings, then reply again.");

  const paths = new Set(findings.map((finding) => finding.path));
  const reread = rewritten.filter((path) => paths.has(path));
  if (reread.length > 0) {
    const names =
      reread.length === 1 ? reread[0] : `${reread.slice(0, -1).join(", ")} and ${reread.at(-1)}`;

    sections.push(
      `stanza rewrote ${names}, so read ${reread.length === 1 ? "it" : "them"} again before editing.`,
    );
  }

  return sections.join("\n\n");
}
