import { includeIgnoreFile } from "@eslint/compat";
import tseslint from "typescript-eslint";
export default tseslint.config(includeIgnoreFile("/x/.gitignore"), { rules: { curly: "off" } });