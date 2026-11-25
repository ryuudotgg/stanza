import { existsSync, mkdirSync, readdirSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { join } from "node:path";

function shellQuoted(text: string): string {
  return `'${text.replaceAll("'", `'\\''`)}'`;
}

function run(argv: string[], cwd?: string): Buffer {
  const result = Bun.spawnSync(argv, { cwd, stderr: "pipe" });
  if (!result.success) throw new Error(`${argv.join(" ")} failed: ${result.stderr.toString()}`);
  return result.stdout;
}

export interface GitCall {
  args: string[];
  stdin?: Uint8Array;
}

export interface ShimOptions {
  stdin?: boolean;
  meet?: { commands: string[]; count: number };
  hideAttributeMetadata?: boolean;
  refuse?: string;
}

function scanning(refuse: string | undefined): string[] {
  const refused = refuse
    ? [`      ${shellQuoted(refuse)}|${shellQuoted(refuse)}=*) refused=$1;;`]
    : [];

  return [
    "scan() {",
    '  while [ "$#" -gt 0 ]; do',
    '    case "$1" in',
    "      --exec-path|--version|-v|--help|-h|--html-path|--man-path|--info-path) return;;",
    ...refused,
    '      -C|-c|--git-dir|--work-tree|--namespace|--config-env|--attr-source|--shallow-file|--super-prefix) [ "$#" -gt 1 ] && shift;;',
    "      -*) ;;",
    "      *) command=$1; shift; following=${1-}; return;;",
    "    esac",
    "    shift",
    "  done",
    "}",
    "command=",
    "following=",
    "refused=",
    'scan "$@"',
  ];
}

// A shell may split one printf into several writes, so each process appends to its own file, named by its PID, and no two live calls share a writer. The order line is one short append, written once the record is whole.
function recording(trace: string, stdin: boolean): string[] {
  const file = `${shellQuoted(trace)}/$$`;
  const order = `printf '%s\\n' "$$" >> ${shellQuoted(trace)}/order || exit 125`;
  if (!stdin) return [`printf '%s\\000' "$#" "$@" - >> ${file} || exit 125`, order];

  return [
    "input=$(mktemp) || exit 125",
    `trap 'rm -f "$input"' EXIT`,
    `cat > "$input" || { echo 'git shim: could not capture stdin' >&2; exit 125; }`,
    `size=$(wc -c < "$input" | tr -d ' ')`,
    `{ printf '%s\\000' "$#" "$@" "$size" && cat "$input"; } >> ${file} || exit 125`,
    order,
    'exec < "$input"',
    'rm "$input"',
    "trap - EXIT",
  ];
}

const refusing = [
  'if [ -n "$refused" ]; then',
  `  printf 'unknown option: %s\\n' "$refused" >&2`,
  "  exit 129",
  "fi",
];

// A round's outcome is one symlink, and ln fails when it exists, so the last arrival and a timeout race to publish it and every caller of the round reads the same one.
function meeting(room: string, commands: string[], count: number): string[] {
  const quoted = shellQuoted(room);
  return [
    `case "$command" in ${commands.map(shellQuoted).join("|")})`,
    "  slot=1",
    "  while :; do",
    `    round=$(((slot + ${count - 1}) / ${count}))`,
    `    if [ -L ${quoted}/round-$round ]; then slot=$((round * ${count} + 1)); continue; fi`,
    `    mkdir ${quoted}/$slot 2>/dev/null && break`,
    `    [ -d ${quoted}/$slot ] || { echo 'git shim: could not join the meeting' >&2; exit 125; }`,
    "    slot=$((slot + 1))",
    "  done",
    `  [ "$slot" -eq $((round * ${count})) ] && ln -s met ${quoted}/round-$round 2>/dev/null`,
    "  waited=0",
    `  until [ -L ${quoted}/round-$round ]; do`,
    `    [ "$waited" -ge 40 ] && { ln -s failed ${quoted}/round-$round 2>/dev/null; break; }`,
    "    sleep 0.05",
    "    waited=$((waited + 1))",
    "  done",
    `  [ "$(readlink ${quoted}/round-$round)" = met ] || { echo 'git shim: no other call met this one' >&2; exit 125; };;`,
    "esac",
  ];
}

const withoutAttributeMetadata = [
  'if [ "$command $following" = "var -l" ]; then',
  "  output=$(mktemp) || exit 125",
  '  "$git" "$@" > "$output"',
  "  status=$?",
  `  sed '/^GIT_ATTR_/d' "$output" || { [ "$status" -eq 0 ] && status=125; }`,
  '  rm "$output"',
  '  exit "$status"',
  "fi",
];

export function traceShim(dir: string, git: string, options: ShimOptions = {}): string {
  if (options.meet) {
    const { commands, count } = options.meet;
    if (commands.length === 0) throw new Error("a meeting needs a command to meet on");
    if (!Number.isSafeInteger(count) || count < 2 || count > 64)
      throw new Error(`a meeting needs two to 64 calls, not ${count}`);
  }

  const trace = join(dir, "calls");
  mkdirSync(trace);

  const lines = [
    "#!/bin/sh",
    `git=${shellQuoted(git)}`,
    ...scanning(options.refuse),
    ...recording(trace, options.stdin ?? false),
    ...refusing,
  ];

  if (options.meet) {
    const room = join(dir, "room");
    mkdirSync(room);
    lines.push(...meeting(room, options.meet.commands, options.meet.count));
  }

  if (options.hideAttributeMetadata) lines.push(...withoutAttributeMetadata);

  writeFileSync(join(dir, "git"), `${[...lines, 'exec "$git" "$@"'].join("\n")}\n`, {
    mode: 0o755,
  });

  return trace;
}

function records(path: string): GitCall[] {
  const bytes = readFileSync(path);
  const calls: GitCall[] = [];
  let at = 0;

  const torn = () => new Error(`${path}: torn git record at byte ${at}`);
  if (bytes.length === 0) throw torn();

  const field = () => {
    const end = bytes.indexOf(0, at);
    if (end < 0) throw torn();

    const text = bytes.toString("utf8", at, end);
    at = end + 1;
    return text;
  };

  while (at < bytes.length) {
    const count = field();
    if (!/^\d+$/.test(count) || Number(count) > bytes.length - at) throw torn();

    const args = Array.from({ length: Number(count) }, field);
    const size = field();
    if (size === "-") {
      calls.push({ args });
      continue;
    }

    if (!/^\d+$/.test(size) || at + Number(size) > bytes.length) throw torn();

    calls.push({ args, stdin: new Uint8Array(bytes.subarray(at, at + Number(size))) });
    at += Number(size);
  }

  return calls;
}

export function takeTrace(trace: string): GitCall[] {
  if (!existsSync(trace)) return [];

  const order = join(trace, "order");
  const names = readdirSync(trace).filter((name) => name !== "order");
  const pending = new Map(names.map((name) => [name, records(join(trace, name))]));
  const started = existsSync(order) ? readFileSync(order, "utf8").split("\n").filter(Boolean) : [];

  const calls = started.map((pid) => {
    const call = pending.get(pid)?.shift();
    if (!call) throw new Error(`${order}: no git record left for process ${pid}`);
    return call;
  });

  for (const [name, left] of pending)
    if (left.length > 0)
      throw new Error(`${join(trace, name)}: torn git record with no order line`);

  for (const name of [...names, "order"]) rmSync(join(trace, name), { force: true });
  return calls;
}

export function shownCall(args: string[]): string {
  const shown = args.map((arg) => (/^[^\s"\\]+$/.test(arg) ? arg : JSON.stringify(arg)));
  return ["git", ...shown].join(" ");
}

export function exportHead(repository: string, into: string): string[] {
  const archive = `${into}.tar`;

  run(["git", "-C", repository, "archive", "--output", archive, "HEAD"]);
  run(["tar", "-xf", archive, "-C", into]);

  const files = run(["tar", "-tf", archive]).toString().split("\n");
  rmSync(archive);
  return files;
}

export function commitTree(dir: string): void {
  if (!existsSync(join(dir, ".git"))) run(["git", "init", "-q"], dir);

  const identity = [
    "-c",
    "user.name=scratch",
    "-c",
    "user.email=scratch@stanza.invalid",
    "-c",
    "commit.gpgsign=false",
  ];

  run(["git", "add", "-A"], dir);
  run(["git", ...identity, "commit", "-q", "--no-verify", "--allow-empty", "-m", "scratch"], dir);
}
