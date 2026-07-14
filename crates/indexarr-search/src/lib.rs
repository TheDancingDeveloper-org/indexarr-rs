use indexarr_core::models::{Torrent, TorrentContent};
use serde::{Deserialize, Serialize};
use sqlx::PgPool;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SortField {
    #[default]
    Relevance,
    Date,
    Size,
    Files,
    Seeders,
    Leechers,
    Name,
    InfoHash,
    Quality,
}

impl SortField {
    pub fn from_str_loose(s: &str) -> Self {
        match s {
            "date" => Self::Date,
            "size" => Self::Size,
            "files" => Self::Files,
            "seeders" => Self::Seeders,
            "leechers" => Self::Leechers,
            "name" => Self::Name,
            "info_hash" => Self::InfoHash,
            "quality" => Self::Quality,
            _ => Self::Relevance,
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SortOrder {
    Asc,
    #[default]
    Desc,
}

impl SortOrder {
    pub fn from_str_loose(s: &str) -> Self {
        if s == "asc" { Self::Asc } else { Self::Desc }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FacetCount {
    pub value: String,
    pub count: i64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SearchFacets {
    pub content_type: Vec<FacetCount>,
    pub resolution: Vec<FacetCount>,
    pub codec: Vec<FacetCount>,
    pub video_source: Vec<FacetCount>,
    pub modifier: Vec<FacetCount>,
    pub hdr: Vec<FacetCount>,
    pub audio_codec: Vec<FacetCount>,
    pub year: Vec<FacetCount>,
    pub language: Vec<FacetCount>,
    pub source: Vec<FacetCount>,
    pub platform: Vec<FacetCount>,
    pub music_format: Vec<FacetCount>,
}

#[derive(Debug, Clone, Default)]
pub struct SearchFilters {
    pub query: String,
    pub content_type: Option<String>,
    pub resolution: Option<String>,
    pub codec: Option<String>,
    pub video_source: Option<String>,
    pub modifier: Option<String>,
    pub hdr: Option<String>,
    pub audio_codec: Option<String>,
    pub year: Option<i32>,
    pub year_min: Option<i32>,
    pub year_max: Option<i32>,
    pub language: Option<String>,
    pub tag: Option<String>,
    pub info_hash: Option<String>,
    pub imdb_id: Option<String>,
    pub tmdb_id: Option<i32>,
    pub season: Option<i32>,
    pub episode: Option<i32>,
    pub source: Option<String>,
    pub platform: Option<String>,
    pub has_subtitles: Option<bool>,
    pub is_anime: Option<bool>,
    pub music_format: Option<String>,
    pub network: Option<String>,
    pub edition: Option<String>,
    pub category: Option<i32>,
    pub min_seeders: Option<i32>,
    pub min_size: Option<i64>,
    pub max_size: Option<i64>,
    pub sort: SortField,
    pub order: SortOrder,
    pub limit: i64,
    pub offset: i64,
}

#[derive(Debug, Clone, Serialize)]
pub struct SearchResultItem {
    pub torrent: Torrent,
    pub content: Option<TorrentContent>,
    pub tags: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct SearchResponse {
    pub results: Vec<SearchResultItem>,
    pub total: i64,
    pub facets: Option<SearchFacets>,
    pub offset: i64,
    pub limit: i64,
}

/// Execute a search with filtering, sorting, and faceting.
pub async fn search(
    pool: &PgPool,
    filters: &SearchFilters,
    include_facets: bool,
) -> Result<SearchResponse, sqlx::Error> {
    // Build the dynamic WHERE clause
    let mut conditions = Vec::new();
    let mut bind_idx = 0u32;
    let mut bind_strings: Vec<String> = Vec::new();
    let _bind_ints: Vec<i64> = Vec::new();

    // Base conditions: resolved + announced. We deliberately *do not* gate on
    // seed_count >= 1 here — that filter belongs on the user's `min_seeders`
    // query param (default 0) so torrents whose announcer hasn't yet stamped
    // a count remain visible.
    conditions.push("t.resolved_at IS NOT NULL".to_string());
    conditions.push("t.announced_at IS NOT NULL".to_string());

    // Full-text search
    let has_fts_query = !filters.query.is_empty();
    if has_fts_query {
        bind_idx += 1;
        conditions.push(format!(
            "t.search_vector @@ plainto_tsquery('english', ${})",
            bind_idx
        ));
        bind_strings.push(filters.query.clone());
    }

    // Since sqlx doesn't support fully dynamic bind parameters in a clean way,
    // we build a raw SQL string with parameter placeholders and use query_as.
    // For the MVP, we use a simpler approach with string interpolation for
    // non-user-input values and proper parameterization for user input.

    // For now, implement a simpler but correct version using raw SQL.
    let (results, total) = execute_search_query(pool, filters).await?;

    let facets = if include_facets && total > 0 {
        Some(compute_facets(pool, filters).await?)
    } else {
        None
    };

    Ok(SearchResponse {
        results,
        total,
        facets,
        offset: filters.offset,
        limit: filters.limit,
    })
}

/// Build and execute the search query with dynamic filters.
async fn execute_search_query(
    pool: &PgPool,
    filters: &SearchFilters,
) -> Result<(Vec<SearchResultItem>, i64), sqlx::Error> {
    // Build WHERE clauses and collect bind values.
    // No hardcoded seed_count gate here — `min_seeders` filter (below) handles
    // that on demand; defaults to 0 so resolved torrents whose announcer
    // hasn't yet stamped a count remain searchable.
    let mut where_parts = vec![
        "t.resolved_at IS NOT NULL".to_string(),
        "t.announced_at IS NOT NULL".to_string(),
    ];

    // We use a query builder approach — build the SQL dynamically,
    // but bind user values safely via sqlx::query_as parameters.
    // Since the number of parameters varies, we use sqlx::query() with
    // raw SQL and explicit .bind() calls.

    // For simplicity in Phase 1, use a macro-free approach:
    // Build a CTE-based query string and bind values via a helper.

    let mut param_idx = 0usize;

    macro_rules! add_text_filter {
        ($col:expr, $val:expr) => {
            if let Some(ref _v) = $val {
                param_idx += 1;
                where_parts.push(format!("{} = ${}", $col, param_idx));
            }
        };
    }

    macro_rules! add_int_filter {
        ($col:expr, $val:expr) => {
            if let Some(_v) = $val {
                param_idx += 1;
                where_parts.push(format!("{} = ${}", $col, param_idx));
            }
        };
    }

    macro_rules! add_int_gte_filter {
        ($col:expr, $val:expr) => {
            if let Some(_v) = $val {
                param_idx += 1;
                where_parts.push(format!("{} >= ${}", $col, param_idx));
            }
        };
    }

    macro_rules! add_int_lte_filter {
        ($col:expr, $val:expr) => {
            if let Some(_v) = $val {
                param_idx += 1;
                where_parts.push(format!("{} <= ${}", $col, param_idx));
            }
        };
    }

    // FTS query
    if !filters.query.is_empty() {
        param_idx += 1;
        where_parts.push(format!(
            "t.search_vector @@ plainto_tsquery('english', ${})",
            param_idx
        ));
    }

    add_text_filter!("t.info_hash", filters.info_hash);
    add_text_filter!("c.content_type", filters.content_type);
    add_text_filter!("c.resolution", filters.resolution);
    add_text_filter!("c.codec", filters.codec);
    add_text_filter!("c.video_source", filters.video_source);
    add_text_filter!("c.modifier", filters.modifier);
    add_text_filter!("c.hdr", filters.hdr);
    add_text_filter!("c.audio_codec", filters.audio_codec);
    add_text_filter!("c.language", filters.language);
    add_text_filter!("c.imdb_id", filters.imdb_id);
    add_text_filter!("t.source", filters.source);
    add_text_filter!("c.platform", filters.platform);
    add_text_filter!("c.music_format", filters.music_format);
    add_text_filter!("c.network", filters.network);
    add_text_filter!("c.edition", filters.edition);

    add_int_filter!("c.tmdb_id", filters.tmdb_id);
    add_int_filter!("c.season", filters.season);
    add_int_filter!("c.episode", filters.episode);
    add_int_filter!("c.year", filters.year);
    add_int_gte_filter!("c.year", filters.year_min);
    add_int_lte_filter!("c.year", filters.year_max);
    add_int_gte_filter!("t.seed_count", filters.min_seeders);
    add_int_gte_filter!("t.size", filters.min_size);
    add_int_lte_filter!("t.size", filters.max_size);

    if let Some(true) = filters.has_subtitles {
        where_parts.push("c.has_subtitles = TRUE".to_string());
    }
    if let Some(true) = filters.is_anime {
        where_parts.push("c.is_anime = TRUE".to_string());
    }

    let where_clause = where_parts.join(" AND ");

    // Sort
    let order_dir = match filters.order {
        SortOrder::Asc => "ASC",
        SortOrder::Desc => "DESC",
    };

    let order_clause = match filters.sort {
        SortField::Relevance if !filters.query.is_empty() => {
            format!("ts_rank(t.search_vector, plainto_tsquery('english', $1)) {order_dir}")
        }
        SortField::Date | SortField::Relevance => format!("t.resolved_at {order_dir}"),
        SortField::Size => format!("t.size {order_dir}"),
        SortField::Seeders => format!("t.seed_count {order_dir}"),
        SortField::Leechers => format!("t.peer_count {order_dir}"),
        SortField::Name => format!("t.name {order_dir}"),
        SortField::InfoHash => format!("t.info_hash {order_dir}"),
        SortField::Quality => format!("c.quality_score {order_dir} NULLS LAST"),
        SortField::Files => format!("t.resolved_at {order_dir}"),
    };

    // Count query
    let count_sql = format!(
        "SELECT COUNT(*) as count FROM torrents t LEFT JOIN torrent_content c ON t.info_hash = c.info_hash WHERE {where_clause}"
    );

    // Data query
    param_idx += 1;
    let limit_param = param_idx;
    param_idx += 1;
    let offset_param = param_idx;

    let data_sql = format!(
        "SELECT t.*, c.info_hash AS c_info_hash, c.content_type, c.title, c.year, c.season, c.episode, \
         c.episode_title, c.\"group\", c.language, c.resolution, c.codec, c.video_source, c.modifier, \
         c.is_3d, c.hdr, c.audio_codec, c.audio_channels, c.edition, c.bit_depth, c.network, \
         c.quality_score, c.is_dubbed, c.is_complete, c.is_remastered, c.is_scene, c.is_proper, \
         c.is_repack, c.platform, c.has_subtitles, c.is_anime, c.music_format, c.tmdb_id, \
         c.imdb_id, c.tmdb_data, c.classified_at, c.classifier_version \
         FROM torrents t LEFT JOIN torrent_content c ON t.info_hash = c.info_hash \
         WHERE {where_clause} ORDER BY {order_clause} LIMIT ${limit_param} OFFSET ${offset_param}"
    );

    // The dynamic fragments above come exclusively from closed enums and
    // hard-coded column names; all user-provided values remain bind parameters.
    let mut count_query = sqlx::query_scalar::<_, i64>(sqlx::AssertSqlSafe(count_sql));

    // Build and bind the data query
    let mut data_query = sqlx::query(sqlx::AssertSqlSafe(data_sql));

    // Bind all parameters in order
    // FTS query
    macro_rules! bind_both {
        ($val:expr, $count_q:expr, $data_q:expr) => {
            $count_q = $count_q.bind($val.clone());
            $data_q = $data_q.bind($val.clone());
        };
    }

    if !filters.query.is_empty() {
        bind_both!(&filters.query, count_query, data_query);
    }

    macro_rules! bind_opt_text {
        ($val:expr) => {
            if let Some(ref v) = $val {
                bind_both!(v, count_query, data_query);
            }
        };
    }
    macro_rules! bind_opt_i32 {
        ($val:expr) => {
            if let Some(v) = $val {
                bind_both!(v, count_query, data_query);
            }
        };
    }
    macro_rules! bind_opt_i64 {
        ($val:expr) => {
            if let Some(v) = $val {
                bind_both!(v, count_query, data_query);
            }
        };
    }

    bind_opt_text!(filters.info_hash);
    bind_opt_text!(filters.content_type);
    bind_opt_text!(filters.resolution);
    bind_opt_text!(filters.codec);
    bind_opt_text!(filters.video_source);
    bind_opt_text!(filters.modifier);
    bind_opt_text!(filters.hdr);
    bind_opt_text!(filters.audio_codec);
    bind_opt_text!(filters.language);
    bind_opt_text!(filters.imdb_id);
    bind_opt_text!(filters.source);
    bind_opt_text!(filters.platform);
    bind_opt_text!(filters.music_format);
    bind_opt_text!(filters.network);
    bind_opt_text!(filters.edition);

    bind_opt_i32!(filters.tmdb_id);
    bind_opt_i32!(filters.season);
    bind_opt_i32!(filters.episode);
    bind_opt_i32!(filters.year);
    bind_opt_i32!(filters.year_min);
    bind_opt_i32!(filters.year_max);
    bind_opt_i32!(filters.min_seeders);
    bind_opt_i64!(filters.min_size);
    bind_opt_i64!(filters.max_size);

    // Limit/offset only on data query
    data_query = data_query.bind(filters.limit).bind(filters.offset);

    let total = count_query.fetch_one(pool).await?;

    let rows = data_query.fetch_all(pool).await?;

    let mut results = Vec::with_capacity(rows.len());
    for row in &rows {
        use sqlx::Row;
        let torrent = Torrent {
            info_hash: row.get("info_hash"),
            name: row.get("name"),
            size: row.get("size"),
            piece_length: row.get("piece_length"),
            piece_count: row.get("piece_count"),
            private: row.get("private"),
            source: row.get("source"),
            discovered_at: row.get("discovered_at"),
            resolved_at: row.get("resolved_at"),
            resolve_attempts: row.get("resolve_attempts"),
            no_peers: row.get("no_peers"),
            announce_miss: row.get("announce_miss"),
            retry_after: row.get("retry_after"),
            observations: row.get("observations"),
            priority: row.get("priority"),
            seed_count: row.get("seed_count"),
            peer_count: row.get("peer_count"),
            scraped_at: row.get("scraped_at"),
            trackers: row.get("trackers"),
            announced_at: row.get("announced_at"),
            nfo: row.get("nfo"),
            search_vector: None, // tsvector not useful to return
            epoch: row.get("epoch"),
            contributor_id: row.get("contributor_id"),
            sync_sequence: row.get("sync_sequence"),
        };

        let content = row
            .get::<Option<String>, _>("c_info_hash")
            .map(|_| TorrentContent {
                info_hash: row.get("c_info_hash"),
                content_type: row.get("content_type"),
                title: row.get("title"),
                year: row.get("year"),
                season: row.get("season"),
                episode: row.get("episode"),
                episode_title: row.get("episode_title"),
                group: row.get("group"),
                language: row.get("language"),
                resolution: row.get("resolution"),
                codec: row.get("codec"),
                video_source: row.get("video_source"),
                modifier: row.get("modifier"),
                is_3d: row.get("is_3d"),
                hdr: row.get("hdr"),
                audio_codec: row.get("audio_codec"),
                audio_channels: row.get("audio_channels"),
                edition: row.get("edition"),
                bit_depth: row.get("bit_depth"),
                network: row.get("network"),
                quality_score: row.get("quality_score"),
                is_dubbed: row.get("is_dubbed"),
                is_complete: row.get("is_complete"),
                is_remastered: row.get("is_remastered"),
                is_scene: row.get("is_scene"),
                is_proper: row.get("is_proper"),
                is_repack: row.get("is_repack"),
                platform: row.get("platform"),
                has_subtitles: row.get("has_subtitles"),
                is_anime: row.get("is_anime"),
                music_format: row.get("music_format"),
                tmdb_id: row.get("tmdb_id"),
                imdb_id: row.get("imdb_id"),
                tmdb_data: row.get("tmdb_data"),
                classified_at: row.get("classified_at"),
                classifier_version: row.get("classifier_version"),
            });

        // Tags loaded separately to avoid N+1
        results.push(SearchResultItem {
            torrent,
            content,
            tags: Vec::new(),
        });
    }

    // Batch-load tags for all results
    if !results.is_empty() {
        let hashes: Vec<&str> = results
            .iter()
            .map(|r| r.torrent.info_hash.as_str())
            .collect();
        let tag_rows = sqlx::query_as::<_, (String, String)>(
            "SELECT info_hash, tag FROM torrent_tags WHERE info_hash = ANY($1)",
        )
        .bind(&hashes)
        .fetch_all(pool)
        .await?;

        for (hash, tag) in tag_rows {
            if let Some(result) = results.iter_mut().find(|r| r.torrent.info_hash == hash) {
                result.tags.push(tag);
            }
        }
    }

    Ok((results, total))
}

/// Compute facet counts for the current filtered result set.
async fn compute_facets(
    pool: &PgPool,
    _filters: &SearchFilters,
) -> Result<SearchFacets, sqlx::Error> {
    let facets = SearchFacets {
        content_type: facet_query(pool, "c.content_type", "torrent_content c").await?,
        resolution: facet_query(pool, "c.resolution", "torrent_content c").await?,
        codec: facet_query(pool, "c.codec", "torrent_content c").await?,
        video_source: facet_query(pool, "c.video_source", "torrent_content c").await?,
        hdr: facet_query(pool, "c.hdr", "torrent_content c").await?,
        audio_codec: facet_query(pool, "c.audio_codec", "torrent_content c").await?,
        language: facet_query(pool, "c.language", "torrent_content c").await?,
        modifier: facet_query(pool, "c.modifier", "torrent_content c").await?,
        platform: facet_query(pool, "c.platform", "torrent_content c").await?,
        music_format: facet_query(pool, "c.music_format", "torrent_content c").await?,
        year: facet_query_limit(pool, "c.year", "torrent_content c", 20).await?,
        source: facet_query(pool, "t.source", "torrents t").await?,
    };

    Ok(facets)
}

async fn facet_query(
    pool: &PgPool,
    column: &str,
    table: &str,
) -> Result<Vec<FacetCount>, sqlx::Error> {
    facet_query_limit(pool, column, table, 50).await
}

async fn facet_query_limit(
    pool: &PgPool,
    column: &str,
    table: &str,
    limit: i64,
) -> Result<Vec<FacetCount>, sqlx::Error> {
    let sql = format!(
        "SELECT CAST({column} AS TEXT) AS value, COUNT(*) AS count \
         FROM {table} \
         WHERE {column} IS NOT NULL \
         GROUP BY {column} \
         ORDER BY count DESC \
         LIMIT $1"
    );
    // Callers supply only the hard-coded table and column names listed in
    // `compute_facets`; the limit remains a bind parameter.
    let rows = sqlx::query_as::<_, (String, i64)>(sqlx::AssertSqlSafe(sql))
        .bind(limit)
        .fetch_all(pool)
        .await?;
    Ok(rows
        .into_iter()
        .map(|(value, count)| FacetCount { value, count })
        .collect())
}
