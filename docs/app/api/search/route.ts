import { source } from "@/lib/source";
import { createFromSource } from "fumadocs-core/search/server";

export const { staticGET: GET } = createFromSource(source, { sort: { enabled: false } });

export const revalidate = false;
