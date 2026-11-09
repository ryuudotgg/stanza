import { Buffer } from "node:buffer";

const aborting = process.argv[2];
const reader = Bun.stdin.stream().getReader();
let pending: Uint8Array = new Uint8Array(0);
let written: Uint8Array[] = [];

async function take(size: number): Promise<Uint8Array> {
  while (pending.length < size) {
    const { value, done } = await reader.read();
    if (done) process.exit(0);
    pending = Buffer.concat([pending, value]);
  }

  const head = pending.subarray(0, size);
  pending = pending.subarray(size);
  return head;
}

async function packets(): Promise<Uint8Array[]> {
  const list = [];
  for (;;) {
    const size = Number.parseInt(new TextDecoder().decode(await take(4)), 16);
    if (size === 0) return list;
    list.push(await take(size - 4));
  }
}

function packet(data: string | Uint8Array): void {
  const bytes = typeof data === "string" ? Buffer.from(data) : data;
  written.push(Buffer.from((bytes.length + 4).toString(16).padStart(4, "0")), bytes);
}

function flush(): void {
  written.push(Buffer.from("0000"));
  process.stdout.write(Buffer.concat(written));
  written = [];
}

function rot13(bytes: Uint8Array): Uint8Array {
  return bytes.map((byte) => {
    const base = byte >= 97 && byte <= 122 ? 97 : byte >= 65 && byte <= 90 ? 65 : 0;
    return base === 0 ? byte : ((byte - base + 13) % 26) + base;
  });
}

await packets();
packet("git-filter-server\n");
packet("version=2\n");
flush();

await packets();
packet("capability=clean\n");
packet("capability=smudge\n");
flush();

for (;;) {
  const header = new Map(
    (await packets()).map((line) => {
      const text = new TextDecoder().decode(line).replace(/\n$/, "");
      const split = text.indexOf("=");
      return [text.slice(0, split), text.slice(split + 1)];
    }),
  );

  const content = Buffer.concat(await packets());

  if (header.get("command") === "smudge" && header.get("pathname") === aborting) {
    packet("status=abort\n");
    flush();
    continue;
  }

  packet("status=success\n");
  flush();

  const result = rot13(content);
  for (let start = 0; start < result.length; start += 65516)
    packet(result.subarray(start, start + 65516));

  flush();
  flush();
}
