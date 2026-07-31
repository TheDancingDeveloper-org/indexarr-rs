# Indexarr

Indexarr is a decentralized torrent indexer written in Rust. It discovers
torrent info-hashes from the BitTorrent DHT, resolves metadata, classifies
content, collects tracker statistics, and exchanges signed index data with
other contributors over a gossip-based peer-to-peer network.

The project includes a Vue 3 web interface and a Torznab-compatible API for
indexer clients such as Prowlarr and Sonarr/Radarr.

## Highlights

- BEP 5 DHT crawling with BEP 51 `sample_infohashes` discovery
- BEP 9 metadata resolution and optional tracker announcing
- Name parsing, content classification, quality scoring, and PostgreSQL full-text search
- Ed25519 contributor identities and recovery keys
- Delta export/merge, peer exchange, reputation, and XMPP/MUC discovery
- REST, health, statistics, upload, and administrative endpoints
- Torznab search and capability endpoints
- Docker images and Linux/Windows installers

Indexarr is licensed under the [GNU Affero General Public License v3.0](LICENSE).

## Quick start with Docker

The all-workers Compose file starts PostgreSQL and Indexarr together:

```bash
docker compose up -d
docker compose logs -f indexarr
```

Open the web interface at <http://localhost:8080>. The health endpoint is
available at <http://localhost:8080/health>.

The first run creates a contributor identity in the `indexarr-data` volume.
Save the recovery key printed in the logs if you need to move the identity to
another installation. The default Compose configuration enables synchronization
and XMPP discovery and seeds the public bootstrap peer. Automatic address
discovery cannot create a firewall, NAT, or VPN port-forward; see
[`SYNC_NETWORKING.md`](SYNC_NETWORKING.md) when running behind one.

For a node that provides the web UI and synchronization but does not crawl the
DHT, use the sync-only file:

```bash
docker compose -f docker-compose.sync.yml up -d
```

To build the image from the current checkout, use the sync-only file (it has a
local `build` context), or build directly:

```bash
docker build -t indexarr:local .
```

## Local development

### Prerequisites

- Rust 1.97 or newer (the repository pins the toolchain in `rust-toolchain.toml`)
- PostgreSQL 17
- Node.js 22 and npm for the Vue frontend

PostgreSQL is currently required by the database layer; the default local
connection string is `postgres://indexarr:indexarr@localhost:5432/indexarr`.

Build and test the Rust workspace:

```bash
cargo build
cargo test --workspace
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
```

Run the backend against a local PostgreSQL instance:

```bash
export INDEXARR_DB_URL=postgres://indexarr:indexarr@localhost:5432/indexarr
cargo run -- --workers http_server
```

Run all workers with `cargo run -- --all`, or select a comma-separated list
with `--workers`, for example:

```bash
cargo run -- --workers http_server,sync
```

Build and type-check the frontend:

```bash
cd ui
npm ci
npm run type-check
npm run build
```

During UI development, `npm run dev` starts Vite's development server.

## Workers

Workers are selected with `--all`, `--workers`, or the `INDEXARR_WORKERS`
environment variable.

| Worker | Responsibility |
| --- | --- |
| `http_server` | Axum HTTP server, REST API, Torznab API, and Vue SPA |
| `dht_crawler` | Discovers real info-hashes through BEP 51 DHT sampling |
| `resolver` | Fetches torrent metadata using the BitTorrent metadata protocol |
| `announcer` | Scrapes trackers for seed and peer counts |
| `sync` | Exports, discovers, and gossips index deltas with other peers |
| `peer_refresher` | Refreshes peer counts for stale or trackerless torrents |
| `bep51_sampler` | Backwards-compatible alias for the DHT crawler |

`sync` starts the XMPP channel when `INDEXARR_XMPP_ENABLED=true`.

## Configuration

All settings use the `INDEXARR_` prefix. The complete list, defaults, and type
conversions are defined in
[`crates/indexarr-core/src/config.rs`](crates/indexarr-core/src/config.rs).

Common settings include:

| Variable | Default | Description |
| --- | --- | --- |
| `INDEXARR_DB_BACKEND` | `postgresql` | Database backend setting |
| `INDEXARR_DB_URL` | `postgres://indexarr:indexarr@localhost:5432/indexarr` | PostgreSQL connection URL |
| `INDEXARR_DATA_DIR` | `data` | Identity and log storage |
| `INDEXARR_HOST` | `0.0.0.0` | HTTP bind address |
| `INDEXARR_PORT` | `8080` | Main HTTP port |
| `INDEXARR_WORKERS` | `http_server,dht_crawler,resolver,announcer,sync` | Workers started by default |
| `INDEXARR_DHT_INSTANCES` | `4` | Number of DHT instances |
| `INDEXARR_DHT_BASE_PORT` | `6881` | First DHT port; additional ports are allocated from it |
| `INDEXARR_RESOLVE_WORKERS` | `20` | Concurrent metadata resolvers |
| `INDEXARR_ANNOUNCER_ENABLED` | `true` | Enable tracker announcing |
| `INDEXARR_SYNC_ENABLED` | `true` | Enable P2P synchronization |
| `INDEXARR_SYNC_PEERS` | `["https://bootstrap.indexarr.net"]` | Bootstrap and sync peers (JSON list) |
| `INDEXARR_SYNC_EXTERNAL_URL` | empty | Public URL advertised to peers |
| `INDEXARR_XMPP_ENABLED` | `true` | Enable XMPP/MUC peer discovery |
| `INDEXARR_XMPP_SERVER` | `conference.indexarr.net:5222` | XMPP server address |
| `INDEXARR_XMPP_MUC_ROOM` | `indexarr-sync@conference.indexarr.net` | Discovery room |
| `INDEXARR_TORZNAB_API_KEY` | empty | Protect Torznab requests when set |
| `INDEXARR_TMDB_API_KEY` | empty | Optional TMDB metadata enrichment key |

CLI flags such as `--host`, `--port`, `--db-backend`, and `--db-url` override
the corresponding environment settings. Indexarr loads `.env` from the
working directory and, for installed binaries, beside the executable.

## HTTP and Torznab APIs

The main listener exposes:

- `GET /health` — readiness and health status
- `/api/v1/*` — search, torrent details, upload/import, identity, sync,
  statistics, queue, and system endpoints
- `/api/torznab` and `/api/torznab/api` — Torznab capabilities and search

The optional `INDEXARR_SYNC_API_PORT` listener exposes only health and sync
routes, which is useful when the public sync port should not expose the full
administrative UI. The generated OpenAPI document is available at
[`website/docs/api/openapi.json`](website/docs/api/openapi.json).

In Prowlarr, use the Indexarr base URL and set the API path to
`/api/torznab` (the compatibility `/api/torznab/api` path is also accepted).

## Workspace layout

The Rust workspace is split into focused crates:

```text
indexarr-core        configuration, models, database, and errors
indexarr-identity    Ed25519 contributor identity and ban list
indexarr-parser      torrent-name parsing
indexarr-classifier  content type and quality classification
indexarr-search      PostgreSQL search and facets
indexarr-web         Axum server, REST routes, SPA hosting, and Torznab
indexarr-tmdb        TMDB client and rate limiting
indexarr-dht         DHT engine, ingest, and metadata resolution
indexarr-announcer   tracker scraping
indexarr-sync        delta exchange, gossip, discovery, and reputation
indexarr-xmpp        XMPP/MUC peer discovery
indexarr-bep51       BEP 51 protocol support
indexarr-bep28       BEP 28 protocol support
indexarr-resolver-v2 resolver experiments and integration tests
ui/                  Vue 3 + Vite + TypeScript + Pinia frontend
```

Database tables are created and upgraded automatically when the application
starts; no separate migration command is required.

## Production and release notes

- [`Dockerfile`](Dockerfile) builds the Vue SPA and Rust binary in a multi-stage image.
- [`installer/linux-install.sh`](installer/linux-install.sh) installs a Linux binary, PostgreSQL, and a systemd service.
- `installer/windows.nsi` and `installer/setup.ps1` package the Windows binary and a bundled PostgreSQL setup.
- CI workflows live under [`.github/workflows/`](.github/workflows/).
- Version-specific changes are documented in [`RELEASE_NOTES_v0.3.2.md`](RELEASE_NOTES_v0.3.2.md) and the earlier release notes.

For architecture and synchronization details, see [`CLAUDE.md`](CLAUDE.md),
[`SYNC_NETWORKING.md`](SYNC_NETWORKING.md), and [`bep-uplift.md`](bep-uplift.md).

## Contributing

Before opening a change, run the Rust formatting, Clippy, workspace tests, and
frontend checks listed above. Keep secrets out of source files and logs. Changes
that affect synchronization, database compatibility, or public API behavior
should include tests and an update to the relevant documentation or release
notes.
