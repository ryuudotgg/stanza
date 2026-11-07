import type { Finding, Mode } from "../src/engine/types.ts";

export interface BatchRequest {
  path: string;
  text: string;
  mode: Mode;
  braces: "on" | null;
  changedLines: { lines: number[]; deletedAfter: number[] } | null;
  explain: number[];
}

export interface BatchResponse {
  id: number;
  text: string | null;
  findings: Finding[];
  explanations: Record<string, string>;
  error: string | null;
}

export interface BatchClient {
  ask(request: BatchRequest): Promise<BatchResponse>;
  close(): Promise<string[]>;
}

export function batchClient(executable: string): BatchClient {
  const child = Bun.spawn([executable, "batch"], {
    stdin: "pipe",
    stdout: "pipe",
    stderr: "inherit",
  });

  const pending = new Map<number, (response: BatchResponse) => void>();
  let failure: Error | undefined;
  let next = 0;

  const fail = (error: unknown) => {
    failure ??= error instanceof Error ? error : new Error(String(error));

    for (const [id, settle] of pending) {
      pending.delete(id);
      settle({
        id,
        text: null,
        findings: [],
        explanations: {},
        error: failure.message,
      });
    }
  };

  const reading = (async () => {
    const decoder = new TextDecoder();

    let buffered = "";
    for await (const chunk of child.stdout) {
      buffered += decoder.decode(chunk, { stream: true });

      let newline: number;
      while ((newline = buffered.indexOf("\n")) !== -1) {
        const response = JSON.parse(buffered.slice(0, newline)) as BatchResponse;
        buffered = buffered.slice(newline + 1);

        const settle = pending.get(response.id);
        if (!settle) throw new Error(`${executable} batch answered unknown id ${response.id}`);

        pending.delete(response.id);
        settle(response);
      }
    }

    if (pending.size > 0) fail(new Error(`${executable} batch stopped answering`));
  })().catch(fail);

  return {
    ask(request) {
      const id = next++;
      const failed = (error: string) =>
        Promise.resolve({ id, text: null, findings: [], explanations: {}, error });

      if (failure) return failed(failure.message);
      if (!request.path.isWellFormed() || !request.text.isWellFormed())
        return failed(
          `${request.path}: the request holds an unpaired surrogate, which JSON cannot carry to the binary`,
        );

      const answered = new Promise<BatchResponse>((settle) => pending.set(id, settle));

      try {
        child.stdin.write(`${JSON.stringify({ id, ...request })}\n`);
        Promise.resolve(child.stdin.flush()).catch(fail);
      } catch (error: unknown) {
        fail(error);
      }

      return answered;
    },
    async close() {
      try {
        await child.stdin.end();
      } catch (error: unknown) {
        fail(error);
      }

      await reading;

      const code = await child.exited;
      const exited = code === 0 ? [] : [`${executable} batch exited with code ${code}`];
      return [...(failure ? [failure.message] : []), ...exited];
    },
  };
}
