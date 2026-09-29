import { source } from "../lib/source";
import { sidebarUrls } from "./sidebar";

console.log(
  JSON.stringify({
    pages: source.getPages().map((page) => page.url),
    sidebar: sidebarUrls(),
  }),
);
