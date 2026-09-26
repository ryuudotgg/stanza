import { extname } from "node:path";
import type { Reach } from "./layers.ts";
import { extensions } from "../files.ts";

interface CompiledGlob {
  directory: RegExp;
  filenames: Reach[];
}

interface PatternList {
  positive: CompiledGlob[];
  uncertain: boolean;
  negated: boolean;
  reaches: Map<string, Reach>;
}

const compiled = new Map<string, CompiledGlob>();
const lists = new Map<string, PatternList>();
const identities = new WeakMap<string[], Map<boolean, PatternList>>();
const allExtensions = [...extensions];

const expansionLimit = 256;

function expand(pattern: string, limit = expansionLimit): string[] | undefined {
  const match = /\{([^{}]+)\}/.exec(pattern);
  if (!match) return [pattern];

  const result: string[] = [];
  for (const part of match[1]!.split(",")) {
    const expanded = expand(
      pattern.slice(0, match.index) + part + pattern.slice(match.index + match[0].length),
      limit - result.length,
    );

    if (!expanded || result.length + expanded.length > limit) return undefined;

    result.push(...expanded);
  }

  return result;
}

function filenameReach(pattern: string, extension: string): Reach {
  if (pattern === "*" || pattern === "**" || pattern === `*${extension}`) return "all";
  if (!pattern.includes("*")) return extname(pattern) === extension ? "some" : "none";

  const ending = pattern.slice(pattern.lastIndexOf("*") + 1);
  if (ending.includes(".") && !ending.endsWith(extension) && !extension.endsWith(ending))
    return "none";

  return "some";
}

function compile(pattern: string): CompiledGlob {
  const cached = compiled.get(pattern);
  if (cached) return cached;

  const segments = pattern.split("/");
  const filename = segments.pop()!;
  if (filename === "**") segments.push("**");

  const source = segments
    .map((segment) =>
      segment === "**"
        ? "(?:[^/]+/)*"
        : `${segment.replace(/[.+^${}()|[\]\\]/g, "\\$&").replace(/\*/g, "[^/]*")}/`,
    )
    .join("");

  const glob = {
    directory: new RegExp(`^${source}$`),
    filenames: allExtensions.map((extension) => filenameReach(filename, extension)),
  };

  compiled.set(pattern, glob);
  return glob;
}

function patternList(patterns: string[], legacy: boolean): PatternList {
  const byMode = identities.get(patterns) ?? new Map<boolean, PatternList>();
  const cached = byMode.get(legacy);
  if (cached) return cached;

  const key = JSON.stringify([patterns, legacy]);

  let list = lists.get(key);
  if (!list) {
    let expanded: string[] | undefined = [];
    for (const pattern of patterns.filter((item) => !item.startsWith("!"))) {
      const alternatives: string[] | undefined = expand(pattern, expansionLimit - expanded.length);
      expanded = alternatives && [...expanded, ...alternatives];
      if (!expanded) break;
    }

    const uncertain =
      !expanded || patterns.some((pattern) => /[?[\]()+@!]/.test(pattern.replace(/^!/, "")));

    const positive =
      !expanded || uncertain
        ? []
        : expanded.map((input) => {
            const pattern = input.replace(/^\.\//, "");
            if (legacy && !pattern.includes("*"))
              return { directory: /.*/, filenames: allExtensions.map((): Reach => "some") };
            return compile(legacy && !pattern.includes("/") ? `**/${pattern}` : pattern);
          });

    list = {
      positive,
      uncertain,
      negated: patterns.some((pattern) => pattern.startsWith("!")),
      reaches: new Map(),
    };

    lists.set(key, list);
  }

  byMode.set(legacy, list);
  identities.set(patterns, byMode);
  return list;
}

export function globReach(
  patterns: string[],
  relDir: string,
  legacy: boolean,
  extension?: string,
): Reach {
  const list = patternList(patterns, legacy);
  const key = `${relDir}\0${extension ?? ""}`;
  const cached = list.reaches.get(key);
  if (cached) return cached;

  const index = extension === undefined ? -1 : allExtensions.indexOf(extension);
  const considered = index === -1 ? allExtensions.map((_, at) => at) : [index];

  let reach: Reach;
  if (list.uncertain) reach = "some";
  else {
    const dir = relDir ? `${relDir}/` : "";
    const matching = list.positive.filter((glob) => glob.directory.test(dir));
    const all = considered.every((at) => matching.some((glob) => glob.filenames[at] === "all"));
    const some = matching.some((glob) => considered.some((at) => glob.filenames[at] !== "none"));
    reach = all && !list.negated ? "all" : some ? "some" : "none";
  }

  list.reaches.set(key, reach);
  return reach;
}
