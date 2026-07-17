# Sync networking

Indexarr enables the sync worker and XMPP discovery by default. New installs
seed discovery from `https://bootstrap.indexarr.net`.

## Automatic public-address discovery

If `INDEXARR_SYNC_EXTERNAL_URL` is empty, Indexarr calls
`/api/v1/sync/observed-address` on its configured bootstrap peers. The
bootstrap returns the source IP it observed for the request. Indexarr then
advertises:

```text
INDEXARR_SYNC_EXTERNAL_SCHEME://observed-ip:effective-port
```

The effective port is selected in this order:

1. `INDEXARR_SYNC_EXTERNAL_PORT`, when non-zero.
2. `INDEXARR_SYNC_API_PORT`, when non-zero.
3. `INDEXARR_PORT`.

Automatic discovery finds the public egress address, including a VPN exit IP,
but cannot create an inbound NAT or VPN port-forward. Other peers validate the
advertised URL by fetching its sync manifest before adding it to their peer
table. Configure the firewall, router, container publish, or VPN forwarding so
the effective TCP port reaches Indexarr.

For a domain, HTTPS reverse proxy, or nonstandard path, set the complete URL
explicitly:

```text
INDEXARR_SYNC_EXTERNAL_URL=https://indexarr.example.net
```

Explicit configuration always takes precedence over automatic discovery.

## Dedicated sync listener

`INDEXARR_SYNC_API_PORT` starts a second HTTP listener containing only the
health and `/api/v1/sync/*` routes. This is useful with VPN providers that
assign a dynamic forwarded port: set the variable to that port before starting
Indexarr. The regular UI can remain on `INDEXARR_PORT`.

The same numeric port can carry DHT over UDP and the sync API over TCP. For
example, a VPN-assigned port can be used for both
`INDEXARR_DHT_BASE_PORT` and `INDEXARR_SYNC_API_PORT`.

## Relevant variables

| Variable | Default | Purpose |
|---|---:|---|
| `INDEXARR_SYNC_ENABLED` | `true` | Runs peer export, discovery, and gossip. |
| `INDEXARR_XMPP_ENABLED` | `true` | Joins the Indexarr discovery MUC. |
| `INDEXARR_XMPP_SERVER` | `conference.indexarr.net:5222` | DNS-only public Prosody endpoint used by the plaintext discovery connector. |
| `INDEXARR_SYNC_PEERS` | `["https://bootstrap.indexarr.net"]` | Bootstrap services used for PEX and address discovery. |
| `INDEXARR_SYNC_EXTERNAL_URL` | empty | Complete authoritative advertised URL. |
| `INDEXARR_SYNC_EXTERNAL_SCHEME` | `http` | Scheme used with an automatically observed IP. |
| `INDEXARR_SYNC_EXTERNAL_PORT` | `0` | Explicit advertised port; zero selects the effective listener port. |
| `INDEXARR_SYNC_API_PORT` | `0` | Optional restricted sync-only TCP listener. |

Wildcard, loopback, private, link-local, and otherwise non-public literal IP
URLs are rejected during XMPP peer validation. An install that cannot discover
a public address does not fall back to advertising `0.0.0.0`; it retries with
backoff and logs that explicit configuration is required.
