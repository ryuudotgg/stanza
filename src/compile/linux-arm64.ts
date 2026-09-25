import addon from "../../node_modules/@oxc-parser/binding-linux-arm64-gnu/parser.linux-arm64-gnu.node" with { type: "file" };
import { start } from "../compiled.ts";
await start(addon);
