import { dirname, extname } from "node:path";
import { isGeneratedHeader } from "./files.ts";
import { processFile } from "./engine/index.ts";
import type { Changed } from "./engine/model.ts";
import { languageOf } from "./languages/index.ts";
import type { Braces, FileResult, Finding, Mode, Options } from "./engine/types.ts";

export type Decoded = string | { message: string };

export interface StepOptions {
  mode: Mode;
  braces?: Braces | undefined;
  changedLines?: Changed | undefined;
  unread?: Set<string>;
}

export interface StepResult {
  findings: Finding[];
  fixed: string | undefined;
  parseError: boolean;
}

const utf8 = new TextDecoder("utf-8", { fatal: true, ignoreBOM: true });
const bom = "﻿";

export function decode(bytes: Uint8Array): Decoded {
  try {
    return utf8.decode(bytes);
  } catch {
    return { message: "not valid UTF-8, left untouched" };
  }
}

export function withoutMark(text: string): string {
  return text.startsWith(bom) ? text.slice(bom.length) : text;
}

export function keepBraces(path: string, braces?: Braces, unread?: Set<string>): boolean {
  if (braces !== undefined) return braces === "off";

  const setting = languageOf(path).config?.setting(dirname(path), extname(path)) ?? {
    enforced: false,
    unread: [],
  };

  for (const file of setting.unread) unread?.add(file);
  return setting.enforced;
}

export function fixText(path: string, text: string, mode: Mode, options: Options): FileResult {
  const body = withoutMark(text);
  const mark = text.slice(0, text.length - body.length);
  const result = processFile(path, body, mode, options);
  return { ...result, text: mark + result.text };
}

function failure(path: string, rule: "parse" | "error", message: string): StepResult {
  return {
    findings: [{ path, line: 1, col: 1, rule, message, fixable: false }],
    fixed: undefined,
    parseError: true,
  };
}

export function formatText(path: string, text: Decoded, options: StepOptions): StepResult {
  if (typeof text !== "string") return failure(path, "parse", text.message);

  try {
    if (isGeneratedHeader(text)) return { findings: [], fixed: undefined, parseError: false };

    const result = fixText(path, text, options.mode, {
      keepBraces: keepBraces(path, options.braces, options.unread),
      ...(options.changedLines === undefined ? {} : { changedLines: options.changedLines }),
    });

    const changed = options.mode === "fix" && !result.parseError && result.text !== text;
    if (changed) {
      const error = languageOf(path).parse(path, withoutMark(result.text)).error;
      if (error)
        return failure(
          path,
          "error",
          `fixed text does not parse (${error.message}), left untouched`,
        );
    }

    return {
      findings: result.findings,
      fixed: changed ? result.text : undefined,
      parseError: result.parseError,
    };
  } catch (error: unknown) {
    return failure(path, "error", String(error));
  }
}
