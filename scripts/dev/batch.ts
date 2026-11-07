import { realpathSync } from "node:fs";
import { basename, dirname, relative, resolve } from "node:path";
import { explainer } from "../../src/engine/explain.ts";
import type { Braces, Finding } from "../../src/engine/types.ts";
import { languageOf } from "../../src/languages/index.ts";
import { fixText, keepBraces, stepSettings, withoutMark } from "../../src/step.ts";

interface Request {
  id: unknown;
  path: string;
  text: string;
  mode: "check" | "fix";
  braces: Braces | null;
  changedLines: { lines: number[]; deletedAfter: number[] } | null;
  explain: number[];
}

interface Response {
  id: unknown;
  text: string | null;
  findings: Finding[];
  explanations: Record<string, string>;
  error: string | null;
}

function failed(id: unknown, error: string): Response {
  return { id, text: null, findings: [], explanations: {}, error };
}

function answer(request: Request, cwd: string): Response {
  const { path } = request;
  const braces = request.braces ?? undefined;
  const keep = keepBraces(path, braces);
  const changedLines = request.changedLines && {
    lines: new Set(request.changedLines.lines),
    deletedAfter: new Set(request.changedLines.deletedAfter),
  };

  const options = {
    keepBraces: keep,
    width: stepSettings(path).width,
    ...(changedLines ? { changedLines } : {}),
  };

  const result = fixText(path, request.text, request.mode, options);
  const response: Response = {
    id: request.id,
    text: result.text,
    findings: result.findings,
    explanations: {},
    error: null,
  };

  if (request.explain.length === 0) return response;

  const base = resolve(cwd, dirname(path));
  const explained = explainer({
    language: languageOf(path),
    path,
    text: withoutMark(request.text),
    braces: braces ?? (keep ? undefined : "on"),
    display: (shown) => relative(base, resolve(cwd, shown)) || basename(path),
  });

  if (typeof explained !== "function") return { ...response, error: explained.error };

  for (const line of request.explain) {
    const explanation = explained(line);
    response.explanations[line] =
      "error" in explanation ? explanation.error : explanation.lines.join("\n");
  }

  return response;
}

function respond(line: string, cwd: string): Response {
  let parsed: unknown;
  try {
    parsed = JSON.parse(line);
  } catch (error: unknown) {
    return failed(null, `malformed request: ${String(error)}`);
  }

  if (typeof parsed !== "object" || parsed === null || Array.isArray(parsed))
    return failed(null, "malformed request: not an object");

  const request = parsed as Request;
  try {
    return answer(request, cwd);
  } catch (error: unknown) {
    return failed(request.id ?? null, `internal error: ${String(error)}`);
  }
}

if (import.meta.main) {
  const cwd = realpathSync(process.cwd());
  const decoder = new TextDecoder();

  let buffered = "";
  for await (const chunk of Bun.stdin.stream()) {
    buffered += decoder.decode(chunk, { stream: true });

    let newline: number;
    while ((newline = buffered.indexOf("\n")) !== -1) {
      const line = buffered.slice(0, newline);
      buffered = buffered.slice(newline + 1);
      process.stdout.write(`${JSON.stringify(respond(line, cwd))}\n`);
    }
  }

  if (buffered !== "") process.stdout.write(`${JSON.stringify(respond(buffered, cwd))}\n`);
}
