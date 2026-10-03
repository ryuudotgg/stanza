import js from "oxc-parser/src-js/generated/deserialize/js.js" with { type: "file" };
import ts from "oxc-parser/src-js/generated/deserialize/ts.js" with { type: "file" };
import { embedDeserializers } from "./deserializers.ts";

export function embedCompiledDeserializers(): void {
  embedDeserializers({ js, ts });
}
