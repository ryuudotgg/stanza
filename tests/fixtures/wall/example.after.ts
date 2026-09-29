function bootstrap(options: Options) {
  const config = loadConfig(options.configPath);
  const logger = createLogger(config.logLevel);
  const db = connect(config.databaseUrl);
  const cache = createCache(config.cacheSize);
  const server = createServer(config.port, db, cache);
  server.start();
  logger.info(`listening on ${config.port}`);
}
