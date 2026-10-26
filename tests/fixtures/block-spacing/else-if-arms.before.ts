function serve(response: Response, ready: boolean, method: string, body: string) {
  response.writeHead(404);
  if (ready && method !== "HEAD") response.write(body);
  else if (ready) response.write("");
  else response.end();
}
