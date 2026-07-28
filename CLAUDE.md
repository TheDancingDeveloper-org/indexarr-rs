# Indexarr (Rust)

Indexarr — decentralized torrent indexing with DHT crawling, content classification, gossip-based P2P sync, contributor identity, and XMPP/MUC peer discovery.

## Architecture

11-crate Rust workspace. Single binary (`indexarr`) runs selectable workers via `--workers` flag. All workers share a PostgreSQL database through sqlx async pool.

### Tech Stack
- **Rust 2024 edition** — Axum 0.8, SQLx 0.8 (async PostgreSQL), Tokio 1.x
- **PostgreSQL 17** (production)
- **librtbit-dht** — DHT protocol (BEP 5), peer discovery
- **librtbit-peer-protocol** — BitTorrent wire protocol (BEP 9/10)
- **tokio-xmpp 4** — direct XMPP client (we ship our own `PlaintextConnector` + a custom MUC channel; the higher-level `xmpp` crate drops non-self presences and would not work)
- **Prosody** — XMPP server hosted in `indexarr/ops/prod/indexarr-bootstrap/` for the `indexarr-sync@conference.indexarr.net` MUC
- **Vue 3 + Vite + TypeScript + Pinia** — Frontend SPA
- **Docker** — Multi-stage build (Node 22 for UI, Rust for backend, Debian slim runtime)

## Development

```bash
# Docker (recommended)
docker compose up -d

# Local development
cargo build
INDEXARR_DB_URL=postgres://indexarr:indexarr@localhost:5432/indexarr \
  cargo run -- --workers http_server

# Tests
cargo test --workspace

# All workers
cargo run -- --all
```

## Workers

| Worker | What it does |
|--------|-------------|
| `http_server` | Axum HTTP — REST API, Torznab XML, Vue SPA |
| `dht_crawler` | N librtbit-dht instances crawling for info_hashes |
| `resolver` | BEP 9 metadata fetching + content pipeline |
| `announcer` | HTTP tracker scrape for seed/peer validation |
| `sync` | 3-loop gossip protocol (export, discovery, gossip) — auto-starts the XMPP channel when `INDEXARR_XMPP_ENABLED=true` |

## Crate Map

```
indexarr-core        Config, models, DB schema, errors
indexarr-identity    Ed25519 keypair, contributor ID, ban list
indexarr-parser      Torrent name parser (regex-based)
indexarr-classifier  Content type detection, quality scoring
indexarr-search      PostgreSQL FTS + 16 faceted filters
indexarr-web         Axum server, ~35 REST endpoints, Torznab XML
indexarr-tmdb        TMDB client with rate limiter + circuit breaker
indexarr-dht         DHT engine, hash ingest, metadata resolver
indexarr-announcer   Rolling pool tracker scraper
indexarr-sync        Gossip protocol, delta export/merge, peer discovery, epoch
indexarr-xmpp        XMPP/MUC peer discovery (XEP-0045 join, XEP-0077 register), feeds discovered peers into the shared `PeerTable` from indexarr-sync
```

## Production deployments

All managed via Komodo (`indexarr/ops`). Image is `ghcr.io/thedancingdeveloper-org/indexarr-rs:<sha>`; the historical Forgejo image remains only as a rollback artifact during migration soak.

| Stack | Periphery | URL | Workers | Purpose |
|-------|-----------|-----|---------|---------|
| `prod-indexarr` | Local (Node B, 192.168.1.75) | http://100.92.54.45:8086 | `--all` | Full crawler, DHT canonical ports 6881-6900/udp + 6890/tcp |
| `prod-indexarr-bootstrap` | vultr (208.76.222.14) | https://bootstrap.indexarr.net | `http_server,sync` | Public sync relay + Prosody XMPP server (5222 open). DHT off. |
| `prod-indexarr-hetzner3` | hetzner3 (100.76.94.94) | http://100.76.94.94:8080 | `http_server` only | Bootstrap-clean deploy; flip features on individually via Komodo stack env. |

The Vultr bootstrap stack also runs **Prosody** (Lua XMPP server, image `prosody/prosody:latest`). MUC config is in `indexarr/ops/prod/indexarr-bootstrap/prosody.cfg.lua`. Conference component: `conference.indexarr.net` (DNS-only A record → 208.76.222.14, NOT CF-proxied — XMPP doesn't tunnel through CF). The Caddy site `bootstrap.indexarr.net` (CF-proxied) is layered on top of `:8087` for the HTTP relay API.

## Environment Variables

All prefixed with `INDEXARR_`. See `crates/indexarr-core/src/config.rs` for the full list. Notable XMPP/sync settings:

| Var | Purpose |
|-----|---------|
| `INDEXARR_XMPP_ENABLED` | Start the XMPP MUC channel (defaults false) |
| `INDEXARR_XMPP_SERVER` | `host:port` override. Empty → SRV+STARTTLS. Set → `PlaintextConnector` (no TLS). Use the latter when reaching the bootstrap Prosody over Tailscale. |
| `INDEXARR_XMPP_JID` / `_PASSWORD` | If empty, derived deterministically from the contributor id (`tn-XXXX@indexarr.net`, password = the id itself). |
| `INDEXARR_XMPP_MUC_ROOM` | Defaults to `indexarr-sync@conference.indexarr.net`. |
| `INDEXARR_SYNC_EXTERNAL_URL` | URL peers should hit us at — baked into the MUC nick (`{contributor_id}\|{url}`). |
| `INDEXARR_DHT_BASE_PORT` | Defaults 6881. The compose maps host UDP 6881-6900 → container. Shift if 6881-6884 collides with another container. |

## Docker

| File | Purpose |
|------|---------|
| `docker-compose.yml` | All workers, sync+XMPP enabled by default, peers seeded with `https://bootstrap.indexarr.net` |
| `docker-compose.sync.yml` | Web UI + sync only (no crawling) |

## CI / release

GitHub Actions — workflows under `.github/workflows/`.

| Trigger | Steps that fire |
|---------|-----------------|
| `push: main` | Rust/UI CI, migration policy, and immutable GHCR container publication |
| `tag` | Rust/UI CI, migration policy, multi-arch GHCR publication, and GitHub release workflow |
| `pull_request` | Rust/UI CI and migration policy |

Linux/Windows binary builds are tag-only — saves ~10 min per main push.

The canonical GHCR package is public and Komodo pins immutable SHA tags.

## Tests

```bash
cargo test --workspace    # parser + classifier + xmpp::parse_nick + ...
```
