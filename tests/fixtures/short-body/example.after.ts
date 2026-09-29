function record(event: Event, log: Event[]) {
  if (event.silent) return;
  log.push(event);
  flush(log);
}
