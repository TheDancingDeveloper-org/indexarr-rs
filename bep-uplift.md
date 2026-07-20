# BEP library uplift — `librtbit-*` to crates.io, parity with `btpydht`

Companion to `uplift.md`. That doc tracks the *application-level* roadmap
(get peer/tracker discovery working in indexarr-rs). This doc tracks the
*library* roadmap (turn rustTorrent's `librtbit-*` crate family into a
properly-published BEP toolkit on crates.io, with parity audited against
the existing Python `btpydht` library).

> Status: **draft, awaiting decisions** — see "Open decisions" at the end.
> Plan once decisions are locked.

---

## Background

Two BEP libraries already exist inside this org:

- **Python**: `pythonTorrentDHT` (Forgejo: `indexarr/pythonTorrentDHT`),
  importable as `btpydht`. ~5,300 LOC. Fork of nitmir/btdht extended for
  Python 3.10+. Used by Python `Indexarr` for DHT crawl + BEP 9 metadata
  fetch.
- **Rust**: 13-crate `librtbit-*` family (Forgejo org `indexarr`, repos
  `librtbit`, `librtbit-core`, `librtbit-bencode`, etc). Powers
  rustTorrent. Six members already on crates.io; the BEP-rich members
  (`peer-protocol`, `tracker-comms`) are not.

`indexarr-rs` only consumes `librtbit-dht` today. Tier 2 and Tier 3 of
`uplift.md` will pull in `librtbit-peer-protocol` and
`librtbit-tracker-comms` — which gives us a forcing function to publish
them properly.

---

## BEP coverage matrix

| BEP | Title | btpydht | librtbit | Notes |
|---|---|---|---|---|
| 5 | DHT Protocol | ✅ `dht.py` | ✅ `librtbit-dht` | parity |
| 9 | ut_metadata | ✅ `metadata.py:fetch_extended_from_peers` | ✅ `librtbit-peer-protocol::extended::ut_metadata` | parity |
| 10 | Extension Protocol | ✅ in `metadata.py` | ✅ `librtbit-peer-protocol::extended::handshake` | parity |
| 11 | PEX (`ut_pex`) | ❌ | ✅ `librtbit-peer-protocol::extended::ut_pex` | rust ahead |
| 12 | `announce-list` (multitracker) | partial (parsed in metadata) | likely ✅ in `librtbit-core::torrent_metainfo` | verify |
| 15 | UDP tracker scrape | ❌ (Python uses libtorrent for this) | ✅ `librtbit-tracker-comms::tracker_comms_udp` | rust ahead |
| 28 | Tracker Exchange (`lt_tex`) | ❌ | ❌ | **gap — both** |
| 51 | DHT infohash sampling (`sample_infohashes`) | ✅ `dht.py` + `tests/test_bep51.py` | ❌ | **gap — Rust missing** |

**Net new BEP work** for full parity-or-better:

- **BEP 51** in `librtbit-dht` — port from btpydht (DHT message + crawler hook).
- **BEP 28** in `librtbit-peer-protocol` — net-new in both stacks.

Everything else either already exists in librtbit or is Python-only (we don't
need to backport BEP 11/15 *to* btpydht).

---

## Crate inventory

| Crate | Forgejo version | crates.io | Hygiene flags |
|---|---|---|---|
| `librtbit` | 0.0.1 | ❌ | placeholder version; facade unclear |
| `librtbit-core` | 5.0.0 | ✅ | — |
| `librtbit-bencode` | 3.1.0 | ✅ | — |
| `librtbit-buffers` | 4.2.0 | ✅ | — |
| `librtbit-clone-to-owned` | 3.0.1 | ✅ | — |
| `librtbit-dht` | 5.3.0 | ✅ | needs BEP 51 |
| `librtbit-peer-protocol` | 4.3.0 | ❌ | needs BEP 28; needs publish |
| `librtbit-tracker-comms` | 3.0.0 | ❌ | **description is wrong** (says "sha1 implementations"); needs publish |
| `librtbit-sha1-wrapper` | 4.1.0 | ✅ | — |
| `librtbit-upnp` | 1.0.0 | ❌ | needs publish (or skip — not BEP-relevant) |
| `librtbit-upnp-serve` | 1.0.1 | ❌ | needs publish (or skip) |
| `librtbit-lsd` | 0.1.0 | ❌ | **description empty**; pre-1.0; defer publish |
| `rtbit` (binary) | 0.0.1 | ❌ | placeholder; CLI, defer |

---

## Phases

### Phase A — Audit & spec lock (½ day)

1. Clone all 13 `librtbit-*` Forgejo repos into `~/Working/Active/apps/libs/`
   (workspace convention — `~/Working/CLAUDE.md` lists `libs/` as the canonical
   shared-crates location). Already partially there — verify what's local.
2. Build the parity audit per BEP, line-by-line. Don't trust "exists in
   both" — compare:
   - Wire-format edge cases (truncated responses, malformed bencode, unknown
     extension IDs).
   - Concurrent-access semantics (Python is single-thread+asyncio; Rust is
     real parallelism).
   - Routing-table behaviour (bucket splits, K=8 vs K=20, ping cadence).
3. Lock in the **target API** for each crate as it'll appear on crates.io.
   Breaking changes happen now, not after publish.

### Phase B — Develop in indexarr-rs (1–2 days)

Recommended: **B2 (consume + new crates)** — depend on
`librtbit-peer-protocol = "4.3"` etc. directly from the Forgejo cargo
registry. Net new code lives as new crates in indexarr-rs:

- `crates/indexarr-bep51` — DHT `sample_infohashes` query/response + crawler
  hook. Wraps `librtbit-dht`. When stable, the *crate's content* moves into
  `librtbit-dht` itself; this crate gets retired.
- `crates/indexarr-bep28` — `lt_tex` extension message type. Mirrors the
  shape of `librtbit-peer-protocol::extended::ut_pex.rs`. When stable,
  moves into `librtbit-peer-protocol`.
- `crates/indexarr-resolver-v2` — the actual orchestrator that uses BEP 9
  from `librtbit-peer-protocol` to drive metadata fetch. Replaces the stub
  in `indexarr-dht::resolver`. Stays in indexarr-rs (it's app-layer).

Why B2:

- One canonical home per crate (rustTorrent's repo). No vendoring drift.
- New BEP work is each its own crate during development, so parity tests
  can be written before merging back.
- When backported, each becomes a discrete PR against rustTorrent's
  individual crate repos.

Alternative B1 (vendor) is viable if Forgejo cross-repo workflow is
painful, but creates two-master problem.

### Phase C — Crate hygiene & publish (½ day)

Per-crate checklist before `cargo publish` to crates.io:

- [ ] `description` is accurate (fix `librtbit-tracker-comms` and `librtbit-lsd` first)
- [ ] `keywords` (max 5) — picks: `bittorrent`, `dht`, `bep`, `p2p`, `protocol`
- [ ] `categories` — `network-programming`, `parsing`, `asynchronous`
- [ ] `repository = "https://github.com/TheDancingDeveloper-org/rustTorrent"`
- [ ] `homepage`, `documentation` URLs (docs.rs auto)
- [ ] `license` declared (MPL-2.0 to match upstream xmpp-rs convention? confirm)
- [ ] `readme = "README.md"` and per-crate README exists with:
  - One-paragraph "what this crate is"
  - BEP coverage table (from this doc)
  - Quick-start example matching the doctest
  - Stability + MSRV statement
- [ ] Public API doc-comments on every `pub fn`, `pub struct`, `pub enum`, `pub trait`
- [ ] At least one runnable example in `examples/` (e.g. `cargo run --example dht_get_peers`)
- [ ] Tests pass with `--no-default-features` and `--all-features`
- [ ] MSRV declared in Cargo.toml (`rust-version = "1.82"` or whatever the workspace uses)
- [ ] CHANGELOG.md entry with version bump rationale
- [ ] CI green on the source repo

Backport pipeline (each crate as discrete PR against its rustTorrent repo):

1. Open PR with new BEP modules + hygiene fixes.
2. Bump versions:
   - `librtbit-peer-protocol` → 4.4.0 (BEP 28 added; backwards-compatible)
   - `librtbit-dht` → 5.4.0 (BEP 51 added; backwards-compatible)
   - `librtbit-tracker-comms` → 3.1.0 (description fix; potentially API tidy)
3. `cargo publish` from each crate's `master` branch (after CI green) — first
   the deps, then dependents, in topological order.
4. Update `indexarr-rs/Cargo.toml` to depend on crates.io versions.
5. Retire `indexarr-bep51`, `indexarr-bep28` from indexarr-rs (or keep as
   thin re-exports for stability during transition).

### Phase D — indexarr-rs application work

Now `uplift.md`'s Tier 2/3 roadmap lands using polished public crates.
Order:

1. Tier 2.1 (BEP 15 UDP scrape via `librtbit-tracker-comms`).
2. Tier 2.2 (DHT peer-count refresher via `librtbit-dht`).
3. Tier 3.1 (BEP 9 metadata fetch via `librtbit-peer-protocol`).
4. Tier 3.2 (BEP 28 lt_tex — uses our new code).
5. Tier 3.3 (BEP 11 PEX — already in `librtbit-peer-protocol`, just enable).

---

## Topological dependency order (for publish)

Existing crates on crates.io publish first; downstream pulls fresh:

```
1. librtbit-clone-to-owned        (already published, 3.0.1)
2. librtbit-buffers                (already published, 4.2.0)
3. librtbit-bencode                (already published, 3.1.0)
4. librtbit-sha1-wrapper           (already published, 4.1.0)
5. librtbit-core                   (already published, 5.0.0)
6. librtbit-dht                    PUBLISH 5.4.0  (was 5.3.0; +BEP 51)
7. librtbit-peer-protocol          PUBLISH 4.4.0  (first crates.io publish; +BEP 28)
8. librtbit-tracker-comms          PUBLISH 3.1.0  (first crates.io publish; description fix)
9. librtbit-upnp                   PUBLISH 1.0.0  (first crates.io publish — optional, not BEP-related)
10. librtbit-upnp-serve            PUBLISH 1.0.1  (first crates.io publish — optional)
11. librtbit-lsd                   defer to 1.0.0; description + LSD spec writeup needed
12. librtbit                       defer; bump to real version once member crates are stable
13. rtbit                          defer; CLI, separate concern
```

---

## Open decisions

These need your call before the plan goes from "draft" to "execute":

1. **B1 (vendor) vs B2 (consume + new crates)** — recommend **B2**.
2. **License for new BEP work** — match the family. Need to confirm what
   the family currently uses (MPL-2.0? MIT/Apache?). Will check during
   Phase A audit. indexarr-rs itself stays AGPL-3.0; new generic library
   crates likely should be MPL-2.0 or MIT/Apache dual.
3. **crates.io namespace** — keep `librtbit-*`. Consistent with what's
   there, search-friendly.
4. **CHANGELOG location** — per-crate `CHANGELOG.md` in each rustTorrent
   crate repo (standard) — confirm.
5. **Backport target branch in rustTorrent** — `master` direct PR vs
   feature branch + cumulative release branch?
6. **Coordinate with rustTorrent's own roadmap** — is rustTorrent
   expecting these libraries to evolve in lock-step with its own client
   work? Need to talk to whoever owns rustTorrent (could be just you).

---

## Out of scope (for this uplift)

- Replacing `btpydht` for the Python Indexarr — Python Indexarr is now
  legacy (replaced by indexarr-rs on hertzde3). btpydht stays for any
  remaining consumers; we don't need to keep it in lockstep.
- Implementing BEP 12 (announce-list multitier) — already in
  `librtbit-core::torrent_metainfo` per audit, just not exercised. No
  publish blocker.
- `librtbit-lsd` (Local Service Discovery) — needs writeup before publish;
  not blocking BEP discovery work; defer to its own task.
- Rebranding away from `librtbit-*` namespace.

---

## Phase A audit findings (2026-04-25)

Phase A complete. The original draft above is **stale on several points** —
versions and a couple of "hygiene flags" have moved on. Source of truth from
here onward is this section.

### A.1 — Corrected crate inventory

All 12 `librtbit-*` repos cloned to `~/Working/Active/apps/libs/`, all on
`main`, clean (just `Cargo.lock` drift + codesight markers). The version
numbers in the original inventory table (5.x / 4.x / 3.x) are wrong; the
family was reset to `0.1.x` for a clean crates.io start. License is **MIT
across the family** — answers open-decision #2 (no MPL/Apache discussion
needed; stay MIT).

| Crate | Local version | crates.io | Forgejo registry | Repo URL in Cargo.toml |
|---|---|---|---|---|
| librtbit-clone-to-owned | 0.1.1 | ✅ 0.1.1 | ✅ 0.1.1 | github |
| librtbit-buffers | 0.1.1 | ✅ 0.1.1 | ✅ 0.1.1 | github |
| librtbit-bencode | 0.1.1 | ✅ 0.1.1 | ✅ 0.1.1 | github |
| librtbit-sha1-wrapper | 0.1.1 | ✅ 0.1.1 | ✅ 0.1.1 | github |
| librtbit-core | 0.1.1 | ✅ 0.1.1 | ✅ 0.1.1 | github |
| librtbit-dht | 0.1.1 | ✅ 0.1.1 | ✅ 0.1.1 | github |
| librtbit-peer-protocol | 0.1.1 | ❌ | ✅ 0.1.1 | **forgejo private IP** |
| librtbit-tracker-comms | 0.1.2 | ❌ | ✅ 0.1.2 | **forgejo private IP** |
| librtbit-upnp | 0.1.1 | ❌ | ✅ 0.1.1 | **forgejo private IP** |
| librtbit-upnp-serve | 0.1.1 | ❌ | ✅ 0.1.1 | **forgejo private IP** |
| librtbit-lsd | 0.1.1 | ❌ | ✅ 0.1.1 | **forgejo private IP** |
| librtbit | 0.1.1 | ❌ | ✅ 0.1.1 | **forgejo private IP** |

> **Correction (2026-04-25, post-audit):** earlier draft above marked the
> bottom 6 as "not published anywhere". They are in fact on the Forgejo
> cargo registry — the unauthenticated curl probes used during Phase A
> returned empty bodies and were misread as 404s. `cargo fetch` with the
> standard `~/.cargo/credentials.toml` resolves all 12 cleanly. So
> consumption from indexarr-rs is unblocked today; only the
> **crates.io** publish for the bottom 6 remains as a Phase C task.

**Hygiene flags already resolved** (vs original draft):
- `librtbit-tracker-comms` description fixed → "HTTP and UDP tracker communication for the rtbit BitTorrent client" (was "sha1 implementations").
- `librtbit-lsd` description fixed → "BEP 14 Local Service Discovery for the rtbit BitTorrent client" (was empty).

**Hygiene flags still open across the whole family** (incl. the published 6):
- No `keywords`, `categories`, `homepage`, `documentation`, `readme`,
  `rust-version` declared in any `Cargo.toml`. Worth a sweep PR per crate
  — even the already-published ones, as a 0.1.2 cleanup release.
- The 6 unpublished crates still have `repository = "http://100.92.54.45:3002/..."`
  (Tailscale IP, internal). Won't render usable links on docs.rs / crates.io.
  Must flip to `https://github.com/AusAgentSmith-org/<crate>` (matching the
  6 already-published crates) before first publish.
- Edition `2024` is correct (stabilised in Rust 1.85, Feb 2025) — ignore any
  tooling that flags it.

### A.2 — BEP parity audit (line-by-line, both stacks)

Audited against `~/Working/Active/RefenceMaterials/reference/External Repos/pythonTorrentDHT`
(`btpydht`, 7,340 LOC across 14 modules — original draft said 5,300, also
stale). Spot-checks below have file:line citations; full agent transcripts
were captured during this audit.

**BEP 5 (DHT Protocol)** — parity confirmed.
- `librtbit-dht`: `bprotocol.rs:433-572` (message kinds + serialize),
  `routing_table.rs:304` (K=8), `routing_table.rs:480` (max RT 512),
  tokio + `Arc<DhtState>` + `RwLock<RoutingTable>` + per-instance worker task.
  Rate-limited to 250 q/s by default (`dht.rs:80-95`).
- `btpydht`: `dht.py:1830-1907` (queries), `dht.py:1942` (K=8),
  `dht.py:2074-2120` (bucket-split), threaded with `threading.Thread + Lock`.
- Semantics match. Rust has explicit per-second rate limit; Python relies on
  socket pacing. No behavioural gap blocking parity.

**BEP 9 / BEP 10 (ut_metadata + extended)** — parity confirmed, Rust slightly
ahead on edge-case coverage.
- `librtbit-peer-protocol`: `extended/handshake.rs:9-30`,
  `extended/ut_metadata.rs:92-216` with full bound checks (oversized payload,
  piece out-of-bounds, exact-size mismatch, trailing bytes).
- `btpydht`: `metadata.py:217-552`. Single-threaded, blocking sockets.
  Caller-managed retry across peers (`fetch_metadata_from_peers`).
- Rust has the codec; **no built-in orchestrator**. The "drive a metadata
  fetch end-to-end" loop is caller-side — that loop is what
  `indexarr-rs::resolver-v2` will own (Phase B).

**BEP 11 (PEX)** — present in Rust (`extended/ut_pex.rs`), absent in btpydht
(parses incoming PEX in `metadata.py:182-213` but doesn't produce). Not a
blocker; rust-ahead.

**BEP 15 (UDP tracker scrape)** — present in Rust
(`librtbit-tracker-comms::tracker_comms_udp::UdpTrackerClient`), absent in
btpydht (Python Indexarr historically called libtorrent for this). Rust-ahead.

**BEP 12 (announce-list)** — assumed present in `librtbit-core::torrent_metainfo`
per the original draft. **Not re-verified this audit** (low priority — already
in the "out of scope" list). Verify before consuming if needed.

**BEP 28 (lt_tex)** — confirmed **absent in both stacks**. `grep -r "lt_tex|BEP.28|bep28"`
returns zero hits in `librtbit-peer-protocol`. Net-new work for Phase B.

**BEP 51 (sample_infohashes)** — confirmed **absent in `librtbit-dht`**.
Exhaustive grep returns zero matches. Present in `btpydht`:
- Query handler: `dht.py:1476-1487` (`_on_sample_infohashes_query`).
- Response builder: `krcp.py:178-179`, `krcp.py:388-406` (compact nodes
  ≤8 + random sample of stored infohashes ≤20, interval 0–21600s per spec).
- User callback hook: `dht.py:1332-1355` (`on_sample_infohashes_*`).
- Tests: `tests/test_bep51.py` (271 LOC).
This is the primary BEP gap for the Rust side.

**Bonus**: `librtbit-peer-protocol` already implements **BEP 55 (ut_holepunch)**
(`extended/ut_holepunch.rs`). Not on the original matrix; useful for NAT
traversal in Phase B+.

### A.3 — Critical API gap (separate from the BEP gap)

`librtbit-dht` exposes only **outbound queries** — `get_peers(infohash)` returns
a stream of peers for an infohash *the caller already knows*. There is **no
callback API** for "the DHT just observed someone querying / announcing
infohash X" (which is the actual indexarr crawl signal).

Today `indexarr-rs::indexarr-dht::engine.rs:98` works around this by sending
`get_peers(random_id)` queries — the random IDs cause neighbour DHT nodes to
respond with peer/infohash data which we then sniff. It works, but:
- It's bandwidth-inefficient (we're sending queries to *receive* discovery).
- It misses the announce_peer traffic flowing past us (we hear queries
  *we* sent, not queries *others* sent).
- BEP 51 and a passive observer hook would both help; they solve overlapping
  problems and can be designed together.

**Two related additions needed in `librtbit-dht`**:
1. **BEP 51 client + server** — query other DHT nodes for samples; respond
   to incoming `sample_infohashes` queries.
2. **Passive observation hook** — a callback (or subscription channel)
   `on_observed_infohash(info_hash, source: ObservedFrom)` so consumers
   can ingest infohashes seen in incoming queries we route. This is a
   pure addition to the existing crate, no protocol work.

Both ride together as `librtbit-dht 0.2.0` (next minor).

### A.4 — Locked target API for first crates.io publish (per crate)

For each crate that's about to make its first crates.io appearance — what
breaking changes (if any) ride this release. Lock them now; everything else
can wait for the next minor.

**`librtbit-peer-protocol` 0.1.x → first publish**:
- Keep `ExtendedMessage::Dyn(u8, BencodeValue<ByteBuf>)` even though it leaks
  `librtbit-bencode`. That's fine — it's a sister crate, also published, also
  0.x; the semver coupling is acceptable and disclosed in docs.
- Add `examples/extended_handshake.rs` and `examples/ut_metadata_fetch.rs`
  (both currently absent — required-quality bar for first publish).
- Move tests out of `src/extended/mod.rs` inline `#[cfg(test)]` block into
  `tests/extended_message_roundtrip.rs` for crates.io discoverability.
- BEP 28 (`lt_tex`) **does not block first publish** — adds in 0.2.0.

**`librtbit-tracker-comms` 0.1.2 → first publish**:
- ⚠ **Glob re-export**: `lib.rs:5` does `pub use tracker_comms::*;`.
  This is a stability risk — every internal item becomes part of the public
  contract. Recommend replacing with explicit re-exports of the intended
  surface (`TrackerClient`, request/response types, `UdpTrackerClient`).
  This **is** a breaking change and must happen before first publish.
- Description already fixed.

**`librtbit-upnp` / `librtbit-upnp-serve` 0.1.x → first publish**:
- Optional / not BEP-related. Defer unless something in indexarr-rs needs
  them. No locked-API decisions to make right now.

**`librtbit-lsd` 0.1.x**:
- Defer to its own task. Description fixed but no real consumer drives the
  API surface yet. Don't first-publish a crate nobody depends on.

**`librtbit` (high-level facade) 0.1.1**:
- Defer. Facade for `Session`, `Api`, full client. Out of scope for
  bep-uplift; that's a rustTorrent-client decision, not an indexarr one.

**Already-published crates (0.1.1, no API churn)**:
- `librtbit-dht` 0.1.1 → bump to **0.2.0** when BEP 51 + passive-observation
  hook land (additive, but a notable feature bump warrants minor — and we
  can use the bump to also add the missing `keywords`/`categories` metadata).
- `librtbit-core`, `-bencode`, `-buffers`, `-clone-to-owned`,
  `-sha1-wrapper`: 0.1.2 patch sweep to add `keywords`/`categories`/
  `rust-version` metadata. No code change.

### A.5 — Open decisions (closed-out 2026-04-25)

Resolved by audit:
- ~~License~~ → **MIT** (matches family).
- ~~btpydht LOC~~ → 7,340, not 5,300 (cosmetic).
- crates.io namespace → keep `librtbit-*` (no change).

All six open decisions have a final disposition now:

1. ~~**B1 (vendor) vs B2 (consume + new crates)**~~ — **resolved B2 by execution.**
   `crates/indexarr-bep51`, `crates/indexarr-bep28`, and
   `crates/indexarr-resolver-v2` were all built and shipped this session,
   consuming `librtbit-peer-protocol` from the Forgejo registry. No
   vendoring, no two-master problem. Confirmed working as of pipeline 55.

2. ~~**`librtbit-tracker-comms` glob re-export fix**~~ — **deferred until
   tracker-comms is queued for crates.io publish.** No consumer is
   currently asking for the explicit surface, no publish is gating on it,
   and refactoring blind costs more than the theoretical hygiene win.
   Reconsider the day someone files a Phase C ticket for tracker-comms
   crates.io publish — at that point we have a forcing function and a
   clear set of stable items to call out.

3. ~~**`librtbit-dht` 0.2.0 features grouping**~~ — **deferred until we
   have a metric that motivates it.** The current passive-discovery
   workaround in `crates/indexarr-dht/src/engine.rs:98` (random-target
   `get_peers()` queries) works. The expected gain from a real passive
   observation hook is real but unmeasured. Trigger condition: when
   indexarr-rs surfaces a "crawl rate would benefit from passive
   observation" metric, or when we want our nodes to be queryable via
   `sample_infohashes` (BEP 51 server side). At that point: ship BEP 51
   *and* the observation hook together as `librtbit-dht 0.2.0`
   (recommended in original draft).

4. ~~**CHANGELOG location**~~ — per-crate `CHANGELOG.md` confirmed
   (matches existing convention; nothing to do until a crates.io publish
   forces a real changelog entry).

5. ~~**Backport target branch**~~ — direct PR to `main` per crate, as
   already done for `librtbit-peer-protocol 0.1.2` this session
   (commit `24f8575`). No further question.

6. ~~**rustTorrent roadmap coordination**~~ — same maintainer; no open
   question.

### A.6 — Phase B kickoff readiness

Phase B sections 1-3 are **complete and deployed.** See [Phase B execution
log](#phase-b-execution-log-2026-04-25) below for what shipped and where.
Phase C (crates.io publishes) is now the only outstanding bep-uplift
work item, and per decision #2 above it's deferred until forced.

---

## Phase B execution log (2026-04-25)

| Section | What shipped | Where | Commit |
|---|---|---|---|
| B.1 | `crates/indexarr-bep51` — BEP 51 codec, 15 parity tests | indexarr-rs | `aca7dfa` |
| B.2 | `crates/indexarr-bep28` — BEP 28 (`lt_tex`) codec, 9 tests | indexarr-rs | `aca7dfa` |
| B.3 | `crates/indexarr-resolver-v2` — BEP 9 metadata-fetch orchestrator, 9 integration tests | indexarr-rs | `aca7dfa` |
| B.3 | `MetadataResolver` wired to `fetch_from_peer` — real BEP 9 fetch live, replaces the `Err("not yet implemented")` stub | `indexarr-dht/src/resolver.rs` | `c350bb6` |
| infra | Forgejo cargo registry wired into local + CI + Dockerfile; all librtbit-* deps unified on Forgejo | `.cargo/`, `.woodpecker.yml`, `Dockerfile`, all crate `Cargo.toml`s | `d7cd6dc`, `3cdbc3a`, `e715a0e` |
| lib fix | `librtbit-peer-protocol 0.1.1 → 0.1.2` — `PeerExtendedMessageIds` skip-None fix + regression test | `librtbit-peer-protocol`, Forgejo registry | `24f8575` |

### Phase D execution log (2026-04-26)

| Item | What shipped | Where | Commit |
|---|---|---|---|
| Tier 1.2 | `/api/v1/import` extended — accepts `trackers`, `nfo`, `seed_count`, `peer_count`, `discovered_at`; persists all to DB | `indexarr-web/src/routes/crud.rs` | `b2502ec` |
| Tier 2.2 | `peer_refresher` worker — DHT peer-count refresh on stale/trackerless resolved torrents; `INDEXARR_PEER_REFRESH_INTERVAL` (default 300s), `INDEXARR_PEER_REFRESH_BATCH` (default 100) | `crates/indexarr-dht/src/peer_refresher.rs` | `b2502ec` |
| BEP 28 | Live lt_tex consumer in `indexarr-resolver-v2` — advertises `lt_tex` (local ID 2) in BEP 10 extended handshake; harvests incoming lt_tex tracker lists; merges via JSONB union into `torrents.trackers` | `crates/indexarr-resolver-v2/src/lib.rs`, `crates/indexarr-dht/src/resolver.rs` | `b2502ec` |
| BEP 51 | `bep51_sampler` worker — standalone UDP socket, bootstraps from well-known DHT nodes, sends `sample_infohashes` queries, feeds hashes into shared ingest queue (source="bep51"), expands node queue from response `nodes`; gated on `INDEXARR_DHT_ENABLE_BEP51` | `crates/indexarr-dht/src/bep51_sampler.rs` | `b2502ec` |

**Note on BEP 51 server-side**: the sampler is client-only (outgoing queries). Server-side `sample_infohashes` responses require `librtbit-dht` to expose a KRPC query hook — not in the current public API. Deferred to `librtbit-dht 0.2.0` per decision #3. Not a blocker for hash discovery; client-side alone is the higher-value path.

**Note on BEP 28 send direction**: `indexarr-resolver-v2` currently only RECEIVES lt_tex. Sending our tracker list to peers requires knowing the peer's lt_tex extension ID, which is lost during `ExtendedHandshake` deserialization (`PeerExtendedMessageIds` has no lt_tex field). To send lt_tex, either parse the raw handshake bencode manually or extend `PeerExtendedMessageIds` in `librtbit-peer-protocol`. Deferred — receive-only is sufficient to harvest new trackers from active peers.

End state: all three new crates live in `indexarr-rs/crates/`, all 46
workspace tests pass, CI green, prod-indexarr on Node B redeployed and
running real BEP 9 metadata fetch as of `c350bb66f`.

Outstanding after 2026-04-25 close-out:
- Phase C crates.io publishes for the 6 unpublished crates — deferred
  (see decision #2 above).
