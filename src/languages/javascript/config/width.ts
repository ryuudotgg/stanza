import { existsSync, readFileSync } from "node:fs";
import { basename, dirname, isAbsolute, join, relative, resolve, sep } from "node:path";
import type { Width } from "../../language.ts";
import { UNKNOWN, type Value, exported, object, property } from "./evaluate.ts";
import { configDirectories, realDirectory } from "./find.ts";
import { moduleAt } from "./module.ts";

type Formatter = "oxfmt" | "prettier" | "biome";
type Config = { file: string; formatter: Formatter; value: Value };
type Directory = { configs: Config[]; unread: string[] };
type Read = { kind: "config"; config: Config } | { kind: "skip" } | { kind: "unread" };
type Setting = { columns?: number; tab?: number; editorconfig: boolean };
type Editor = {
  root: boolean;
  sections: { pattern: string | null; values: Record<string, string> }[];
};
type FormatKeys = {
  width: string;
  tab: string;
  editorconfig?: string;
  files: string;
  legacyFiles?: string;
  options: string;
  defaultWidth: number;
  basenameOnly: boolean;
};

const formats: Record<Formatter, FormatKeys> = {
  oxfmt: {
    width: "printWidth",
    tab: "tabWidth",
    files: "files",
    options: "options",
    defaultWidth: 100,
    basenameOnly: true,
  },
  prettier: {
    width: "printWidth",
    tab: "tabWidth",
    files: "files",
    options: "options",
    defaultWidth: 80,
    basenameOnly: true,
  },
  biome: {
    width: "lineWidth",
    tab: "indentWidth",
    editorconfig: "useEditorconfig",
    files: "includes",
    legacyFiles: "include",
    options: "formatter",
    defaultWidth: 80,
    basenameOnly: false,
  },
};

const FILES = [
  ".oxfmtrc.json",
  ".oxfmtrc.jsonc",
  ...["ts", "mts", "cts", "js", "mjs", "cjs"].map((ext) => `oxfmt.config.${ext}`),
  "biome.json",
  "biome.jsonc",
  ".prettierrc",
  ".prettierrc.toml",
  ...["json", "json5", "yaml", "yml", "js", "cjs", "mjs", "ts", "cts", "mts"].map(
    (ext) => `.prettierrc.${ext}`,
  ),
  ...["js", "cjs", "mjs", "ts", "cts", "mts"].map((ext) => `prettier.config.${ext}`),
  "package.json",
];

const parsed = new Map<string, Read>();
const directories = new Map<string, Directory>();
const dependencies = new Map<string, Formatter[]>();
const globs = new Map<string, Bun.Glob>();
const editorGlobs = new Map<string, RegExp>();
const editors = new Map<string, Editor | null>();
const widths = new Map<string, Width>();

function family(file: string): Formatter {
  const name = basename(file);
  return name.startsWith("biome.")
    ? "biome"
    : name.startsWith("oxfmt.") || name.startsWith(".oxfmt")
      ? "oxfmt"
      : "prettier";
}

function scalar(value: string): Value {
  const text = value.trim().replace(/\s+#.*$/, "");
  if (/^\d+$/.test(text)) return Number(text);
  if (text === "true" || text === "false") return text === "true";
  if (/^(["']).*\1$/.test(text)) return text.slice(1, -1);
  return text;
}

function flat(text: string, separator: ":" | "="): Value {
  const value: Record<string, Value> = {};
  for (const line of text.split(/\r?\n/)) {
    if (!line.trim() || /^\s*#/.test(line)) continue;

    const match =
      separator === ":"
        ? /^(?:([\w-]+)|"([\w-]+)"|'([\w-]+)')\s*:\s*(.*)$/.exec(line)
        : /^([\w-]+)\s*=\s*(.*)$/.exec(line);

    const key = separator === ":" ? (match?.[1] ?? match?.[2] ?? match?.[3]) : match?.[1];
    const raw = separator === ":" ? match?.[4] : match?.[2];
    if (!key || key === "overrides" || !raw) return UNKNOWN;

    value[key] = scalar(raw);
  }

  return value;
}

function readable(file: string): Read {
  const cached = parsed.get(file);
  if (cached) return cached;

  let value: Value = UNKNOWN;
  const name = basename(file);
  const formatter = family(file);
  try {
    if (name === ".prettierrc.toml") value = flat(readFileSync(file, "utf8"), "=");
    else if (/\.ya?ml$/.test(name)) value = flat(readFileSync(file, "utf8"), ":");
    else if (name === ".prettierrc") {
      const module = moduleAt(file);
      value = module?.program
        ? exported(file, "default")
        : module
          ? flat(module.text, ":")
          : UNKNOWN;
    } else if (name === "package.json") value = property(exported(file, "default"), "prettier");
    else value = exported(file, "default");

    if (formatter === "biome" && object(value)) value = mergedBiome(file, value, new Set());
  } catch {}

  const result: Read =
    value === undefined || (formatter === "biome" && object(value) && !biomeActive(value))
      ? { kind: "skip" }
      : object(value)
        ? { kind: "config", config: { file, formatter, value } }
        : { kind: "unread" };

  parsed.set(file, result);
  return result;
}

function biomeActive(value: Value): boolean {
  if (!object(value)) return true;

  const formatter = property(value, "formatter");
  if (property(formatter, "enabled") === false) return false;
  if (property(property(property(value, "javascript"), "formatter"), "enabled") === false)
    return false;

  if (formatter !== undefined || property(property(value, "javascript"), "formatter") !== undefined)
    return true;

  const overrides = property(value, "overrides");
  if (overrides === UNKNOWN) return true;
  return (
    Array.isArray(overrides) &&
    overrides.some(
      (entry) =>
        object(entry) &&
        (property(entry, "formatter") !== undefined ||
          property(property(entry, "javascript"), "formatter") !== undefined),
    )
  );
}

function mergedBiome(file: string, value: Value, seen: Set<string>): Value {
  if (!object(value) || seen.has(file)) return UNKNOWN;

  const entries = property(value, "extends");
  if (entries === undefined) return value;

  const list = typeof entries === "string" ? [entries] : entries;
  if (!Array.isArray(list) || !list.every((entry) => typeof entry === "string")) return UNKNOWN;

  seen.add(file);

  let merged: Value = {};
  for (const entry of list as string[]) {
    let target: string | undefined;
    if (entry === "//") {
      let dir = dirname(file);
      while (dirname(dir) !== dir && !target) {
        dir = dirname(dir);

        for (const name of ["biome.json", "biome.jsonc"]) {
          const candidate = join(dir, name);
          if (existsSync(candidate)) {
            target = candidate;
            break;
          }
        }
      }
    } else if (entry.startsWith(".") || isAbsolute(entry) || /\.jsonc?$/.test(entry))
      target = resolve(dirname(file), entry);

    if (!target || !/\.jsonc?$/.test(target) || !existsSync(target)) return UNKNOWN;

    const parent = mergedBiome(target, exported(target, "default"), seen);
    if (!object(parent)) return UNKNOWN;

    merged = mergeBiome(merged, parent);
  }

  seen.delete(file);
  return mergeBiome(merged, value);
}

function mergeBiome(base: Value, next: Value): Value {
  const left = base as Record<string, Value>;
  const right = next as Record<string, Value>;
  const javascript = property(right, "javascript");
  const previous = property(left, "javascript");
  const result = { ...left, ...right };
  if (property(left, "formatter") !== undefined || property(right, "formatter") !== undefined)
    result.formatter = {
      ...(property(left, "formatter") as object),
      ...(property(right, "formatter") as object),
    };

  if (previous !== undefined || javascript !== undefined) {
    result.javascript = { ...(previous as object), ...(javascript as object) };
    if (
      property(previous, "formatter") !== undefined ||
      property(javascript, "formatter") !== undefined
    )
      (result.javascript as Record<string, Value>).formatter = {
        ...(property(previous, "formatter") as object),
        ...(property(javascript, "formatter") as object),
      };
  }

  return result;
}

function directory(dir: string, files: string[]): Directory {
  const cached = directories.get(dir);
  if (cached) return cached;

  const configs: Config[] = [];
  const unread: string[] = [];
  for (const file of files) {
    const read = readable(file);
    if (read.kind === "config") configs.push(read.config);
    if (read.kind === "unread") unread.push(file);
  }

  const found = { configs, unread };
  directories.set(dir, found);
  return found;
}

function dependent(dir: string): Formatter[] {
  const cached = dependencies.get(dir);
  if (cached) return cached;

  let current = dir;
  while (true) {
    try {
      const manifest = JSON.parse(readFileSync(join(current, "package.json"), "utf8")) as Record<
        string,
        Record<string, unknown>
      >;

      const names = { ...manifest.dependencies, ...manifest.devDependencies };
      const found: Formatter[] = [];
      if ("oxfmt" in names) found.push("oxfmt");
      if ("@biomejs/biome" in names) found.push("biome");
      if ("prettier" in names) found.push("prettier");
      if (found.length) {
        dependencies.set(dir, found);
        return found;
      }
    } catch {}

    const parent = dirname(current);
    if (parent === current) break;

    current = parent;
  }

  dependencies.set(dir, []);
  return [];
}

function number(value: Value): number | undefined {
  return typeof value === "number" && Number.isInteger(value) && value > 0 ? value : undefined;
}

function match(pattern: string, file: string, root: string, basenameOnly = true): boolean {
  const path =
    basenameOnly && !pattern.includes("/")
      ? basename(file)
      : relative(root, file).split(sep).join("/");

  let glob = globs.get(pattern);
  if (!glob) {
    glob = new Bun.Glob(pattern);
    globs.set(pattern, glob);
  }

  return glob.match(path);
}

function orderedIncludes(entries: string[], file: string, root: string): boolean {
  let included = false;
  for (const entry of entries) {
    const bangs = /^!+/.exec(entry)?.[0].length ?? 0;
    if (match(entry.slice(bangs), file, root, false)) included = bangs === 0;
  }

  return included;
}

function patterns(value: Value): string[] | null {
  if (typeof value === "string") return [value];
  return Array.isArray(value) && value.every((item) => typeof item === "string")
    ? (value as string[])
    : null;
}

function unknown(value: Value): boolean {
  if (value === UNKNOWN) return true;
  if (Array.isArray(value)) return value.some(unknown);
  return object(value) && Object.values(value).some(unknown);
}

function scoped(config: Config, path: string): Value {
  let value = config.value;
  const overrides = property(value, "overrides");
  if (overrides === UNKNOWN || (overrides !== undefined && !Array.isArray(overrides)))
    return UNKNOWN;

  if (!Array.isArray(overrides)) return value;

  const keys = formats[config.formatter];
  for (const entry of overrides) {
    if (!object(entry)) return UNKNOWN;

    const current = property(entry, keys.files);
    const files = patterns(
      current ?? (keys.legacyFiles ? property(entry, keys.legacyFiles) : undefined),
    );

    const options = property(entry, keys.options);
    const javascript = property(entry, "javascript");
    const excluded = patterns(property(entry, "excludeFiles"));
    if (
      !files ||
      unknown(options) ||
      unknown(javascript) ||
      (property(entry, "excludeFiles") !== undefined && !excluded) ||
      (options !== undefined && !object(options)) ||
      (javascript !== undefined && !object(javascript))
    )
      return UNKNOWN;

    const root = dirname(config.file);
    const legacy = keys.legacyFiles && current === undefined;
    const included =
      keys.options === "formatter" && !legacy
        ? orderedIncludes(files, path, root)
        : files.some((pattern) =>
            match(
              legacy && !pattern.includes("/") ? `**/${pattern}` : pattern,
              path,
              root,
              keys.basenameOnly,
            ),
          );

    if (!included || excluded?.some((pattern) => match(pattern, path, root))) continue;
    if (unknown(property(value, "formatter")) || unknown(property(value, "javascript")))
      return UNKNOWN;

    value =
      keys.options === "formatter"
        ? {
            ...(value as object),
            formatter: {
              ...(property(value, "formatter") as object),
              ...(options as object),
            },
            javascript: {
              ...(property(value, "javascript") as object),
              ...(javascript as object),
              formatter: {
                ...(property(property(value, "javascript"), "formatter") as object),
                ...(property(javascript, "formatter") as object),
              },
            },
          }
        : { ...(value as object), ...(options as object) };
  }

  return value;
}

function setting(config: Config, path: string): Setting | "skip" | null {
  const value = scoped(config, path);
  if (value === UNKNOWN) return null;
  if (config.formatter === "biome" && !biomeActive(value)) return "skip";

  const keys = formats[config.formatter];
  const format = keys.options === "formatter" ? property(value, "formatter") : value;
  const javascript = property(property(value, "javascript"), "formatter");
  const width = property(javascript, keys.width);
  const baseWidth = property(format, keys.width);

  const tab = property(javascript, keys.tab);
  const baseTab = property(format, keys.tab);
  const editorconfig = keys.editorconfig ? property(format, keys.editorconfig) : undefined;
  if ([width, baseWidth, tab, baseTab, editorconfig].includes(UNKNOWN)) return null;

  if (config.formatter === "biome") {
    const files = property(value, "files");
    if (files === UNKNOWN) return null;

    const includes = property(files, "includes");
    const include = property(files, "include");
    const ignore = property(files, "ignore");
    if ([includes, include, ignore].includes(UNKNOWN)) return null;

    const root = dirname(config.file);
    const v2 = includes === undefined ? undefined : patterns(includes);
    const v1 = include === undefined ? undefined : patterns(include);
    const ignored = ignore === undefined ? undefined : patterns(ignore);
    if (
      (includes !== undefined && !v2) ||
      (include !== undefined && !v1) ||
      (ignore !== undefined && !ignored)
    )
      return null;

    if (v2 && !orderedIncludes(v2, path, root)) return "skip";
    if (
      v1 &&
      !v1.some((pattern) =>
        match(!pattern.includes("/") ? `**/${pattern}` : pattern, path, root, false),
      )
    )
      return "skip";

    if (
      ignored?.some((pattern) =>
        match(!pattern.includes("/") ? `**/${pattern}` : pattern, path, root, false),
      )
    )
      return "skip";

    const formatterIncludes = property(format, "includes");
    if (formatterIncludes === UNKNOWN) return null;

    const scopedFiles = formatterIncludes === undefined ? undefined : patterns(formatterIncludes);
    if (formatterIncludes !== undefined && !scopedFiles) return null;
    if (scopedFiles && !orderedIncludes(scopedFiles, path, root)) return "skip";
  }

  const dimensions = {
    columns: number(width) ?? number(baseWidth),
    tab: number(tab) ?? number(baseTab),
  };

  return {
    ...dimensions,
    editorconfig: !keys.editorconfig || editorconfig === true,
  };
}

function parsedEditor(file: string): Editor | null {
  if (editors.has(file)) return editors.get(file)!;

  try {
    const sections: Editor["sections"] = [{ pattern: null, values: {} }];
    for (const line of readFileSync(file, "utf8").split(/\r?\n/)) {
      const section = /^\s*\[(.+)\]\s*$/.exec(line);
      if (section) {
        sections.push({ pattern: section[1]!, values: {} });
        continue;
      }

      const pair = /^\s*([\w_]+)\s*=\s*([^#;]*)/.exec(line);
      if (pair) sections.at(-1)!.values[pair[1]!.toLowerCase()] = pair[2]!.trim().toLowerCase();
    }

    const result = { root: sections[0]!.values.root === "true", sections };
    editors.set(file, result);
    return result;
  } catch {
    editors.set(file, null);
    return null;
  }
}

function editorPattern(pattern: string): RegExp {
  const cached = editorGlobs.get(pattern);
  if (cached) return cached;

  const source = pattern.replace(/^\//, "");
  const compile = (part: string): string => {
    let result = "";
    for (let index = 0; index < part.length; index++) {
      const char = part[index]!;
      if (char === "*")
        if (part[index + 1] === "*") {
          result += part[index + 2] === "/" ? "(?:.*/)?" : ".*";
          index += part[index + 2] === "/" ? 2 : 1;
        } else result += "[^/]*";
      else if (char === "?") result += "[^/]";
      else if (char === "[") {
        const end = part.indexOf("]", index + 1);
        if (end < 0) result += "\\[";
        else {
          const content = part.slice(index + 1, end);
          result += `[${content.startsWith("!") ? `^${content.slice(1)}` : content}]`;
          index = end;
        }
      } else if (char === "{") {
        const end = part.indexOf("}", index + 1);
        if (end < 0) result += "\\{";
        else {
          const content = part.slice(index + 1, end);
          const range = /^(-?\d+)\.\.(-?\d+)$/.exec(content);
          const choices = range
            ? Array.from({ length: Math.abs(Number(range[2]) - Number(range[1])) + 1 }, (_, step) =>
                String(Number(range[1]) + step * Math.sign(Number(range[2]) - Number(range[1]))),
              )
            : content.split(",");

          result += `(?:${choices.map(compile).join("|")})`;
          index = end;
        }
      } else result += /[\\^$+?.()|{}]/.test(char) ? `\\${char}` : char;
    }

    return result;
  };

  const regex = new RegExp(`^${compile(source)}$`);
  editorGlobs.set(pattern, regex);
  return regex;
}

function editorMatch(pattern: string, file: string, root: string): boolean {
  const target = pattern.replace(/^\//, "").includes("/")
    ? relative(root, file).split(sep).join("/")
    : basename(file);

  return editorPattern(pattern).test(target);
}

function editorconfig(
  path: string,
  unread: string[],
  biomeDir?: string,
): { columns?: number; tab?: number; file?: string } {
  const values: { columns?: number; tab?: number; file?: string } = {};

  let sawWidth = false;
  let sawTab = false;
  let dir = biomeDir ?? dirname(path);
  while (true) {
    const file = join(dir, ".editorconfig");

    let root = false;
    if (existsSync(file)) {
      const editor = parsedEditor(file);
      if (!editor) unread.push(file);
      else {
        root = editor.root;
        const local: Record<string, string> = {};
        for (const section of editor.sections)
          if (section.pattern === null || editorMatch(section.pattern, path, dir))
            Object.assign(local, section.values);

        if (!sawWidth && local.max_line_length !== undefined) {
          sawWidth = true;
          values.columns = number(Number(local.max_line_length));
          if (values.columns !== undefined) values.file = file;
        }

        if (!sawTab && (local.tab_width !== undefined || local.indent_size !== undefined)) {
          sawTab = true;
          values.tab = number(Number(local.tab_width)) ?? number(Number(local.indent_size));
        }
      }
    }

    if (biomeDir || root || existsSync(join(dir, ".git")) || existsSync(join(dir, ".hg"))) break;

    const parent = dirname(dir);
    if (parent === dir) break;

    dir = parent;
  }

  return values;
}

export function formatterWidth(path: string): Width {
  const dir = realDirectory(dirname(resolve(path)));
  const file = join(dir, basename(path));
  const cached = widths.get(file);
  if (cached) return cached;

  const unread: string[] = [];
  const dependency = dependent(dir);

  let choices: { config: Config; setting: Setting }[] = [];
  let current = dir;
  for (const files of configDirectories(dir, { family: "width", files: FILES })) {
    const found = directory(current, files);
    unread.push(...found.unread);
    const valid: typeof choices = [];
    for (const config of found.configs) {
      const scoped = setting(config, file);
      if (scoped === null) unread.push(config.file);
      else if (scoped !== "skip") valid.push({ config, setting: scoped });
    }

    if (valid.length) {
      choices = valid;
      break;
    }

    const parent = dirname(current);
    if (parent === current) break;

    current = parent;
  }

  const resolved = (setting: Setting, formatter: Formatter, config: Config) =>
    setting.columns ??
    (setting.editorconfig
      ? editorconfig(file, [], formatter === "biome" ? dirname(config.file) : undefined).columns
      : undefined) ??
    formats[formatter].defaultWidth;

  const listed = choices.filter((choice) => dependency.includes(choice.config.formatter));
  const chosen = (listed.length === 1 ? listed : choices).toSorted(
    (left, right) =>
      resolved(left.setting, left.config.formatter, left.config) -
      resolved(right.setting, right.config.formatter, right.config),
  )[0];

  const formatter =
    chosen?.config.formatter ??
    dependency.toSorted(
      (left, right) => formats[left].defaultWidth - formats[right].defaultWidth,
    )[0];

  const own = chosen?.setting;
  const reads = formatter !== undefined && (own?.editorconfig ?? formatter !== "biome");
  const chosenEditor = reads
    ? editorconfig(
        file,
        unread,
        chosen?.config.formatter === "biome" ? dirname(chosen.config.file) : undefined,
      )
    : {};

  const columns =
    own?.columns ??
    (reads ? chosenEditor.columns : undefined) ??
    (formatter ? formats[formatter].defaultWidth : 80);

  const tab = own?.tab ?? (reads ? chosenEditor.tab : undefined) ?? 2;
  const source =
    own?.columns !== undefined
      ? { kind: "config" as const, file: chosen!.config.file }
      : reads && chosenEditor.columns !== undefined
        ? { kind: "config" as const, file: chosenEditor.file! }
        : formatter
          ? { kind: "default" as const, formatter }
          : { kind: "fallback" as const };

  const result = { columns, tab, source, unread };
  widths.set(file, result);
  return result;
}
