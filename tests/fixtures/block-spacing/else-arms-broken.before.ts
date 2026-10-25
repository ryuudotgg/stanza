function serve(response: Response, ready: boolean, method: string, body: string) {
  response.writeHead(404);
  if (ready && method !== "HEAD") response.write(body);
  else
    response.end(`no body for ${method} because the response is not ready yet, try again later on`);
}
