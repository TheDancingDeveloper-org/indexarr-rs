use std::sync::Arc;

use axum::Router;
use axum::extract::{Query, State};
use axum::http::header;
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use chrono::{DateTime, Utc};
use serde::Deserialize;
use sqlx::Row;

use crate::routes::categories::compute_category;
use crate::state::AppState;

#[derive(Debug, Deserialize)]
pub struct TorznabParams {
    #[serde(default = "default_caps")]
    t: String,
    #[serde(default)]
    q: String,
    #[serde(default)]
    #[allow(dead_code)]
    cat: String,
    #[serde(default)]
    season: String,
    #[serde(default)]
    ep: String,
    #[serde(default)]
    imdbid: String,
    #[serde(default)]
    tmdbid: String,
    #[serde(default)]
    apikey: String,
    #[serde(default = "default_limit")]
    limit: i64,
    #[serde(default)]
    offset: i64,
}

fn default_caps() -> String {
    "caps".into()
}
fn default_limit() -> i64 {
    50
}

fn xml_response(xml: &str) -> Response {
    (
        [(header::CONTENT_TYPE, "application/xml")],
        format!("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n{xml}"),
    )
        .into_response()
}

fn error_response(code: i32, description: &str) -> Response {
    xml_response(&format!(
        "<error code=\"{code}\" description=\"{}\"/>",
        quick_xml::escape::escape(description)
    ))
}

fn caps_xml() -> String {
    let mut xml = String::from("<caps>\n");
    xml.push_str("  <server title=\"Indexarr\" version=\"0.1.0\"/>\n");
    xml.push_str("  <limits max=\"100\" default=\"50\"/>\n");
    xml.push_str("  <searching>\n");
    xml.push_str("    <search available=\"yes\" supportedParams=\"q\"/>\n");
    xml.push_str("    <tv-search available=\"yes\" supportedParams=\"q,season,ep,imdbid\"/>\n");
    xml.push_str("    <movie-search available=\"yes\" supportedParams=\"q,imdbid,tmdbid\"/>\n");
    xml.push_str("    <audio-search available=\"yes\" supportedParams=\"q\"/>\n");
    xml.push_str("    <book-search available=\"yes\" supportedParams=\"q\"/>\n");
    xml.push_str("  </searching>\n");
    xml.push_str("  <categories>\n");

    let parents: &[(i32, &str)] = &[
        (1000, "Console"),
        (2000, "Movies"),
        (3000, "Audio"),
        (4000, "PC"),
        (5000, "TV"),
        (6000, "XXX"),
        (7000, "Books"),
        (8000, "Other"),
    ];
    let subcats: &[(i32, &str, i32)] = &[
        (2030, "Movies/SD", 2000),
        (2040, "Movies/HD", 2000),
        (2045, "Movies/UHD", 2000),
        (2050, "Movies/BluRay", 2000),
        (2060, "Movies/3D", 2000),
        (5030, "TV/SD", 5000),
        (5040, "TV/HD", 5000),
        (5045, "TV/UHD", 5000),
        (5070, "TV/Anime", 5000),
        (3010, "Audio/Lossy", 3000),
        (3040, "Audio/Lossless", 3000),
        (3030, "Audio/Audiobook", 3000),
        (4050, "PC/Games", 4000),
        (4010, "PC/Software", 4000),
        (4030, "PC/Mac", 4000),
        (1080, "Console/PS", 1000),
        (1010, "Console/Xbox", 1000),
        (1030, "Console/Switch", 1000),
        (7010, "Books/Ebook", 7000),
        (7020, "Books/Comics", 7000),
    ];

    for (id, name) in parents {
        xml.push_str(&format!("    <category id=\"{id}\" name=\"{name}\">\n"));
        for (sid, sname, pid) in subcats {
            if pid == id {
                xml.push_str(&format!("      <subcat id=\"{sid}\" name=\"{sname}\"/>\n"));
            }
        }
        xml.push_str("    </category>\n");
    }

    xml.push_str("  </categories>\n");
    xml.push_str("</caps>");
    xml
}

async fn torznab_api(
    State(state): State<Arc<AppState>>,
    Query(params): Query<TorznabParams>,
) -> Response {
    // Auth check
    if !state.check_api_key(&params.apikey) {
        return error_response(100, "Incorrect API key");
    }

    if params.t == "caps" {
        return xml_response(&caps_xml());
    }

    if !matches!(
        params.t.as_str(),
        "search" | "tvsearch" | "movie" | "music" | "book"
    ) {
        return error_response(202, &format!("No such function: {}", params.t));
    }

    match do_search(&state, &params).await {
        Ok(xml) => xml_response(&xml),
        Err(e) => {
            tracing::error!(error = %e, "torznab search failed");
            error_response(900, "Internal error")
        }
    }
}

async fn do_search(state: &AppState, params: &TorznabParams) -> Result<String, sqlx::Error> {
    let pool = &state.pool;

    let mut conditions = vec![
        "t.resolved_at IS NOT NULL".to_string(),
        "t.announced_at IS NOT NULL".to_string(),
        "t.seed_count >= 1".to_string(),
    ];

    let mut bind_strings: Vec<String> = Vec::new();
    let mut bind_ints: Vec<i32> = Vec::new();
    let mut param_idx = 0usize;

    // Text search
    if !params.q.is_empty() {
        param_idx += 1;
        conditions.push(format!(
            "t.search_vector @@ plainto_tsquery('english', ${})",
            param_idx
        ));
        bind_strings.push(params.q.clone());
    }

    // Season/Episode
    if !params.season.is_empty()
        && let Ok(s) = params.season.parse::<i32>()
    {
        param_idx += 1;
        conditions.push(format!("c.season = ${}", param_idx));
        bind_ints.push(s);
    }
    if !params.ep.is_empty()
        && let Ok(e) = params.ep.parse::<i32>()
    {
        param_idx += 1;
        conditions.push(format!("c.episode = ${}", param_idx));
        bind_ints.push(e);
    }

    // IMDB
    if !params.imdbid.is_empty() {
        param_idx += 1;
        let imdb = if params.imdbid.starts_with("tt") {
            params.imdbid.clone()
        } else {
            format!("tt{}", params.imdbid)
        };
        conditions.push(format!("c.imdb_id = ${}", param_idx));
        bind_strings.push(imdb);
    }

    // TMDB
    if !params.tmdbid.is_empty()
        && let Ok(id) = params.tmdbid.parse::<i32>()
    {
        param_idx += 1;
        conditions.push(format!("c.tmdb_id = ${}", param_idx));
        bind_ints.push(id);
    }

    // Function-specific type filters
    match params.t.as_str() {
        "tvsearch" => conditions.push("c.content_type = 'tv_show'".into()),
        "movie" => conditions.push("c.content_type = 'movie'".into()),
        "music" => conditions.push("c.content_type IN ('music', 'audiobook')".into()),
        "book" => conditions.push("c.content_type IN ('ebook', 'comic')".into()),
        _ => {}
    }

    let where_clause = conditions.join(" AND ");
    let limit = params.limit.clamp(1, 100);
    let offset = params.offset.max(0);

    // Build a simpler query that avoids complex dynamic binding
    // For Torznab, we use a straightforward approach
    let sql = format!(
        "SELECT t.info_hash, t.name, t.size, t.seed_count, t.peer_count, t.resolved_at, t.discovered_at, \
         c.content_type, c.resolution, c.codec, c.audio_codec, c.video_source, \
         c.season, c.episode, c.year, c.imdb_id, c.tmdb_id, c.platform, c.is_3d, c.is_anime, c.music_format \
         FROM torrents t LEFT JOIN torrent_content c ON t.info_hash = c.info_hash \
         WHERE {where_clause} \
         ORDER BY t.resolved_at DESC LIMIT {limit} OFFSET {offset}"
    );

    // Conditions are selected from hard-coded fragments above; user values
    // continue to use bind parameters, and limit/offset are clamped integers.
    let mut query = sqlx::query(sqlx::AssertSqlSafe(sql));

    // Bind in order: strings first, then ints (matching how we built the params)
    // This is a simplified approach — for production we'd use a proper query builder
    for s in &bind_strings {
        query = query.bind(s);
    }
    for i in &bind_ints {
        query = query.bind(i);
    }

    let rows = query.fetch_all(pool).await?;

    // Build RSS XML
    let mut xml = String::from(
        "<rss version=\"2.0\" xmlns:torznab=\"http://torznab.com/schemas/2015/feed\">\n<channel>\n",
    );
    xml.push_str("  <title>Indexarr</title>\n");
    xml.push_str("  <description>Indexarr Torznab Feed</description>\n");
    xml.push_str(&format!(
        "  <response offset=\"0\" total=\"{}\"/>\n",
        rows.len()
    ));

    for row in &rows {
        let info_hash: String = row.get("info_hash");
        let name: String = row
            .get::<Option<String>, _>("name")
            .unwrap_or_else(|| info_hash.clone());
        let size: i64 = row.get::<Option<i64>, _>("size").unwrap_or(0);
        let seed_count: i32 = row.get::<Option<i32>, _>("seed_count").unwrap_or(0);
        let peer_count: i32 = row.get::<Option<i32>, _>("peer_count").unwrap_or(0);

        let escaped_name = quick_xml::escape::escape(&name);

        let magnet = format!(
            "magnet:?xt=urn:btih:{info_hash}&amp;dn={escaped_name}\
             &amp;tr=udp://tracker.opentrackr.org:1337/announce\
             &amp;tr=udp://open.stealth.si:80/announce\
             &amp;tr=udp://tracker.openbittorrent.com:6969/announce"
        );

        let pub_date = row
            .get::<Option<DateTime<Utc>>, _>("resolved_at")
            .or_else(|| row.get::<Option<DateTime<Utc>>, _>("discovered_at"))
            .map(|d| d.format("%a, %d %b %Y %H:%M:%S %z").to_string())
            .unwrap_or_default();

        let content_type: Option<String> = row.get("content_type");
        let resolution: Option<String> = row.get("resolution");
        let platform: Option<String> = row.get("platform");
        let is_3d: bool = row.get::<Option<bool>, _>("is_3d").unwrap_or(false);
        let is_anime: bool = row.get::<Option<bool>, _>("is_anime").unwrap_or(false);
        let music_format: Option<String> = row.get("music_format");
        let video_source: Option<String> = row.get("video_source");

        let cat_id = compute_category(
            content_type.as_deref(),
            resolution.as_deref(),
            platform.as_deref(),
            is_3d,
            is_anime,
            music_format.as_deref(),
            video_source.as_deref(),
        );

        xml.push_str("  <item>\n");
        xml.push_str(&format!("    <title>{escaped_name}</title>\n"));
        xml.push_str(&format!("    <guid>{info_hash}</guid>\n"));
        xml.push_str("    <jackettindexer>Indexarr</jackettindexer>\n");
        xml.push_str(&format!("    <link>{magnet}</link>\n"));
        xml.push_str(&format!(
            "    <enclosure url=\"{magnet}\" length=\"{size}\" type=\"application/x-bittorrent;x-scheme-handler/magnet\"/>\n"
        ));
        xml.push_str(&format!("    <pubDate>{pub_date}</pubDate>\n"));
        xml.push_str(&format!("    <size>{size}</size>\n"));
        xml.push_str(&format!("    <category>{cat_id}</category>\n"));

        // Torznab attributes
        let ns = "torznab";
        xml.push_str(&format!(
            "    <{ns}:attr name=\"category\" value=\"{cat_id}\"/>\n"
        ));
        xml.push_str(&format!(
            "    <{ns}:attr name=\"size\" value=\"{size}\"/>\n"
        ));
        xml.push_str(&format!(
            "    <{ns}:attr name=\"seeders\" value=\"{seed_count}\"/>\n"
        ));
        xml.push_str(&format!(
            "    <{ns}:attr name=\"peers\" value=\"{peer_count}\"/>\n"
        ));
        xml.push_str(&format!(
            "    <{ns}:attr name=\"infohash\" value=\"{info_hash}\"/>\n"
        ));
        xml.push_str(&format!(
            "    <{ns}:attr name=\"magneturl\" value=\"{magnet}\"/>\n"
        ));

        if let Some(ref imdb) = row.get::<Option<String>, _>("imdb_id") {
            let imdb_num = imdb.replace("tt", "");
            xml.push_str(&format!(
                "    <{ns}:attr name=\"imdb\" value=\"{imdb_num}\"/>\n"
            ));
        }
        if let Some(tmdb) = row.get::<Option<i32>, _>("tmdb_id") {
            xml.push_str(&format!(
                "    <{ns}:attr name=\"tmdbid\" value=\"{tmdb}\"/>\n"
            ));
        }
        if let Some(season) = row.get::<Option<i32>, _>("season") {
            xml.push_str(&format!(
                "    <{ns}:attr name=\"season\" value=\"{season}\"/>\n"
            ));
        }
        if let Some(episode) = row.get::<Option<i32>, _>("episode") {
            xml.push_str(&format!(
                "    <{ns}:attr name=\"episode\" value=\"{episode}\"/>\n"
            ));
        }
        if let Some(year) = row.get::<Option<i32>, _>("year") {
            xml.push_str(&format!(
                "    <{ns}:attr name=\"year\" value=\"{year}\"/>\n"
            ));
        }
        if let Some(ref res) = resolution {
            xml.push_str(&format!(
                "    <{ns}:attr name=\"resolution\" value=\"{res}\"/>\n"
            ));
        }
        if let Some(ref codec) = row.get::<Option<String>, _>("codec") {
            xml.push_str(&format!(
                "    <{ns}:attr name=\"video\" value=\"{codec}\"/>\n"
            ));
        }
        if let Some(ref audio) = row.get::<Option<String>, _>("audio_codec") {
            xml.push_str(&format!(
                "    <{ns}:attr name=\"audio\" value=\"{audio}\"/>\n"
            ));
        }

        xml.push_str("  </item>\n");
    }

    xml.push_str("</channel>\n</rss>");
    Ok(xml)
}

pub fn router(_state: Arc<AppState>) -> Router<Arc<AppState>> {
    Router::new().route("/", get(torznab_api))
}
