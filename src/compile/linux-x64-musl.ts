import addon from "../../node_modules/@oxc-parser/binding-linux-x64-musl/parser.linux-x64-musl.node" with { type: "file" };
import { start } from "../compiled.ts";
await start(addon);
