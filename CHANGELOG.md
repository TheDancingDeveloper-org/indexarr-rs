# Changelog

## Unreleased

### Fixed

- Windows installer: the service runs sync-only by default. It no longer starts
  the DHT crawler, resolver, or announcer on the machine's public address
  (`--all` dropped from the service command line; the worker set comes from
  `INDEXARR_WORKERS` in `.env`, defaulting to `http_server,sync`). An existing
  `INDEXARR_WORKERS` line is kept on upgrade, so an opted-in crawl setting
  survives reinstalls. Originally PR #14.
