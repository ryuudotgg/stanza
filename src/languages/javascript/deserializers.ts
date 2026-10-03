import { dirname, join } from "node:path";

export interface DeserializerFiles {
  js: string;
  ts: string;
}

let embedded: DeserializerFiles | undefined;
export function embedDeserializers(files: DeserializerFiles): void {
  embedded = files;
}

export function deserializerFiles(): DeserializerFiles {
  if (embedded) return embedded;
  const generated = join(dirname(require.resolve("oxc-parser")), "generated", "deserialize");
  return { js: join(generated, "js.js"), ts: join(generated, "ts.js") };
}
