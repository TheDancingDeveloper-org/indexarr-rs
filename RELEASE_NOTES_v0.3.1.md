# Indexarr v0.3.1

## Summary

Indexarr 0.3.1 makes peer synchronization work out of the box for new binary
and container installs. Sync and XMPP discovery are enabled by default, the
public bootstrap service can report a peer's observed address, and Indexarr
can advertise that address to the discovery MUC without requiring a manually
configured URL.

## Automatic peer address discovery

- New installs seed peer exchange from `https://bootstrap.indexarr.net`.
- When `INDEXARR_SYNC_EXTERNAL_URL` is empty, Indexarr asks a configured
  bootstrap peer which public IP made the request and builds an advertised URL
  from that address.
- Forwarded client addresses from Cloudflare and reverse proxies are handled,
  with the original `X-Forwarded-For` client preferred over a Docker gateway
  address in `X-Real-IP`.
- Discovered addresses are accepted only when they use HTTP or HTTPS and refer
  to a public IP or non-local hostname.
- An explicit `INDEXARR_SYNC_EXTERNAL_URL` remains authoritative for domains,
  HTTPS reverse proxies, and nonstandard paths.

## Dedicated sync listener

- `INDEXARR_SYNC_API_PORT` can start a second HTTP listener containing only
  health and synchronization routes.
- `INDEXARR_SYNC_EXTERNAL_PORT` overrides the advertised port; otherwise the
  dedicated sync port is preferred, followed by the main HTTP port.
- This supports VPN and NAT setups where a forwarded public port should expose
  sync endpoints without exposing the full web application.

## Discovery defaults

- The `sync` worker, synchronization, and XMPP discovery are enabled by
  default.
- XMPP discovery defaults to the public
  `conference.indexarr.net:5222` endpoint and the
  `indexarr-sync@conference.indexarr.net` MUC.
- Docker images and both Compose examples carry the same bootstrap, sync, and
  XMPP defaults as direct binary installs.
- Invalid or unreachable automatically discovered URLs are not advertised to
  peers.

## Networking note

Automatic discovery observes a public egress address but cannot create an
inbound router, firewall, NAT, or VPN port-forward. Operators must still make
the effective sync port reachable. See `SYNC_NETWORKING.md` for configuration
and examples.

## Validation

- Address parsing, public-IP filtering, URL validation, forwarded-header
  preference, and external-port selection have regression coverage.
- The release passes formatting, all-target checks, strict Clippy, workspace
  and documentation tests, the UI build, release builds, and runtime container
  smoke tests in the release pipeline.

## Upgrade notes

- Existing explicit sync and XMPP settings continue to override the new
  defaults.
- Operators that intentionally run a standalone node can set
  `INDEXARR_SYNC_ENABLED=false` and `INDEXARR_XMPP_ENABLED=false`.
- Existing PostgreSQL data and container mounts remain compatible.

## Downloads

- Linux x86_64: `indexarr-v0.3.1-linux-x86_64.tar.gz`
- Linux aarch64: `indexarr-v0.3.1-linux-aarch64.tar.gz`
- Linux installer: `indexarr-v0.3.1-linux-install.sh`
- Windows x86_64 binary: `indexarr-v0.3.1-windows-x86_64.exe`
- Windows x86_64 installer: `indexarr-v0.3.1-windows-x86_64-setup.exe`
- Checksums: `SHA256SUMS-v0.3.1.txt`
- Docker: `ghcr.io/ausagentsmith-org/indexarr-rs:v0.3.1`

All downloadable files are attached to the public GitHub release.
