import { readFileSync, readdirSync } from "node:fs";
import { basename, dirname, isAbsolute, join, resolve } from "node:path";
import { RULES } from "./engine/rules.ts";
import type { Finding } from "./engine/types.ts";

export const claudeCodeHooks =
  '{ "hooks": { "PreToolUse": [{ "matcher": "Write", "hooks": [{ "type": "command", "command": "stanza hook" }] }], "PostToolUse": [{ "matcher": "Edit|MultiEdit", "hooks": [{ "type": "command", "command": "stanza hook" }] }], "Stop": [{ "hooks": [{ "type": "command", "command": "stanza hook" }] }] } }';

export function hookInput(
  text: string,
  fallbackCwd: string,
):
  | { event: "stop"; cwd: string; stopHookActive: boolean; transcriptPath: string | undefined }
  | {
      event: "write";
      cwd: string;
      toolInput: { file_path: string; content: string } & Record<string, unknown>;
    }
  | { event: "edit"; cwd: string; paths: string[] }
  | { event: "ignored" }
  | { error: string } {
  let input: unknown;
  try {
    input = JSON.parse(text);
  } catch {
    return { error: "input must be valid JSON" };
  }

  if (!isRecord(input)) return { error: "input must be a JSON object" };

  const event = "hook_event_name" in input ? input.hook_event_name : undefined;
  if (event != null && typeof event !== "string")
    return { error: "hook_event_name must be a string" };

  if (event != null && !events.has(event)) return { event: "ignored" };

  const cwd = "cwd" in input ? input.cwd : undefined;
  if (cwd != null && typeof cwd !== "string") return { error: "cwd must be a string" };

  const dir = cwd == null ? fallbackCwd : resolve(fallbackCwd, cwd);
  if (event === "PostToolUse") {
    const paths = editedPaths(input);
    if (paths.length === 0) return { event: "ignored" };
    return { event: "edit", cwd: dir, paths: paths.map((path) => resolve(dir, path)) };
  }

  if (event === "PreToolUse") {
    const toolInput = "tool_input" in input ? input.tool_input : undefined;
    if (
      !("tool_name" in input && input.tool_name === "Write") ||
      !isRecord(toolInput) ||
      typeof toolInput.file_path !== "string" ||
      typeof toolInput.content !== "string"
    )
      return { event: "ignored" };

    return {
      event: "write",
      cwd: dir,
      toolInput: { ...toolInput, file_path: toolInput.file_path, content: toolInput.content },
    };
  }

  const transcriptPath = "transcript_path" in input ? input.transcript_path : undefined;
  if (transcriptPath != null && typeof transcriptPath !== "string")
    return { error: "transcript_path must be a string" };

  const stopHookActive = "stop_hook_active" in input ? input.stop_hook_active : undefined;
  if (stopHookActive != null && typeof stopHookActive !== "boolean")
    return { error: "stop_hook_active must be a boolean" };

  return {
    event: "stop",
    cwd: dir,
    stopHookActive: stopHookActive ?? false,
    transcriptPath: transcriptPath == null ? undefined : resolve(fallbackCwd, transcriptPath),
  };
}

const events = new Set(["Stop", "SubagentStop", "PreToolUse", "PostToolUse"]);
const patchHeader = /^\*\*\* (Add File|Update File|Move to): (.+)$/;
const patchApplied = /^Success\. Updated the following files:$/m;
function editedPaths(input: Record<string, unknown>): string[] {
  const toolInput = input.tool_input;
  if (!isRecord(toolInput)) return [];
  if (input.tool_name === "Edit" || input.tool_name === "MultiEdit")
    return typeof toolInput.file_path === "string" ? [toolInput.file_path] : [];

  if (input.tool_name !== "apply_patch" || typeof toolInput.command !== "string") return [];
  if (typeof input.tool_response !== "string" || !patchApplied.test(input.tool_response)) return [];

  const paths: string[] = [];
  for (const line of toolInput.command.split(/\r?\n/)) {
    const [, kind, path] = patchHeader.exec(line) ?? [];
    if (path === undefined) continue;
    if (kind === "Move to") paths.pop();
    paths.push(path);
  }

  return paths;
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return value !== null && typeof value === "object" && !Array.isArray(value);
}

const toolUseLine = /"type"\s*:\s*"tool_use"/;
const failedResultLine = /"is_error"\s*:\s*true/;
const itemCompletedLine = /"item_completed"/;

export function writtenFiles(transcriptPath: string): Set<string> | undefined {
  let text: string;
  try {
    text = readFileSync(transcriptPath, "utf8");
  } catch {
    return undefined;
  }

  const codex = writtenFilesInCodexRollout(text);
  if (codex !== undefined) return codex.size > 0 ? codex : undefined;

  const main = writtenFilesInTranscript(text);
  if (main === undefined) return undefined;

  const written = new Set(main);
  const subagents = join(dirname(transcriptPath), basename(transcriptPath, ".jsonl"), "subagents");

  let names: string[];
  try {
    names = readdirSync(subagents);
  } catch {
    return written;
  }

  for (const name of names) {
    if (!name.endsWith(".jsonl")) continue;

    let text: string;
    try {
      text = readFileSync(join(subagents, name), "utf8");
    } catch {
      continue;
    }

    const subagent = writtenFilesInTranscript(text);
    if (subagent === undefined) continue;

    for (const path of subagent) written.add(path);
  }

  return written;
}

function writtenFilesInCodexRollout(text: string): Set<string> | undefined {
  const written = new Set<string>();

  let recognized = false;
  for (const line of text.split("\n")) {
    if (!itemCompletedLine.test(line)) continue;

    let record: unknown;
    try {
      record = JSON.parse(line);
    } catch {
      continue;
    }

    if (!isRecord(record) || record.type !== "event_msg" || !isRecord(record.payload)) continue;
    if (record.payload.type !== "item_completed") continue;

    recognized = true;

    const item = record.payload.item;
    if (!isRecord(item) || item.type !== "FileChange" || item.status !== "completed") continue;
    if (!isRecord(item.changes)) continue;

    for (const [source, change] of Object.entries(item.changes)) {
      if (!isRecord(change) || change.type === "delete") continue;

      const path = typeof change.move_path === "string" ? change.move_path : source;
      if (!isAbsolute(path)) continue;

      written.add(path);
    }
  }

  return recognized ? written : undefined;
}

function writtenFilesInTranscript(text: string): Set<string> | undefined {
  const attempted = new Map<unknown, string>();
  const failed = new Set<unknown>();

  let recognized = false;
  for (const line of text.split("\n")) {
    if (recognized && !toolUseLine.test(line) && !failedResultLine.test(line)) continue;

    let record: unknown;
    try {
      record = JSON.parse(line);
    } catch {
      continue;
    }

    if (!isRecord(record) || !isRecord(record.message)) continue;

    const content = record.message.content;
    if (!Array.isArray(content)) continue;
    if (record.type === "assistant") recognized = true;

    for (const item of content) {
      if (!isRecord(item)) continue;

      const path = editedPath(item);
      if (record.type === "assistant" && path !== undefined) attempted.set(item.id, path);
      if (record.type === "user" && item.type === "tool_result" && item.is_error === true)
        failed.add(item.tool_use_id);
    }
  }

  if (!recognized) return undefined;

  const written = new Set<string>();
  for (const [id, path] of attempted) if (!failed.has(id)) written.add(path);

  return written;
}

const editTools = new Set(["Write", "Edit", "MultiEdit"]);
function editedPath(item: Record<string, unknown>): string | undefined {
  if (item.type !== "tool_use" || !editTools.has(String(item.name))) return undefined;
  if (!isRecord(item.input) || typeof item.input.file_path !== "string") return undefined;
  return isAbsolute(item.input.file_path) ? item.input.file_path : undefined;
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
  if (reread.length > 0) sections.push(rereadLine("rewrote", reread));

  return sections.join("\n\n");
}

export function rereadLine(verb: string, paths: string[]): string {
  const names =
    paths.length === 1 ? paths[0] : `${paths.slice(0, -1).join(", ")} and ${paths.at(-1)}`;

  return `stanza ${verb} ${names}, so read ${paths.length === 1 ? "it" : "them"} again before editing.`;
}
