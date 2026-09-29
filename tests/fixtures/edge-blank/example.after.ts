function close(connection: Connection) {
  connection.end();
  log("closed");
}
