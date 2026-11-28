import { mkdirSync, writeFileSync } from "node:fs";
import { basename, dirname, join } from "node:path";
import { type RunResult, run, scratch } from "./support.ts";

type Line = string | Record<string, unknown>;

export function writeTranscript(path: string, ...lines: Line[]): string {
  const text = lines.map((line) => (typeof line === "string" ? line : JSON.stringify(line)));
  mkdirSync(dirname(path), { recursive: true });
  writeFileSync(path, text.join("\n"));
  return path;
}

export function transcript(...lines: Line[]): string {
  return writeTranscript(join(scratch("hook-transcript"), "stop.jsonl"), ...lines);
}

export function subagentTranscript(main: string, agent: string, ...lines: Line[]): string {
  const directory = join(dirname(main), basename(main, ".jsonl"), "subagents");
  return writeTranscript(join(directory, `${agent}.jsonl`), ...lines);
}

export function toolCalls(
  ...calls: [name: string, path: string, failed?: boolean][]
): [attempts: Record<string, unknown>, results: Record<string, unknown>] {
  const uses = calls.map(([name, file_path], index) => ({
    type: "tool_use",
    id: `t${index}`,
    name,
    input: { file_path },
  }));

  const results = calls.map(([, , failed], index) => ({
    type: "tool_result",
    tool_use_id: `t${index}`,
    content: failed ? "String to replace not found in file." : "ok",
    is_error: failed ?? false,
  }));

  return [
    { type: "assistant", message: { role: "assistant", content: uses } },
    { type: "user", message: { role: "user", content: results } },
  ];
}

export function codexRollout(meta: Record<string, unknown>, ...records: Line[]): string {
  return transcript(
    {
      timestamp: "2026-09-30T00:00:00Z",
      type: "session_meta",
      payload: { id: "thread-1", cli_version: "0.159.0", source: "exec", ...meta },
    },
    { type: "event_msg", payload: { type: "task_started", turn_id: "turn-1" } },
    ...records,
  );
}

export function codexExec(path: string, applied = true): Record<string, unknown>[] {
  const patch = `*** Begin Patch\n*** Update File: ${path}\n@@\n-old\n+new\n*** End Patch`;
  return [
    {
      type: "response_item",
      payload: {
        type: "custom_tool_call",
        status: "completed",
        call_id: `call_${basename(path)}`,
        name: "exec",
        input: `const patch = ${JSON.stringify(patch)};\nconst result = await tools.apply_patch(patch);\ntext(result);\n`,
      },
    },
    {
      type: "response_item",
      payload: {
        type: "custom_tool_call_output",
        call_id: `call_${basename(path)}`,
        output: [
          { type: "input_text", text: applied ? "Script completed\n" : "Script failed\n" },
          {
            type: "input_text",
            text: applied
              ? `Success. Updated the following files:\nM ${path}\n`
              : "Script error:\npatch rejected: writing is blocked by read-only sandbox; rejected by user approval settings",
          },
        ],
      },
    },
  ];
}

export function codexFileChange(
  changes: Record<string, unknown>,
  status = "completed",
): Record<string, unknown> {
  return {
    type: "event_msg",
    payload: {
      type: "item_completed",
      thread_id: "thread-1",
      turn_id: "turn-1",
      item: {
        type: "FileChange",
        id: "exec-1",
        changes,
        status,
        stdout: "Success. Updated the following files:\n",
        stderr: "",
      },
      started_at_ms: 1,
      completed_at_ms: 2,
    },
  };
}

export function postToolUse(
  cwd: string,
  tool: string,
  toolInput: Record<string, unknown>,
  toolResponse?: unknown,
): Record<string, unknown> {
  return {
    cwd,
    hook_event_name: "PostToolUse",
    tool_name: tool,
    tool_input: toolInput,
    tool_response: toolResponse,
  };
}

interface HookOptions {
  args?: string[];
  cwd?: string;
  env?: NodeJS.ProcessEnv;
}

export function hook(
  input: string | Uint8Array | Record<string, unknown>,
  { args = [], cwd = scratch("hook"), env }: HookOptions = {},
): RunResult {
  const stdin =
    input instanceof Uint8Array
      ? input
      : Buffer.from(typeof input === "string" ? input : JSON.stringify(input));

  return run({ cwd, env, stdin }, "hook", ...args);
}
