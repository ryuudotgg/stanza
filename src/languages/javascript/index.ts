import type { Language } from "../language.ts";
import { braceDecisions, bracesSetting } from "./config/index.ts";
import { formatterWidth } from "./config/width.ts";
import { parse, rejection } from "./parse.ts";
import { scan } from "./scan.ts";

export const language: Language = {
  parse(path, text) {
    const parsed = parse(path, text);
    const error = parsed.errors[0];
    return {
      comments: parsed.comments,
      error: error ? { start: error.labels[0]?.start ?? 0, message: error.message } : undefined,
      rejection: (text) => rejection(text, parsed),
      scan: (doc, width) => scan(doc, parsed.program, width),
    };
  },
  config: { setting: bracesSetting, decisions: braceDecisions, width: formatterWidth },
  oracle: () => (require("./oracle.ts") as typeof import("./oracle.ts")).oracle,
};
