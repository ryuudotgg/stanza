import { source } from "../lib/source";
import { sidebarUrls } from "./sidebar";

const pages = source.getPages();
const texts = Object.fromEntries(
  await Promise.all(
    pages.map(async (page) => [
      page.url,
      {
        path: page.path,
        raw: await page.data.getText("raw"),
        processed: await page.data.getText("processed"),
      },
    ]),
  ),
);

console.log(
  JSON.stringify({ pages: pages.map((page) => page.url), sidebar: sidebarUrls(), texts }),
);
