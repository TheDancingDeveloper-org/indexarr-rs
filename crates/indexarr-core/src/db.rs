use sqlx::PgPool;
use sqlx::postgres::PgPoolOptions;

use crate::config::Settings;

/// Initialize the PostgreSQL connection pool and run migrations.
pub async fn init_db(settings: &Settings) -> Result<PgPool, sqlx::Error> {
    let pool = PgPoolOptions::new()
        .max_connections(30)
        .min_connections(2)
        .acquire_timeout(std::time::Duration::from_secs(10))
        .connect(&settings.effective_db_url())
        .await?;

    run_migrations(&pool).await?;

    Ok(pool)
}

async fn run_migrations(pool: &PgPool) -> Result<(), sqlx::Error> {
    // Create tables
    sqlx::raw_sql(SCHEMA_SQL).execute(pool).await?;

    // Versions before the issue #3 fix inserted random get_peers lookup
    // targets as if they were observed info-hashes. The get_peers source was
    // exclusive to that broken crawler path, so unresolved rows from it are
    // not valid discoveries and only cause guaranteed BEP 9 failures.
    let removed =
        sqlx::query("DELETE FROM torrents WHERE source = 'get_peers' AND resolved_at IS NULL")
            .execute(pool)
            .await?
            .rows_affected();
    if removed > 0 {
        tracing::info!(rows = removed, "removed invalid random get_peers targets");
    }

    // Set up full-text search trigger (PostgreSQL only)
    sqlx::raw_sql(SEARCH_VECTOR_TRIGGER_SQL)
        .execute(pool)
        .await?;

    // Backfill any rows missing search_vector
    let result = sqlx::raw_sql(BACKFILL_SEARCH_VECTOR_SQL)
        .execute(pool)
        .await?;
    if result.rows_affected() > 0 {
        tracing::info!(
            rows = result.rows_affected(),
            "backfilled search_vector for existing torrents"
        );
    }

    Ok(())
}

const SCHEMA_SQL: &str = r#"
-- Torrents
CREATE TABLE IF NOT EXISTS torrents (
    info_hash       VARCHAR(40) PRIMARY KEY,
    name            TEXT,
    size            BIGINT,
    piece_length    INTEGER,
    piece_count     INTEGER,
    private         BOOLEAN NOT NULL DEFAULT FALSE,
    source          VARCHAR(20) NOT NULL DEFAULT 'sample',
    discovered_at   TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    resolved_at     TIMESTAMPTZ,
    resolve_attempts INTEGER NOT NULL DEFAULT 0,
    no_peers        BOOLEAN NOT NULL DEFAULT FALSE,
    announce_miss   INTEGER NOT NULL DEFAULT 0,
    retry_after     TIMESTAMPTZ,
    observations    INTEGER NOT NULL DEFAULT 1,
    priority        BOOLEAN NOT NULL DEFAULT FALSE,
    seed_count      INTEGER NOT NULL DEFAULT 0,
    peer_count      INTEGER NOT NULL DEFAULT 0,
    scraped_at      TIMESTAMPTZ,
    trackers        JSONB,
    announced_at    TIMESTAMPTZ,
    nfo             TEXT,
    search_vector   TSVECTOR,
    epoch           INTEGER NOT NULL DEFAULT 1,
    contributor_id  VARCHAR(20),
    sync_sequence   BIGINT
);

CREATE INDEX IF NOT EXISTS idx_torrents_discovered ON torrents (discovered_at);
CREATE INDEX IF NOT EXISTS idx_torrents_resolved ON torrents (resolved_at);
CREATE INDEX IF NOT EXISTS idx_torrents_unresolved ON torrents (resolved_at) WHERE resolved_at IS NULL;
CREATE INDEX IF NOT EXISTS idx_torrents_source ON torrents (source);
CREATE INDEX IF NOT EXISTS idx_torrents_search ON torrents USING gin (search_vector);
CREATE INDEX IF NOT EXISTS idx_torrents_unannounced ON torrents (announced_at) WHERE announced_at IS NULL;
CREATE INDEX IF NOT EXISTS idx_torrents_sync_sequence ON torrents (sync_sequence);
CREATE INDEX IF NOT EXISTS idx_torrents_seed_count ON torrents (seed_count DESC) WHERE seed_count > 0;
CREATE INDEX IF NOT EXISTS idx_torrents_peer_count ON torrents (peer_count DESC) WHERE peer_count > 0;
CREATE INDEX IF NOT EXISTS idx_torrents_size ON torrents (size) WHERE size IS NOT NULL;
CREATE INDEX IF NOT EXISTS idx_torrents_sync_export ON torrents (resolved_at, sync_sequence) WHERE resolved_at IS NOT NULL AND sync_sequence IS NULL;
CREATE INDEX IF NOT EXISTS idx_torrents_private ON torrents (private) WHERE private IS TRUE;
CREATE INDEX IF NOT EXISTS idx_torrents_no_peers ON torrents (no_peers) WHERE no_peers IS TRUE;

-- Torrent files
CREATE TABLE IF NOT EXISTS torrent_files (
    id          SERIAL PRIMARY KEY,
    info_hash   VARCHAR(40) NOT NULL REFERENCES torrents(info_hash) ON DELETE CASCADE,
    path        TEXT NOT NULL,
    size        BIGINT NOT NULL,
    extension   VARCHAR(20)
);

CREATE INDEX IF NOT EXISTS idx_files_info_hash ON torrent_files (info_hash);
CREATE INDEX IF NOT EXISTS idx_files_extension ON torrent_files (extension);

-- Torrent content (parsed metadata)
CREATE TABLE IF NOT EXISTS torrent_content (
    info_hash           VARCHAR(40) PRIMARY KEY REFERENCES torrents(info_hash) ON DELETE CASCADE,
    content_type        TEXT,
    title               TEXT,
    year                INTEGER,
    season              INTEGER,
    episode             INTEGER,
    episode_title       TEXT,
    "group"             VARCHAR(100),
    language            VARCHAR(50),
    resolution          TEXT,
    codec               TEXT,
    video_source        TEXT,
    modifier            TEXT,
    is_3d               BOOLEAN NOT NULL DEFAULT FALSE,
    hdr                 VARCHAR(20),
    audio_codec         VARCHAR(30),
    audio_channels      VARCHAR(10),
    edition             VARCHAR(50),
    bit_depth           VARCHAR(10),
    network             VARCHAR(20),
    quality_score       INTEGER,
    is_dubbed           BOOLEAN NOT NULL DEFAULT FALSE,
    is_complete         BOOLEAN NOT NULL DEFAULT FALSE,
    is_remastered       BOOLEAN NOT NULL DEFAULT FALSE,
    is_scene            BOOLEAN NOT NULL DEFAULT FALSE,
    is_proper           BOOLEAN NOT NULL DEFAULT FALSE,
    is_repack           BOOLEAN NOT NULL DEFAULT FALSE,
    platform            VARCHAR(30),
    has_subtitles       BOOLEAN NOT NULL DEFAULT FALSE,
    is_anime            BOOLEAN NOT NULL DEFAULT FALSE,
    music_format        VARCHAR(20),
    tmdb_id             INTEGER,
    imdb_id             VARCHAR(20),
    tmdb_data           JSONB,
    classified_at       TIMESTAMPTZ,
    classifier_version  VARCHAR(20)
);

CREATE INDEX IF NOT EXISTS idx_content_type ON torrent_content (content_type);
CREATE INDEX IF NOT EXISTS idx_content_title ON torrent_content (title);
CREATE INDEX IF NOT EXISTS idx_content_year ON torrent_content (year);
CREATE INDEX IF NOT EXISTS idx_content_resolution ON torrent_content (resolution);
CREATE INDEX IF NOT EXISTS idx_content_codec ON torrent_content (codec);
CREATE INDEX IF NOT EXISTS idx_content_source ON torrent_content (video_source);
CREATE INDEX IF NOT EXISTS idx_content_tmdb ON torrent_content (tmdb_id);
CREATE INDEX IF NOT EXISTS idx_content_imdb ON torrent_content (imdb_id);
CREATE INDEX IF NOT EXISTS idx_content_season_episode ON torrent_content (season, episode);
CREATE INDEX IF NOT EXISTS idx_content_platform ON torrent_content (platform);
CREATE INDEX IF NOT EXISTS idx_content_audio_codec ON torrent_content (audio_codec);
CREATE INDEX IF NOT EXISTS idx_content_language ON torrent_content (language);
CREATE INDEX IF NOT EXISTS idx_content_quality_score ON torrent_content (quality_score);
CREATE INDEX IF NOT EXISTS idx_content_network ON torrent_content (network);
CREATE INDEX IF NOT EXISTS idx_content_edition ON torrent_content (edition);

-- Tags
CREATE TABLE IF NOT EXISTS torrent_tags (
    id          SERIAL PRIMARY KEY,
    info_hash   VARCHAR(40) NOT NULL REFERENCES torrents(info_hash) ON DELETE CASCADE,
    tag         VARCHAR(100) NOT NULL,
    source      VARCHAR(20) NOT NULL DEFAULT 'classifier'
);

CREATE INDEX IF NOT EXISTS idx_tags_info_hash ON torrent_tags (info_hash);
CREATE INDEX IF NOT EXISTS idx_tags_tag ON torrent_tags (tag);
CREATE UNIQUE INDEX IF NOT EXISTS idx_tags_unique ON torrent_tags (info_hash, tag);

-- TMDB cache
CREATE TABLE IF NOT EXISTS tmdb_cache (
    id          SERIAL PRIMARY KEY,
    tmdb_id     INTEGER NOT NULL,
    media_type  VARCHAR(10) NOT NULL,
    data        JSONB NOT NULL,
    fetched_at  TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE UNIQUE INDEX IF NOT EXISTS idx_tmdb_cache_lookup ON tmdb_cache (tmdb_id, media_type);

-- Sync state
CREATE TABLE IF NOT EXISTS sync_state (
    id              SERIAL PRIMARY KEY,
    peer_id         VARCHAR(64) NOT NULL UNIQUE,
    last_sync_at    TIMESTAMPTZ,
    last_sequence   BIGINT NOT NULL DEFAULT 0,
    peer_url        TEXT,
    first_seen      TIMESTAMPTZ,
    fail_count      INTEGER NOT NULL DEFAULT 0,
    reputation      DOUBLE PRECISION NOT NULL DEFAULT 100.0,
    source          VARCHAR(20) NOT NULL DEFAULT 'dht'
);

CREATE INDEX IF NOT EXISTS idx_sync_state_source ON sync_state (source);
CREATE INDEX IF NOT EXISTS idx_sync_state_health ON sync_state (fail_count, last_sync_at);

-- Content bans
CREATE TABLE IF NOT EXISTS content_bans (
    id          SERIAL PRIMARY KEY,
    pattern     TEXT NOT NULL,
    ban_type    VARCHAR(20) NOT NULL,
    reason      TEXT,
    created_at  TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    active      BOOLEAN NOT NULL DEFAULT TRUE
);

CREATE INDEX IF NOT EXISTS idx_content_ban_active ON content_bans (active) WHERE active IS TRUE;

-- Comments
CREATE TABLE IF NOT EXISTS torrent_comments (
    id          SERIAL PRIMARY KEY,
    info_hash   VARCHAR(40) NOT NULL REFERENCES torrents(info_hash) ON DELETE CASCADE,
    parent_id   INTEGER REFERENCES torrent_comments(id) ON DELETE CASCADE,
    nickname    VARCHAR(50) NOT NULL DEFAULT 'Anonymous',
    body        TEXT NOT NULL,
    fingerprint VARCHAR(64) NOT NULL,
    created_at  TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    edited_at   TIMESTAMPTZ,
    deleted     BOOLEAN NOT NULL DEFAULT FALSE
);

CREATE INDEX IF NOT EXISTS idx_comments_info_hash ON torrent_comments (info_hash);
CREATE INDEX IF NOT EXISTS idx_comments_parent ON torrent_comments (parent_id);
CREATE INDEX IF NOT EXISTS idx_comments_created ON torrent_comments (created_at);
CREATE INDEX IF NOT EXISTS idx_comments_fingerprint ON torrent_comments (fingerprint);

-- Votes
CREATE TABLE IF NOT EXISTS torrent_votes (
    id          SERIAL PRIMARY KEY,
    info_hash   VARCHAR(40) NOT NULL REFERENCES torrents(info_hash) ON DELETE CASCADE,
    fingerprint VARCHAR(64) NOT NULL,
    value       INTEGER NOT NULL,
    created_at  TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE INDEX IF NOT EXISTS idx_votes_info_hash ON torrent_votes (info_hash);
CREATE UNIQUE INDEX IF NOT EXISTS idx_votes_unique ON torrent_votes (info_hash, fingerprint);

-- Nuke suggestions
CREATE TABLE IF NOT EXISTS nuke_suggestions (
    id          SERIAL PRIMARY KEY,
    info_hash   VARCHAR(40) NOT NULL REFERENCES torrents(info_hash) ON DELETE CASCADE,
    fingerprint VARCHAR(64) NOT NULL,
    reason      TEXT NOT NULL,
    created_at  TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    reviewed    BOOLEAN NOT NULL DEFAULT FALSE,
    reviewed_at TIMESTAMPTZ,
    outcome     VARCHAR(20)
);

CREATE INDEX IF NOT EXISTS idx_nuke_info_hash ON nuke_suggestions (info_hash);
CREATE UNIQUE INDEX IF NOT EXISTS idx_nuke_unique_per_ip ON nuke_suggestions (info_hash, fingerprint);
CREATE INDEX IF NOT EXISTS idx_nuke_reviewed ON nuke_suggestions (reviewed) WHERE reviewed IS FALSE;

-- Peer records (DHT-style: signed DeviceId → address announcements for rsSync)
CREATE TABLE IF NOT EXISTS peer_records (
    device_id   VARCHAR(64) PRIMARY KEY,
    addresses   JSONB NOT NULL,
    expiry      TIMESTAMPTZ NOT NULL,
    public_key  VARCHAR(100) NOT NULL,
    signature   VARCHAR(200) NOT NULL,
    updated_at  TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE INDEX IF NOT EXISTS idx_peer_records_expiry ON peer_records (expiry);
"#;

const SEARCH_VECTOR_TRIGGER_SQL: &str = r#"
DO $$
BEGIN
    CREATE OR REPLACE FUNCTION torrents_search_vector_update() RETURNS trigger AS $func$
    BEGIN
        NEW.search_vector := to_tsvector('english', coalesce(NEW.name, ''));
        RETURN NEW;
    END;
    $func$ LANGUAGE plpgsql;

    IF NOT EXISTS (
        SELECT 1 FROM pg_trigger WHERE tgname = 'trg_torrents_search_vector'
    ) THEN
        CREATE TRIGGER trg_torrents_search_vector
            BEFORE INSERT OR UPDATE OF name ON torrents
            FOR EACH ROW
            EXECUTE FUNCTION torrents_search_vector_update();
    END IF;
END;
$$;
"#;

const BACKFILL_SEARCH_VECTOR_SQL: &str = r#"
UPDATE torrents
SET search_vector = to_tsvector('english', coalesce(name, ''))
WHERE search_vector IS NULL AND name IS NOT NULL;
"#;
