import addon from "../../node_modules/@oxc-parser/binding-linux-arm64-musl/parser.linux-arm64-musl.node" with { type: "file" };
import { start } from "../compiled.ts";
await start(addon);
