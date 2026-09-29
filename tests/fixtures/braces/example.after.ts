function retry(task: Task, attempts: number) {
  if (attempts === 0)
    throw new Error(`${task.name} failed`);

  while (!task.done)
    task.run();

  for (const listener of task.listeners) {
    listener.notify(task);
    listener.close();
  }
}
