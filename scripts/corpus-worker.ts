import { tooDeep } from "../src/depth.ts";
import { serveChunks } from "../src/pool.ts";
import { judgeFile, type CorpusJob } from "./corpus.ts";

serveChunks((paths: string[], job: CorpusJob) => ({
  outcomes: paths.map((path) => judgeFile(path, job, tooDeep)),
}));
