mod llm;
mod vocabulary;
mod vocabulary_quiz;
mod word_graph;

use llm::LlmManager;
use reqwest::Client;
use rusqlite::{Connection, OpenFlags, OptionalExtension, Row};
use scraper::{Html, Selector};
use serde::Serialize;
use std::{
    cmp::Ordering,
    collections::HashSet,
    fs::{self, File},
    io,
    path::{Path, PathBuf},
    sync::OnceLock,
    time::Duration,
};
use tauri::{AppHandle, Manager};
use zip::ZipArchive;

const DATABASE_FILE: &str = "aurora-dictionary.sqlite3";
const BUNDLED_DICTIONARY_ARCHIVE: &str = "resources/ecdict-sqlite-28.zip";
const BUNDLED_SOURCE_FILE: &str = ".ecdict-source.sqlite3";
const COMPLETE_DICTIONARY_MINIMUM_ENTRIES: i64 = 100_000;
const USER_AGENT: &str = "AuroraDictionary/0.1 (desktop dictionary; contact: local-app)";

// Installation validation requires opening the database and counting its rows.
// Do that once per application process rather than once for every keystroke.
static BUNDLED_DICTIONARY_READY: OnceLock<PathBuf> = OnceLock::new();
static WORD_LOOKUP_INDEX_READY: OnceLock<()> = OnceLock::new();

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct DictionaryEntry {
    word: String,
    phonetic: Option<String>,
    uk_phonetic: Option<String>,
    us_phonetic: Option<String>,
    uk_audio: Option<String>,
    us_audio: Option<String>,
    translation: Option<String>,
    definition: Option<String>,
    pos: Option<String>,
    exchange: Option<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct LocalLookup {
    query: String,
    entries: Vec<DictionaryEntry>,
    suggestions: Vec<String>,
    sample_data: bool,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct LocalSuggestions {
    suggestions: Vec<String>,
    correction: bool,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct DictionaryStatus {
    entry_count: i64,
    sample_data: bool,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct OnlineSense {
    part_of_speech: String,
    definitions: Vec<String>,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct OnlineExample {
    english: String,
    translation: Option<String>,
    source: Option<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct OnlinePhrase {
    term: String,
    translation: Option<String>,
}

/// A source-specific part of an online result.  Most providers only expose one
/// structured view, while Youdao exposes both its concise and Collins views.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct OnlineContentSection {
    id: String,
    senses: Vec<OnlineSense>,
    examples: Vec<OnlineExample>,
    phrases: Vec<OnlinePhrase>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct OnlineLookup {
    source: String,
    word: String,
    pronunciation: Option<String>,
    uk_phonetic: Option<String>,
    us_phonetic: Option<String>,
    uk_audio: Option<String>,
    us_audio: Option<String>,
    senses: Vec<OnlineSense>,
    examples: Vec<OnlineExample>,
    sections: Vec<OnlineContentSection>,
    note: Option<String>,
    source_url: String,
}

fn database_path(app: &AppHandle) -> Result<PathBuf, String> {
    let dir = app
        .path()
        .app_data_dir()
        .map_err(|error| format!("无法取得应用数据目录：{error}"))?;
    fs::create_dir_all(&dir).map_err(|error| format!("无法创建应用数据目录：{error}"))?;
    Ok(dir.join(DATABASE_FILE))
}

fn database(app: &AppHandle) -> Result<Connection, String> {
    ensure_bundled_dictionary(app)?;
    let path = database_path(app)?;
    let connection =
        Connection::open(path).map_err(|error| format!("无法打开本地词库：{error}"))?;
    connection
        .busy_timeout(Duration::from_millis(1_200))
        .map_err(|error| format!("无法设置本地词库等待时间：{error}"))?;
    connection
        .execute_batch("PRAGMA journal_mode = WAL; PRAGMA synchronous = NORMAL;")
        .map_err(|error| format!("无法初始化本地词库：{error}"))?;
    if WORD_LOOKUP_INDEX_READY.get().is_none() {
        connection
            .execute_batch(
                "CREATE INDEX IF NOT EXISTS idx_stardict_word_nocase
                 ON stardict(word COLLATE NOCASE);",
            )
            .map_err(|error| format!("无法初始化输入建议索引：{error}"))?;
        let _ = WORD_LOOKUP_INDEX_READY.set(());
    }
    Ok(connection)
}

fn create_search_index(connection: &Connection) -> Result<(), String> {
    connection
        .execute_batch(
            "
            CREATE VIRTUAL TABLE IF NOT EXISTS dictionary_fts USING fts5(
                word, translation, definition,
                content='stardict', content_rowid='id', tokenize='unicode61'
            );
            ",
        )
        .map_err(|error| format!("无法建立词库检索索引：{error}"))
}

fn rebuild_search_index(connection: &Connection) -> Result<(), String> {
    connection
        .execute(
            "INSERT INTO dictionary_fts(dictionary_fts) VALUES('rebuild')",
            [],
        )
        .map_err(|error| format!("无法更新词库检索索引：{error}"))?;
    Ok(())
}

fn entry_from_row(row: &Row<'_>) -> rusqlite::Result<DictionaryEntry> {
    let phonetic: Option<String> = row.get(1)?;
    let (uk_phonetic, us_phonetic) = local_pronunciations(phonetic.as_deref());
    Ok(DictionaryEntry {
        word: row.get(0)?,
        phonetic,
        uk_phonetic,
        us_phonetic,
        uk_audio: None,
        us_audio: None,
        translation: row.get(2)?,
        definition: row.get(3)?,
        pos: row.get(4)?,
        exchange: row.get(5)?,
    })
}

fn select_columns() -> &'static str {
    "word, phonetic, translation, definition, pos, exchange"
}

fn contains_chinese(input: &str) -> bool {
    input
        .chars()
        .any(|character| ('\u{4e00}'..='\u{9fff}').contains(&character))
}

fn normalise_query(query: &str) -> String {
    query.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn local_pronunciations(phonetic: Option<&str>) -> (Option<String>, Option<String>) {
    let shared = phonetic
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string);
    // ECDICT keeps one IPA field for most entries. It is still useful to expose both
    // controls: the front end asks the operating system for an en-GB or en-US voice.
    (shared.clone(), shared)
}

#[tauri::command]
fn lookup_local(app: AppHandle, query: String) -> Result<LocalLookup, String> {
    let connection = database(&app)?;
    let query = normalise_query(&query);
    if query.is_empty() {
        return Ok(LocalLookup {
            query,
            entries: Vec::new(),
            suggestions: Vec::new(),
            sample_data: sample_data(&connection)?,
        });
    }

    let entries = if contains_chinese(&query) {
        search_chinese_entries(&connection, &query)?
    } else {
        let exact_sql = format!(
            "SELECT {} FROM stardict WHERE lower(word) = lower(?1) LIMIT 1",
            select_columns()
        );
        let exact = connection
            .query_row(&exact_sql, [&query], entry_from_row)
            .optional()
            .map_err(|error| format!("无法检索本地词典：{error}"))?;
        if let Some(entry) = exact {
            vec![entry]
        } else {
            let prefix_sql = format!(
                "SELECT {} FROM stardict
                 WHERE word LIKE ?1 COLLATE NOCASE
                 ORDER BY frq DESC, word COLLATE NOCASE LIMIT 24",
                select_columns()
            );
            let mut statement = connection
                .prepare(&prefix_sql)
                .map_err(|error| format!("无法检索本地词典：{error}"))?;
            let prefix_matches = statement
                .query_map([format!("{query}%")], entry_from_row)
                .map_err(|error| format!("无法读取本地词典：{error}"))?
                .collect::<Result<Vec<_>, _>>()
                .map_err(|error| format!("无法读取本地词典：{error}"))?;
            prefix_matches
        }
    };

    let suggestions = if entries.is_empty() && !contains_chinese(&query) {
        spelling_suggestions(&connection, &query)?
    } else {
        Vec::new()
    };

    Ok(LocalLookup {
        query,
        entries,
        suggestions,
        sample_data: sample_data(&connection)?,
    })
}

/// Fast type-ahead lookup used while a user is still composing an English
/// query.  Prefixes keep the usual flow effortless; when the prefix has no
/// match, the existing edit-distance ranking supplies spelling corrections.
#[tauri::command]
fn suggest_local_words(app: AppHandle, query: String) -> Result<LocalSuggestions, String> {
    let query = normalise_query(&query);
    if query.chars().count() < 2 || contains_chinese(&query) {
        return Ok(LocalSuggestions {
            suggestions: Vec::new(),
            correction: false,
        });
    }

    let connection = database(&app)?;
    autocomplete_suggestions(&connection, &query)
}

fn autocomplete_suggestions(
    connection: &Connection,
    query: &str,
) -> Result<LocalSuggestions, String> {
    let exact_match = connection
        .query_row(
            "SELECT 1 FROM stardict WHERE lower(word) = lower(?1) LIMIT 1",
            [&query],
            |_| Ok(()),
        )
        .optional()
        .map_err(|error| format!("无法检查输入词条：{error}"))?
        .is_some();
    let mut statement = connection
        .prepare(
            "SELECT word FROM stardict
             WHERE word LIKE ?1 COLLATE NOCASE AND lower(word) <> lower(?2)
             ORDER BY frq DESC, word COLLATE NOCASE LIMIT 10",
        )
        .map_err(|error| format!("无法生成输入建议：{error}"))?;
    let prefix_matches = statement
        .query_map((format!("{query}%"), query), |row| row.get::<_, String>(0))
        .map_err(|error| format!("无法读取输入建议：{error}"))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| format!("无法读取输入建议：{error}"))?;

    if !prefix_matches.is_empty() {
        return Ok(LocalSuggestions {
            suggestions: prefix_matches,
            correction: false,
        });
    }

    if exact_match {
        return Ok(LocalSuggestions {
            suggestions: Vec::new(),
            correction: false,
        });
    }

    Ok(LocalSuggestions {
        suggestions: spelling_suggestions(&connection, &query)?,
        correction: true,
    })
}

/// Prefer FTS5 for the normal CJK query path. The LIKE fallback deliberately
/// remains for short/infix phrases, because dictionary source text may join CJK
/// characters into a longer FTS token.
fn search_chinese_entries(
    connection: &Connection,
    query: &str,
) -> Result<Vec<DictionaryEntry>, String> {
    let safe_query = query.replace('"', " ");
    let fts_query = format!("\"{safe_query}\"*");
    let fts_sql = format!(
        "SELECT stardict.{} FROM dictionary_fts
         JOIN stardict ON stardict.id = dictionary_fts.rowid
         WHERE dictionary_fts MATCH ?1
         ORDER BY bm25(dictionary_fts), stardict.frq DESC LIMIT 36",
        select_columns().replace(", ", ", stardict.")
    );
    let mut fts_statement = connection
        .prepare(&fts_sql)
        .map_err(|error| format!("无法检索本地词典索引：{error}"))?;
    let fts_results = fts_statement
        .query_map([fts_query], entry_from_row)
        .map_err(|error| format!("无法读取本地词典索引：{error}"))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| format!("无法读取本地词典索引：{error}"))?;
    if !fts_results.is_empty() {
        return Ok(fts_results);
    }

    let pattern = format!("%{query}%");
    let like_sql = format!(
        "SELECT {} FROM stardict
         WHERE translation LIKE ?1 OR definition LIKE ?1
         ORDER BY frq DESC, word COLLATE NOCASE LIMIT 36",
        select_columns()
    );
    let mut like_statement = connection
        .prepare(&like_sql)
        .map_err(|error| format!("无法检索本地词典：{error}"))?;
    let like_results = like_statement
        .query_map([pattern], entry_from_row)
        .map_err(|error| format!("无法读取本地词典：{error}"))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| format!("无法读取本地词典：{error}"))?;
    Ok(like_results)
}

fn sample_data(connection: &Connection) -> Result<bool, String> {
    let count: i64 = connection
        .query_row("SELECT COUNT(*) FROM stardict", [], |row| row.get(0))
        .map_err(|error| format!("无法读取词库状态：{error}"))?;
    Ok(count <= 5)
}

fn spelling_suggestions(connection: &Connection, query: &str) -> Result<Vec<String>, String> {
    let Some(first) = query.chars().next() else {
        return Ok(Vec::new());
    };
    let mut statement = connection
        .prepare(
            "SELECT word FROM stardict
             WHERE word LIKE ?1 COLLATE NOCASE
             ORDER BY frq DESC, word COLLATE NOCASE LIMIT 240",
        )
        .map_err(|error| format!("无法生成拼写建议：{error}"))?;
    let candidates = statement
        .query_map([format!("{first}%")], |row| row.get::<_, String>(0))
        .map_err(|error| format!("无法读取拼写建议：{error}"))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| format!("无法读取拼写建议：{error}"))?;

    let maximum_distance = if query.len() <= 4 { 2 } else { 3 };
    let mut ranked = candidates
        .into_iter()
        .map(|word| {
            let distance = levenshtein(&word.to_lowercase(), &query.to_lowercase());
            (word, distance)
        })
        .filter(|(_, distance)| *distance <= maximum_distance)
        .collect::<Vec<_>>();
    ranked.sort_by(|left, right| match left.1.cmp(&right.1) {
        Ordering::Equal => left.0.cmp(&right.0),
        order => order,
    });
    Ok(ranked.into_iter().take(10).map(|(word, _)| word).collect())
}

fn levenshtein(left: &str, right: &str) -> usize {
    let right_chars = right.chars().collect::<Vec<_>>();
    let mut previous = (0..=right_chars.len()).collect::<Vec<_>>();
    for (left_index, left_char) in left.chars().enumerate() {
        let mut current = vec![left_index + 1];
        for (right_index, right_char) in right_chars.iter().enumerate() {
            let replacement_cost = usize::from(left_char != *right_char);
            current.push(
                (previous[right_index + 1] + 1)
                    .min(current[right_index] + 1)
                    .min(previous[right_index] + replacement_cost),
            );
        }
        previous = current;
    }
    previous[right_chars.len()]
}

#[tauri::command]
fn dictionary_status(app: AppHandle) -> Result<DictionaryStatus, String> {
    let connection = database(&app)?;
    let entry_count: i64 = connection
        .query_row("SELECT COUNT(*) FROM stardict", [], |row| row.get(0))
        .map_err(|error| format!("无法读取词库状态：{error}"))?;
    Ok(DictionaryStatus {
        entry_count,
        sample_data: entry_count <= 5,
    })
}

fn complete_dictionary_exists(path: &Path) -> bool {
    if !path.is_file() {
        return false;
    }
    let Ok(connection) = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY) else {
        return false;
    };
    let entry_count = connection
        .query_row("SELECT COUNT(*) FROM stardict", [], |row| {
            row.get::<_, i64>(0)
        })
        .unwrap_or_default();
    let has_search_index = connection
        .query_row(
            "SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'dictionary_fts' LIMIT 1",
            [],
            |_| Ok(()),
        )
        .is_ok();
    entry_count >= COMPLETE_DICTIONARY_MINIMUM_ENTRIES && has_search_index
}

fn bundled_dictionary_archive(app: &AppHandle) -> Result<PathBuf, String> {
    let resource_dir = app
        .path()
        .resource_dir()
        .map_err(|error| format!("无法取得应用资源目录：{error}"))?;
    let packaged_path = resource_dir.join(BUNDLED_DICTIONARY_ARCHIVE);
    let flat_packaged_path = resource_dir.join("ecdict-sqlite-28.zip");
    let development_path = Path::new(env!("CARGO_MANIFEST_DIR")).join(BUNDLED_DICTIONARY_ARCHIVE);
    [packaged_path, flat_packaged_path, development_path]
        .into_iter()
        .find(|path| path.is_file())
        .ok_or_else(|| "安装包中缺少默认 ECDICT SQLite 资源。请重新安装应用。".into())
}

fn unpack_bundled_dictionary(archive_path: &Path, staging_path: &Path) -> Result<(), String> {
    let archive_file =
        File::open(archive_path).map_err(|error| format!("无法打开内置 ECDICT 资源：{error}"))?;
    let mut archive = ZipArchive::new(archive_file)
        .map_err(|error| format!("无法读取内置 ECDICT 压缩包：{error}"))?;
    let mut source_index = None;
    for index in 0..archive.len() {
        let entry = archive
            .by_index(index)
            .map_err(|error| format!("无法读取内置 ECDICT 文件：{error}"))?;
        let extension = Path::new(entry.name())
            .extension()
            .and_then(|extension| extension.to_str())
            .map(str::to_ascii_lowercase);
        if !entry.is_dir()
            && matches!(
                extension.as_deref(),
                Some("sqlite") | Some("sqlite3") | Some("db")
            )
        {
            source_index = Some(index);
            break;
        }
    }
    let source_index =
        source_index.ok_or_else(|| "内置 ECDICT 压缩包中没有 SQLite 数据库。".to_string())?;
    let mut source = archive
        .by_index(source_index)
        .map_err(|error| format!("无法解压内置 ECDICT 数据库：{error}"))?;
    let mut destination =
        File::create(staging_path).map_err(|error| format!("无法创建离线词库临时文件：{error}"))?;
    io::copy(&mut source, &mut destination)
        .map_err(|error| format!("无法解压内置 ECDICT 数据库：{error}"))?;
    destination
        .sync_all()
        .map_err(|error| format!("无法写入离线词库临时文件：{error}"))?;
    Ok(())
}

fn ensure_bundled_dictionary(app: &AppHandle) -> Result<(), String> {
    let destination_path = database_path(app)?;
    if BUNDLED_DICTIONARY_READY
        .get()
        .is_some_and(|ready_path| ready_path == &destination_path)
    {
        return Ok(());
    }
    if complete_dictionary_exists(&destination_path) {
        let _ = BUNDLED_DICTIONARY_READY.set(destination_path);
        return Ok(());
    }

    let staging_path = destination_path.with_file_name(BUNDLED_SOURCE_FILE);
    let _ = fs::remove_file(&staging_path);
    let install_result = (|| {
        let archive_path = bundled_dictionary_archive(app)?;
        unpack_bundled_dictionary(&archive_path, &staging_path)?;
        prepare_bundled_dictionary(&staging_path)?;
        remove_dictionary_files(&destination_path);
        fs::rename(&staging_path, &destination_path)
            .map_err(|error| format!("无法安装内置 ECDICT 数据库：{error}"))?;
        Ok(())
    })();
    let _ = fs::remove_file(&staging_path);
    if install_result.is_ok() {
        let _ = BUNDLED_DICTIONARY_READY.set(destination_path);
    }
    install_result
}

fn remove_dictionary_files(path: &Path) {
    let _ = fs::remove_file(path);
    let _ = fs::remove_file(format!("{}-wal", path.display()));
    let _ = fs::remove_file(format!("{}-shm", path.display()));
}

fn prepare_bundled_dictionary(path: &Path) -> Result<(), String> {
    let connection = Connection::open(path)
        .map_err(|error| format!("无法打开内置 ECDICT SQLite 文件：{error}"))?;
    let has_stardict: Option<String> = connection
        .query_row(
            "SELECT name FROM sqlite_master WHERE type = 'table' AND name = 'stardict' LIMIT 1",
            [],
            |row| row.get(0),
        )
        .optional()
        .map_err(|error| format!("无法验证 ECDICT SQLite 文件：{error}"))?;
    if has_stardict.is_none() {
        return Err("内置 ECDICT SQLite 文件不包含 stardict 表。".into());
    }

    let entry_count: i64 = connection
        .query_row("SELECT COUNT(*) FROM stardict", [], |row| row.get(0))
        .map_err(|error| format!("无法读取内置 ECDICT 词条：{error}"))?;
    if entry_count < COMPLETE_DICTIONARY_MINIMUM_ENTRIES {
        return Err("内置 ECDICT 数据不完整，无法启用离线词典。".into());
    }
    create_search_index(&connection)?;
    rebuild_search_index(&connection)?;
    Ok(())
}

#[tauri::command]
async fn lookup_online(
    provider: String,
    query: String,
    api_key: Option<String>,
) -> Result<OnlineLookup, String> {
    let query = normalise_query(&query);
    if query.is_empty() {
        return Err("请输入要查询的词或短语。".into());
    }
    if query.chars().count() > 100 {
        return Err("查询内容过长，请输入一个词或短语。".into());
    }
    if matches!(provider.as_str(), "dictionary" | "merriam_webster") && contains_chinese(&query) {
        return Err("该英文词典主要提供英文释义；请改用英文单词查询，或切换至有道词典。".into());
    }

    let encoded = urlencoding::encode(&query);
    match provider.as_str() {
        "youdao" => {
            let source = "有道词典";
            // Youdao uses distinct SSR payloads for English and Chinese input.
            // Requesting the Chinese mode is essential: the English page has no
            // concise-definition module for Chinese-to-English lookups.
            let language = if contains_chinese(&query) { "zh" } else { "en" };
            let url = format!("https://dict.youdao.com/result?word={encoded}&lang={language}");
            let page = fetch_source_page(source, &url).await?;
            let mut result = parse_youdao(&page, &query, &url)?;
            result.source = source.into();
            Ok(result)
        }
        "dictionary" => lookup_dictionary_api(&query).await,
        "merriam_webster" => {
            lookup_merriam_webster(&query, api_key.as_deref().unwrap_or_default()).await
        }
        _ => Err("未知在线词典来源。".into()),
    }
}

async fn fetch_source_page(source: &str, url: &str) -> Result<String, String> {
    let client = Client::builder()
        .user_agent(USER_AGENT)
        .timeout(Duration::from_secs(12))
        .build()
        .map_err(|error| format!("无法建立网络连接：{error}"))?;
    let response = client
        .get(url)
        .send()
        .await
        .map_err(|error| format!("无法访问 {source}：{error}"))?
        .error_for_status()
        .map_err(|error| format!("{source} 暂时无法响应：{error}"))?;
    response
        .text()
        .await
        .map_err(|error| format!("无法读取 {source} 的查询结果：{error}"))
}

/// The initial source selection is a UX hint only: users can always choose a
/// different source afterwards.  Probe the same public host used by the
/// Youdao adapter so an online browser connection does not incorrectly select
/// an unavailable source.
#[tauri::command]
async fn youdao_is_available() -> bool {
    let Ok(client) = Client::builder()
        .user_agent(USER_AGENT)
        .timeout(Duration::from_secs(3))
        .build()
    else {
        return false;
    };
    client.get("https://dict.youdao.com/").send().await.is_ok()
}

// Retry transient failures once; never retry missing words or rate limits.
fn dictionary_status_is_retryable(status: u16) -> bool {
    matches!(status, 408 | 500 | 502 | 503 | 504 | 520..=524)
}

fn dictionary_status_error(status: u16, query: &str) -> String {
    match status {
        404 => format!("Dictionary 未找到“{query}”，请检查拼写；该词典可能未收录此词或短语。"),
        429 => "Dictionary API 请求过于频繁，请稍后再试，或切换至本地查询。".into(),
        408 | 504 | 522 | 524 => format!(
            "Dictionary API 服务响应超时（HTTP {status}），请稍后重试，或切换至本地查询/有道词典。"
        ),
        500..=599 => format!(
            "Dictionary API 服务暂时不可用（HTTP {status}），请稍后重试，或切换至本地查询/有道词典。"
        ),
        _ => format!("Dictionary API 请求失败（HTTP {status}）。请稍后重试。"),
    }
}

fn dictionary_request_error(error: &reqwest::Error) -> String {
    let reason = if error.is_timeout() {
        "连接或读取响应超时"
    } else if error.is_connect() {
        "无法建立连接，请检查网络、DNS 和代理设置"
    } else {
        "网络请求或响应读取失败"
    };
    // Display alone hides the underlying DNS/TLS/socket cause. Preserve that
    // cause without including the requested URL in the top-level error.
    let mut details = Vec::new();
    let mut source = std::error::Error::source(error);
    while let Some(cause) = source {
        let detail = cause.to_string();
        if !details.contains(&detail) {
            details.push(detail);
        }
        source = cause.source();
    }
    let details = if details.is_empty() {
        String::new()
    } else {
        format!("\n详细原因：{}", details.join("；"))
    };
    format!("Dictionary API {reason}。请稍后重试，或切换至本地查询/有道词典。{details}")
}

async fn lookup_dictionary_api(query: &str) -> Result<OnlineLookup, String> {
    let encoded_query = urlencoding::encode(query);
    let api_url = format!("https://freedictionaryapi.com/api/v1/entries/en/{encoded_query}");
    let client = Client::builder()
        .user_agent(USER_AGENT)
        .connect_timeout(Duration::from_secs(5))
        .timeout(Duration::from_secs(12))
        .build()
        .map_err(|error| format!("无法建立 Dictionary API 连接：{error}"))?;
    for attempt in 0..2 {
        if attempt > 0 {
            tokio::time::sleep(Duration::from_millis(500)).await;
        }
        let response = match client
            .get(&api_url)
            .header("Accept", "application/json")
            .send()
            .await
        {
            Ok(response) => response,
            Err(error) => {
                if attempt == 0 && (error.is_timeout() || error.is_connect()) {
                    continue;
                }
                return Err(dictionary_request_error(&error));
            }
        };
        let status = response.status();
        if !status.is_success() {
            if attempt == 0 && dictionary_status_is_retryable(status.as_u16()) {
                continue;
            }
            return Err(dictionary_status_error(status.as_u16(), query));
        }
        let payload = match response.text().await {
            Ok(payload) => payload,
            Err(error) => {
                if attempt == 0 && (error.is_timeout() || error.is_body()) {
                    continue;
                }
                return Err(dictionary_request_error(&error));
            }
        };
        return parse_dictionary_api(&payload, query, &api_url);
    }
    unreachable!("the last request attempt always returns a result")
}

async fn lookup_merriam_webster(query: &str, api_key: &str) -> Result<OnlineLookup, String> {
    let api_key = api_key.trim();
    if api_key.is_empty() {
        return Err("请先在设置中填写 Merriam-Webster API Key。".into());
    }
    if api_key.len() > 128
        || !api_key
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || character == '-')
    {
        return Err("Merriam-Webster API Key 格式无效，请检查后重试。".into());
    }
    let encoded_query = urlencoding::encode(query);
    let api_url =
        format!("https://www.dictionaryapi.com/api/v3/references/collegiate/json/{encoded_query}");
    let client = Client::builder()
        .user_agent(USER_AGENT)
        .timeout(Duration::from_secs(12))
        .build()
        .map_err(|_| "无法建立 Merriam-Webster API 连接。".to_string())?;
    let response = client
        .get(&api_url)
        .query(&[("key", api_key)])
        .send()
        .await
        .map_err(|_| "无法访问 Merriam-Webster API，请检查网络连接。".to_string())?;
    if matches!(response.status().as_u16(), 401 | 403) {
        return Err("Merriam-Webster API Key 无效或没有访问权限。".into());
    }
    let response = response
        .error_for_status()
        .map_err(|_| "Merriam-Webster API 暂时无法响应。".to_string())?;
    let payload = response
        .text()
        .await
        .map_err(|_| "无法读取 Merriam-Webster API 响应。".to_string())?;
    let source_url = format!(
        "https://www.merriam-webster.com/dictionary/{}",
        urlencoding::encode(query)
    );
    parse_merriam_webster_api(&payload, query, &source_url)
}

fn parse_youdao(page: &str, query: &str, url: &str) -> Result<OnlineLookup, String> {
    let document = Html::parse_document(page);
    let word = first_text(&document, &[".word-head h1", "#keyword", ".keyword"])
        .unwrap_or_else(|| query.to_string());
    let phonetics = unique_texts(&document, &[".phonetic", ".pronounce"], 6);
    let (uk_phonetic, us_phonetic) = labelled_phonetics(&phonetics);
    let pronunciation = us_phonetic
        .clone()
        .or_else(|| uk_phonetic.clone())
        .or_else(|| phonetics.first().cloned());
    let is_chinese_query = contains_chinese(query);
    let simple_senses = if is_chinese_query {
        parse_youdao_chinese_translations(page)
    } else {
        parse_youdao_senses(&document)
    };
    let simple_examples = parse_youdao_examples(&document);
    let simple_phrases = parse_youdao_phrases(&document);
    let mut sections = Vec::new();
    if !simple_senses.is_empty() {
        sections.push(OnlineContentSection {
            id: "simple".into(),
            senses: simple_senses.clone(),
            examples: simple_examples.clone(),
            phrases: simple_phrases,
        });
    }

    let collins = (!is_chinese_query).then(|| parse_youdao_collins(page));
    if let Some(collins) = collins.filter(|section| !section.senses.is_empty()) {
        sections.push(collins);
    }

    if sections.is_empty() {
        return Err("有道词典未返回可识别的释义，可能是页面结构或访问策略已更新。".into());
    }
    let fallback_section = &sections[0];
    let encoded_word = urlencoding::encode(&word).into_owned();
    Ok(OnlineLookup {
        source: String::new(),
        word,
        pronunciation,
        uk_phonetic,
        us_phonetic,
        uk_audio: (!is_chinese_query)
            .then(|| format!("http://dict.youdao.com/dictvoice?type=1&audio={encoded_word}")),
        us_audio: (!is_chinese_query)
            .then(|| format!("http://dict.youdao.com/dictvoice?type=0&audio={encoded_word}")),
        senses: fallback_section.senses.clone(),
        examples: fallback_section.examples.clone(),
        sections,
        note: Some("在线内容经结构化提取后呈现；释义以原网页为准。".into()),
        source_url: url.to_string(),
    })
}

fn parse_youdao_senses(document: &Html) -> Vec<OnlineSense> {
    let pos_selector = Selector::parse(".pos").unwrap();
    let translation_selector = Selector::parse(".trans").unwrap();
    for selector_text in [
        ".simple.dict-module .trans-container > ul.basic > li.word-exp",
        ".ec.dict-module .trans-container > ul.basic > li.word-exp",
    ] {
        let Ok(entry_selector) = Selector::parse(selector_text) else {
            continue;
        };
        let senses = document
            .select(&entry_selector)
            .filter_map(|entry| {
                let translation = entry
                    .select(&translation_selector)
                    .next()
                    .map(|element| clean_text(element.text().collect::<Vec<_>>().join(" ")))
                    .filter(|value| !value.is_empty())?;
                let definitions = translation
                    .split(['；', ';', '\n'])
                    .map(str::trim)
                    .filter(|value| !value.is_empty())
                    .map(str::to_string)
                    .collect::<Vec<_>>();
                (!definitions.is_empty()).then(|| OnlineSense {
                    part_of_speech: entry
                        .select(&pos_selector)
                        .next()
                        .map(|element| clean_text(element.text().collect::<Vec<_>>().join(" ")))
                        .filter(|value| !value.is_empty())
                        .unwrap_or_else(|| "简明释义".into()),
                    definitions,
                })
            })
            .collect::<Vec<_>>();
        if !senses.is_empty() {
            return senses;
        }
    }
    Vec::new()
}

/// Chinese queries are server-rendered in Youdao's `web_trans` state rather
/// than the `.simple` DOM module used for English words.  The `value` fields
/// are the concise English candidate translations; summaries are deliberately
/// excluded because they are corpus snippets, not definitions.
fn parse_youdao_chinese_translations(page: &str) -> Vec<OnlineSense> {
    let Some(marker) = page.find("web_trans:") else {
        return Vec::new();
    };
    let object_start = marker + "web_trans:".len();
    let Some(web_trans) = balanced_javascript_segment(page, object_start, b'{', b'}') else {
        return Vec::new();
    };
    let mut seen = HashSet::new();
    let definitions = javascript_string_values(web_trans, "value:")
        .into_iter()
        .map(|value| clean_markup(&value))
        .filter(|value| !value.is_empty() && seen.insert(value.clone()))
        .take(12)
        .collect::<Vec<_>>();
    definitions.is_empty().then(Vec::new).unwrap_or_else(|| {
        vec![OnlineSense {
            part_of_speech: "中译英".into(),
            definitions,
        }]
    })
}

fn parse_youdao_examples(document: &Html) -> Vec<OnlineExample> {
    let entry_selector = Selector::parse(".blng_sents_part.dict-module li.mcols-layout").unwrap();
    let english_selector = Selector::parse(".sen-eng").unwrap();
    let translation_selector = Selector::parse(".sen-ch").unwrap();
    let source_selector = Selector::parse(".secondary").unwrap();
    let mut seen = HashSet::new();
    document
        .select(&entry_selector)
        .filter_map(|entry| {
            let english = entry
                .select(&english_selector)
                .next()
                .map(|element| clean_text(element.text().collect::<Vec<_>>().join(" ")))
                .filter(|value| !value.is_empty())?;
            if !seen.insert(english.clone()) {
                return None;
            }
            let translation = entry
                .select(&translation_selector)
                .next()
                .map(|element| clean_text(element.text().collect::<Vec<_>>().join(" ")))
                .filter(|value| !value.is_empty());
            let source = entry
                .select(&source_selector)
                .next()
                .map(|element| clean_text(element.text().collect::<Vec<_>>().join(" ")))
                .filter(|value| !value.is_empty());
            Some(OnlineExample {
                english,
                translation,
                source,
            })
        })
        .take(24)
        .collect()
}

fn parse_youdao_phrases(document: &Html) -> Vec<OnlinePhrase> {
    let entry_selector = Selector::parse(".webPhrase li.mcols-layout").unwrap();
    let term_selector = Selector::parse("a.point").unwrap();
    let translation_selector = Selector::parse(".sen-phrase").unwrap();
    let mut seen = HashSet::new();
    document
        .select(&entry_selector)
        .filter_map(|entry| {
            let term = entry
                .select(&term_selector)
                .next()
                .map(|element| clean_text(element.text().collect::<Vec<_>>().join(" ")))
                .filter(|value| !value.is_empty())?;
            if !seen.insert(term.clone()) {
                return None;
            }
            let translation = entry
                .select(&translation_selector)
                .next()
                .map(|element| clean_text(element.text().collect::<Vec<_>>().join(" ")))
                .filter(|value| !value.is_empty());
            Some(OnlinePhrase { term, translation })
        })
        .take(24)
        .collect()
}

/// Youdao renders the concise definition as HTML, but its Collins tab is
/// hydrated from the page's server state.  We read that data as text rather
/// than evaluating page JavaScript.  The parser deliberately keeps only the
/// definition and bilingual example fields we render locally.
fn parse_youdao_collins(page: &str) -> OnlineContentSection {
    let Some(marker) = page.find("collins_entries:[") else {
        return empty_youdao_collins();
    };
    let array_start = marker + "collins_entries:".len();
    let Some(collins_data) = balanced_javascript_segment(page, array_start, b'[', b']') else {
        return empty_youdao_collins();
    };

    let definitions = javascript_string_values(collins_data, "tran:")
        .into_iter()
        .map(|value| clean_markup(&value))
        .filter(|value| !value.is_empty())
        .collect::<Vec<_>>();
    let english_examples = javascript_string_values(collins_data, "eng_sent:");
    let chinese_examples = javascript_string_values(collins_data, "chn_sent:");
    let mut examples = Vec::new();
    let mut seen = HashSet::new();
    for (index, english) in english_examples.into_iter().enumerate() {
        let english = clean_markup(&english);
        if english.is_empty() || !seen.insert(english.clone()) {
            continue;
        }
        let translation = chinese_examples
            .get(index)
            .map(|value| clean_markup(value))
            .filter(|value| !value.is_empty());
        examples.push(OnlineExample {
            english,
            translation,
            source: Some("Collins".into()),
        });
    }

    OnlineContentSection {
        id: "collins".into(),
        senses: (!definitions.is_empty())
            .then(|| {
                vec![OnlineSense {
                    part_of_speech: "Collins".into(),
                    definitions,
                }]
            })
            .unwrap_or_default(),
        examples,
        phrases: Vec::new(),
    }
}

fn empty_youdao_collins() -> OnlineContentSection {
    OnlineContentSection {
        id: "collins".into(),
        senses: Vec::new(),
        examples: Vec::new(),
        phrases: Vec::new(),
    }
}

/// Return the bracketed JavaScript array/object beginning at `start`, while
/// correctly skipping nested strings and escape sequences.  This lets the
/// scraper read an SSR state payload without running anything from the page.
fn balanced_javascript_segment(source: &str, start: usize, open: u8, close: u8) -> Option<&str> {
    let bytes = source.as_bytes();
    if bytes.get(start).copied()? != open {
        return None;
    }
    let mut depth = 0usize;
    let mut index = start;
    let mut quote = None;
    while index < bytes.len() {
        let byte = bytes[index];
        if let Some(active_quote) = quote {
            if byte == b'\\' {
                index += 2;
                continue;
            }
            if byte == active_quote {
                quote = None;
            }
        } else if matches!(byte, b'\'' | b'"') {
            quote = Some(byte);
        } else if byte == open {
            depth += 1;
        } else if byte == close {
            depth = depth.checked_sub(1)?;
            if depth == 0 {
                return source.get(start..=index);
            }
        }
        index += 1;
    }
    None
}

/// Collect string-literal values assigned to a key from a small JavaScript
/// state fragment.  Aliased values are intentionally ignored: the direct
/// literals contain the dictionary wording and stay stable across minification.
fn javascript_string_values(source: &str, key: &str) -> Vec<String> {
    let mut values = Vec::new();
    let mut offset = 0usize;
    while let Some(relative) = source.get(offset..).and_then(|slice| slice.find(key)) {
        let value_start = offset + relative + key.len();
        if source.as_bytes().get(value_start) != Some(&b'"') {
            offset = value_start;
            continue;
        }
        let mut index = value_start + 1;
        let bytes = source.as_bytes();
        while index < bytes.len() {
            if bytes[index] == b'\\' {
                index += 2;
                continue;
            }
            if bytes[index] == b'"' {
                if let Some(raw) = source.get(value_start..=index) {
                    if let Ok(value) = serde_json::from_str::<String>(raw) {
                        values.push(value);
                    }
                }
                index += 1;
                break;
            }
            index += 1;
        }
        offset = index;
    }
    values
}

fn clean_markup(value: &str) -> String {
    let fragment = Html::parse_fragment(value);
    clean_text(fragment.root_element().text().collect::<Vec<_>>().join(" "))
}

fn parse_dictionary_api(
    payload: &str,
    query: &str,
    source_url: &str,
) -> Result<OnlineLookup, String> {
    let response: serde_json::Value = serde_json::from_str(payload)
        .map_err(|error| format!("无法解析 Dictionary API 数据：{error}"))?;
    let entries = response
        .get("entries")
        .and_then(|value| value.as_array())
        .ok_or_else(|| format!("Dictionary 未找到“{query}”。"))?;
    if entries.is_empty() {
        return Err(format!("Dictionary 未找到“{query}”。"));
    }
    let word = response
        .get("word")
        .and_then(|value| value.as_str())
        .map(str::to_string)
        .filter(|word| !word.is_empty())
        .unwrap_or_else(|| query.to_string());
    let pronunciations = entries
        .iter()
        .flat_map(|entry| {
            entry
                .get("pronunciations")
                .and_then(|value| value.as_array())
                .into_iter()
                .flatten()
        })
        .collect::<Vec<_>>();
    let phonetic_text = |item: &&serde_json::Value| {
        let is_ipa = item
            .get("type")
            .and_then(|value| value.as_str())
            .map_or(true, |value| value.eq_ignore_ascii_case("ipa"));
        if !is_ipa {
            return None;
        }
        item.get("text")
            .and_then(|value| value.as_str())
            .map(str::to_string)
            .filter(|value| !value.is_empty())
    };
    let has_region = |item: &&serde_json::Value, regions: &[&str]| {
        item.get("tags")
            .and_then(|value| value.as_array())
            .is_some_and(|tags| {
                tags.iter().filter_map(|tag| tag.as_str()).any(|tag| {
                    let tag = tag.to_ascii_lowercase();
                    regions.iter().any(|region| tag.contains(region))
                })
            })
    };
    let fallback_phonetic = pronunciations.iter().find_map(phonetic_text);
    let us_phonetic = pronunciations
        .iter()
        .find(|item| has_region(item, &["american", "canada", "us"]))
        .and_then(phonetic_text)
        .or_else(|| fallback_phonetic.clone());
    let uk_phonetic = pronunciations
        .iter()
        .find(|item| {
            has_region(
                item,
                &["received pronunciation", "british", "uk", "england"],
            )
        })
        .and_then(phonetic_text)
        .or_else(|| fallback_phonetic.clone());

    let mut senses: Vec<OnlineSense> = Vec::new();
    let mut examples = Vec::new();
    let mut seen_examples = HashSet::new();
    for entry in entries {
        let part_of_speech = entry
            .get("partOfSpeech")
            .and_then(|value| value.as_str())
            .filter(|value| !value.is_empty())
            .unwrap_or("English definition")
            .to_string();
        let sense_values = entry
            .get("senses")
            .and_then(|value| value.as_array())
            .cloned()
            .unwrap_or_default();
        let definitions = sense_values
            .iter()
            .filter_map(|sense| sense.get("definition").and_then(|value| value.as_str()))
            .map(str::to_string)
            .filter(|definition| !definition.is_empty())
            .take(8)
            .collect::<Vec<_>>();
        if !definitions.is_empty() {
            if let Some(existing) = senses
                .iter_mut()
                .find(|sense| sense.part_of_speech == part_of_speech)
            {
                for definition in definitions {
                    if existing.definitions.len() >= 8 {
                        break;
                    }
                    if !existing.definitions.contains(&definition) {
                        existing.definitions.push(definition);
                    }
                }
            } else if senses.len() < 6 {
                senses.push(OnlineSense {
                    part_of_speech,
                    definitions,
                });
            }
        }
        for example in sense_values.iter().flat_map(|sense| {
            sense
                .get("examples")
                .and_then(|value| value.as_array())
                .into_iter()
                .flatten()
                .filter_map(|value| value.as_str())
        }) {
            let example = example.trim();
            if !example.is_empty()
                && examples.len() < 10
                && seen_examples.insert(example.to_string())
            {
                examples.push(OnlineExample {
                    english: example.to_string(),
                    translation: None,
                    source: None,
                });
            }
        }
    }
    if senses.is_empty() {
        return Err("Dictionary API 没有返回可展示的释义。".into());
    }
    let source_page = response
        .get("source")
        .and_then(|source| source.get("url"))
        .and_then(|value| value.as_str())
        .filter(|value| !value.is_empty())
        .unwrap_or(source_url)
        .to_string();
    Ok(OnlineLookup {
        source: "FreeDictionaryAPI.com".into(),
        word,
        pronunciation: us_phonetic.clone().or_else(|| uk_phonetic.clone()),
        uk_phonetic,
        us_phonetic,
        uk_audio: None,
        us_audio: None,
        senses,
        examples,
        sections: Vec::new(),
        note: Some("释义来自 English Wiktionary，依据 CC BY-SA 4.0 使用。".into()),
        source_url: source_page,
    })
}

fn parse_merriam_webster_api(
    payload: &str,
    query: &str,
    source_url: &str,
) -> Result<OnlineLookup, String> {
    let response: serde_json::Value = serde_json::from_str(payload)
        .map_err(|error| format!("无法解析 Merriam-Webster API 数据：{error}"))?;
    let items = response
        .as_array()
        .ok_or_else(|| "Merriam-Webster API 返回了无法识别的数据。".to_string())?;
    if items.first().is_some_and(|item| item.is_string()) {
        let suggestions = items
            .iter()
            .filter_map(|item| item.as_str())
            .take(5)
            .collect::<Vec<_>>();
        let suffix = if suggestions.is_empty() {
            String::new()
        } else {
            format!(" 建议：{}。", suggestions.join("、"))
        };
        return Err(format!("Merriam-Webster 未找到“{query}”。{suffix}"));
    }

    let entries = items
        .iter()
        .filter(|item| item.is_object())
        .collect::<Vec<_>>();
    if entries.is_empty() {
        return Err(format!("Merriam-Webster 未找到“{query}”。"));
    }
    let word = entries
        .first()
        .and_then(|entry| entry.get("meta"))
        .and_then(|meta| meta.get("id"))
        .and_then(|value| value.as_str())
        .and_then(|value| value.split(':').next())
        .filter(|value| !value.is_empty())
        .unwrap_or(query)
        .replace('*', "");

    let pronunciation_item = entries.iter().find_map(|entry| {
        entry
            .get("hwi")
            .and_then(|hwi| hwi.get("prs"))
            .and_then(|value| value.as_array())
            .and_then(|items| items.first())
    });
    let pronunciation = pronunciation_item
        .and_then(|item| item.get("mw"))
        .and_then(|value| value.as_str())
        .filter(|value| !value.is_empty())
        .map(|value| format!("/{value}/"));
    let us_audio = pronunciation_item
        .and_then(|item| item.get("sound"))
        .and_then(|sound| sound.get("audio"))
        .and_then(|value| value.as_str())
        .and_then(merriam_webster_audio_url);

    let mut senses: Vec<OnlineSense> = Vec::new();
    for entry in entries {
        let part_of_speech = entry
            .get("fl")
            .and_then(|value| value.as_str())
            .filter(|value| !value.is_empty())
            .unwrap_or("English definition")
            .to_string();
        let definitions = entry
            .get("shortdef")
            .and_then(|value| value.as_array())
            .into_iter()
            .flatten()
            .filter_map(|value| value.as_str())
            .map(str::to_string)
            .filter(|value| !value.is_empty())
            .take(8)
            .collect::<Vec<_>>();
        if definitions.is_empty() {
            continue;
        }
        if let Some(existing) = senses
            .iter_mut()
            .find(|sense| sense.part_of_speech == part_of_speech)
        {
            for definition in definitions {
                if existing.definitions.len() >= 8 {
                    break;
                }
                if !existing.definitions.contains(&definition) {
                    existing.definitions.push(definition);
                }
            }
        } else if senses.len() < 6 {
            senses.push(OnlineSense {
                part_of_speech,
                definitions,
            });
        }
    }
    if senses.is_empty() {
        return Err("Merriam-Webster API 没有返回可展示的释义。".into());
    }

    Ok(OnlineLookup {
        source: "Merriam-Webster".into(),
        word,
        pronunciation: pronunciation.clone(),
        uk_phonetic: None,
        us_phonetic: pronunciation,
        uk_audio: None,
        us_audio,
        senses,
        examples: Vec::new(),
        sections: Vec::new(),
        note: Some("释义与发音来自 Merriam-Webster Collegiate Dictionary API。".into()),
        source_url: source_url.to_string(),
    })
}

fn merriam_webster_audio_url(audio: &str) -> Option<String> {
    let audio = audio.trim();
    if audio.is_empty() {
        return None;
    }
    let lower = audio.to_ascii_lowercase();
    let first = lower.chars().next()?;
    let subdirectory = if lower.starts_with("bix") {
        "bix".to_string()
    } else if lower.starts_with("gg") {
        "gg".to_string()
    } else if first.is_ascii_digit() || !first.is_ascii_alphabetic() {
        "number".to_string()
    } else {
        first.to_string()
    };
    Some(format!(
        "https://media.merriam-webster.com/audio/prons/en/us/mp3/{subdirectory}/{audio}.mp3"
    ))
}

fn labelled_phonetics(values: &[String]) -> (Option<String>, Option<String>) {
    let mut uk = None;
    let mut us = None;
    for value in values {
        let lower = value.to_ascii_lowercase();
        let phonetic = tidy_phonetic(value);
        if lower.contains('英') || lower.contains(" uk") || lower.contains("bre") {
            uk = phonetic.or(uk);
        } else if lower.contains('美') || lower.contains(" us") || lower.contains("ame") {
            us = phonetic.or(us);
        }
    }
    let fallback = values.first().and_then(|value| tidy_phonetic(value));
    (uk.or_else(|| fallback.clone()), us.or(fallback))
}

fn tidy_phonetic(value: &str) -> Option<String> {
    let value = clean_text(value.to_string());
    let value = value
        .trim_start_matches(|character: char| {
            character.is_whitespace() || matches!(character, '英' | '美' | ':' | '：')
        })
        .trim();
    (!value.is_empty()).then(|| value.to_string())
}

fn first_text(document: &Html, selectors: &[&str]) -> Option<String> {
    for selector in selectors {
        let Ok(selector) = Selector::parse(selector) else {
            continue;
        };
        if let Some(element) = document.select(&selector).next() {
            let text = clean_text(element.text().collect::<Vec<_>>().join(" "));
            if !text.is_empty() {
                return Some(text);
            }
        }
    }
    None
}

fn unique_texts(document: &Html, selectors: &[&str], limit: usize) -> Vec<String> {
    let mut output = Vec::new();
    let mut seen = HashSet::new();
    for selector in selectors {
        let Ok(selector) = Selector::parse(selector) else {
            continue;
        };
        for element in document.select(&selector) {
            let text = clean_text(element.text().collect::<Vec<_>>().join(" "));
            if text.len() > 1 && text.len() < 440 && seen.insert(text.clone()) {
                output.push(text);
                if output.len() >= limit {
                    return output;
                }
            }
        }
    }
    output
}

fn clean_text(text: String) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stardict_fts_indexes_the_bundled_source_table() {
        let connection = Connection::open_in_memory().unwrap();
        connection
            .execute_batch(
                "
                CREATE TABLE stardict (
                    id INTEGER PRIMARY KEY,
                    word TEXT,
                    phonetic TEXT,
                    definition TEXT,
                    translation TEXT,
                    pos TEXT,
                    exchange TEXT,
                    frq INTEGER
                );
                INSERT INTO stardict (word, definition, translation)
                VALUES ('serendipity', 'a fortunate discovery', '机缘巧合');
                ",
            )
            .unwrap();
        create_search_index(&connection).unwrap();
        rebuild_search_index(&connection).unwrap();

        let word: String = connection
            .query_row(
                "SELECT stardict.word FROM dictionary_fts
                 JOIN stardict ON stardict.id = dictionary_fts.rowid
                 WHERE dictionary_fts MATCH '机缘巧合*'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(word, "serendipity");
    }

    #[test]
    fn autocomplete_keeps_longer_words_after_an_exact_prefix() {
        let connection = Connection::open_in_memory().unwrap();
        connection
            .execute_batch(
                "
                CREATE TABLE stardict (word TEXT, frq INTEGER);
                INSERT INTO stardict (word, frq) VALUES
                  ('app', 100), ('apple', 90), ('application', 80), ('App', 70);
                ",
            )
            .unwrap();

        let result = autocomplete_suggestions(&connection, "app").unwrap();
        assert_eq!(result.suggestions, ["apple", "application"]);
        assert!(!result.correction);
    }

    #[test]
    fn dictionary_transient_failures_are_distinct_from_missing_words() {
        for status in [408, 500, 502, 503, 504, 520, 522, 524] {
            assert!(dictionary_status_is_retryable(status));
        }
        for status in [200, 400, 401, 403, 404, 429] {
            assert!(!dictionary_status_is_retryable(status));
        }
        assert!(dictionary_status_error(522, "preclude").contains("超时"));
        assert!(!dictionary_status_error(522, "preclude").contains("未找到"));
        assert!(dictionary_status_error(404, "preclude").contains("未找到“preclude”"));
        assert!(dictionary_status_error(429, "preclude").contains("过于频繁"));
        assert!(dictionary_status_error(503, "preclude").contains("服务暂时不可用"));
    }

    #[test]
    fn dictionary_api_parser_keeps_regional_ipa_definitions_examples_and_source() {
        let payload = r#"{
          "word": "serendipity",
          "entries": [{
            "partOfSpeech": "noun",
            "pronunciations": [
              {"type": "ipa", "text": "/ˌsɛɹənˈdɪpəti/", "tags": ["General American"]},
              {"type": "ipa", "text": "/ˌsɛɹənˈdɪpɪti/", "tags": ["Received Pronunciation"]}
            ],
            "senses": [{
              "definition": "A fortunate discovery.",
              "examples": ["It was pure serendipity."]
            }]
          }],
          "source": {
            "url": "https://en.wiktionary.org/wiki/serendipity",
            "license": {"name": "CC BY-SA 4.0"}
          }
        }"#;
        let result = parse_dictionary_api(payload, "serendipity", "https://example.test").unwrap();
        assert_eq!(result.source, "FreeDictionaryAPI.com");
        assert_eq!(result.us_phonetic.as_deref(), Some("/ˌsɛɹənˈdɪpəti/"));
        assert_eq!(result.uk_phonetic.as_deref(), Some("/ˌsɛɹənˈdɪpɪti/"));
        assert!(result.us_audio.is_none());
        assert_eq!(result.senses[0].definitions, ["A fortunate discovery."]);
        assert_eq!(result.examples[0].english, "It was pure serendipity.");
        assert_eq!(
            result.source_url,
            "https://en.wiktionary.org/wiki/serendipity"
        );
    }

    #[test]
    fn merriam_webster_parser_keeps_short_definitions_and_audio() {
        let payload = r#"[
          {
            "meta": {"id": "referral:1"},
            "hwi": {"hw": "re*fer*ral", "prs": [{"mw": "ri-ˈfər-əl", "sound": {"audio": "referr01"}}]},
            "fl": "noun",
            "shortdef": ["the act, action, or an instance of referring", "one that is referred"]
          }
        ]"#;
        let result = parse_merriam_webster_api(
            payload,
            "referral",
            "https://www.merriam-webster.com/dictionary/referral",
        )
        .unwrap();
        assert_eq!(result.source, "Merriam-Webster");
        assert_eq!(result.word, "referral");
        assert_eq!(result.us_phonetic.as_deref(), Some("/ri-ˈfər-əl/"));
        assert_eq!(
            result.us_audio.as_deref(),
            Some("https://media.merriam-webster.com/audio/prons/en/us/mp3/r/referr01.mp3")
        );
        assert_eq!(result.senses[0].part_of_speech, "noun");
        assert_eq!(result.senses[0].definitions.len(), 2);
    }

    #[test]
    fn merriam_webster_parser_reports_spelling_suggestions() {
        let error = parse_merriam_webster_api(
            r#"["referral", "reversal"]"#,
            "refarral",
            "https://example.test",
        )
        .err()
        .unwrap();
        assert!(error.contains("referral"));
        assert!(error.contains("reversal"));
    }

    #[test]
    fn youdao_parser_uses_direct_us_and_uk_audio_urls() {
        let page = r#"
          <h1 class="word-head">serendipity</h1>
          <span class="phonetic">英 /ˌserənˈdɪpəti/</span>
          <span class="phonetic">美 /ˌserənˈdɪpəti/</span>
          <div class="simple dict-module"><div class="trans-container"><ul class="basic">
            <li class="word-exp"><span class="pos">n.</span><span class="trans">机缘巧合；意外发现</span></li>
          </ul></div></div>
          <div class="web_trans dict-module"><div class="trans-container"><li>不应显示的网络释义</li></div></div>
          <div class="blng_sents_part dict-module"><li class="mcols-layout">
            <div class="sen-eng">A fortunate serendipity happened.</div>
            <div class="sen-ch">一次幸运的机缘巧合发生了。</div>
            <div class="secondary">示例来源</div>
          </li></div>
        "#;
        let result = parse_youdao(page, "serendipity", "https://example.test").unwrap();
        assert!(result
            .us_audio
            .unwrap()
            .contains("type=0&audio=serendipity"));
        assert!(result
            .uk_audio
            .unwrap()
            .contains("type=1&audio=serendipity"));
        assert_eq!(result.senses[0].definitions, ["机缘巧合", "意外发现"]);
        assert_eq!(result.examples.len(), 1);
        assert_eq!(
            result.examples[0].english,
            "A fortunate serendipity happened."
        );
        assert_eq!(result.sections[0].id, "simple");
    }

    #[test]
    fn youdao_parser_reads_chinese_to_english_ssr_candidates() {
        let page = r#"
          <h1 class="word-head">机缘巧合</h1>
          <script>
            window.__NUXT__={wordData:{web_trans:{"web-translation":[
              {value:"serendipity"},
              {value:"by chance"},
              {value:"serendipity"}
            ]}}};
          </script>
        "#;
        let result = parse_youdao(page, "机缘巧合", "https://example.test").unwrap();
        assert_eq!(result.senses[0].part_of_speech, "中译英");
        assert_eq!(result.senses[0].definitions, ["serendipity", "by chance"]);
        assert!(result.uk_audio.is_none());
        assert!(result.us_audio.is_none());
    }

    #[test]
    fn youdao_parser_reads_collins_ssr_state_without_running_script() {
        let page = r#"
          <h1 class="word-head">good</h1>
          <div class="simple dict-module"><ul class="basic"><li class="word-exp">
            <span class="pos">adj.</span><span class="trans">好的</span>
          </li></ul></div>
          <script>
            window.__NUXT__={wordData:{collins:{collins_entries:[{
              entries:{entry:[{tran_entry:[{
                pos_entry:{pos:"ADJ"},
                exam_sents:{sent:[{eng_sent:"A good example.",chn_sent:"一个好例子。"}]},
                tran:"\u003Cb\u003EGood\u003C/b\u003E means pleasant. 令人愉快的"
              }]}]}
            }]}}};
          </script>
        "#;
        let result = parse_youdao(page, "good", "https://example.test").unwrap();
        let collins = result
            .sections
            .iter()
            .find(|section| section.id == "collins")
            .unwrap();
        assert_eq!(
            collins.senses[0].definitions,
            ["Good means pleasant. 令人愉快的"]
        );
        assert_eq!(collins.examples[0].english, "A good example.");
        assert_eq!(
            collins.examples[0].translation.as_deref(),
            Some("一个好例子。")
        );
    }
}

#[cfg(target_os = "linux")]
fn configure_linux_native_window(window: &tauri::WebviewWindow) -> tauri::Result<()> {
    use gtk::prelude::*;

    let gtk_window = window.gtk_window()?;

    // GTK themes paint a solid `window.background` layer below WebKit. That
    // layer is visible in the gutter reserved for the client-rendered shadow,
    // even when Tauri and the page both request a transparent background.
    // Give the native window an RGBA visual and override the theme layer so
    // only the rounded web canvas and its shadow are painted there.
    gtk_window.set_app_paintable(true);
    if let Some(screen) = gtk::prelude::WidgetExt::screen(&gtk_window) {
        if let Some(visual) = screen.rgba_visual() {
            gtk_window.set_visual(Some(&visual));
        }
    }
    gtk_window
        .style_context()
        .add_class("aurora-transparent-window");
    let transparent_window_css = gtk::CssProvider::new();
    transparent_window_css.load_from_data(
        b"window.aurora-transparent-window.background { background-color: transparent; background-image: none; }",
    ).map_err(|error| std::io::Error::other(error.to_string()))?;
    gtk_window.style_context().add_provider(
        &transparent_window_css,
        gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
    );

    // GNOME refuses half-screen tiling when the minimum width exceeds half
    // the logical work area (780px on a 3120px display at 200% scaling).
    window.set_min_size(Some(tauri::LogicalSize::new(600.0, 500.0)))?;

    // Tauri's isMaximized() does not include GNOME's side-by-side tiled state.
    // GTK reports it on both Wayland and XWayland. Keep it separate from the
    // frontend's maximized/fullscreen flag so resize queries cannot clear it.
    let frontend = window.clone();
    gtk_window.connect_window_state_event(move |_, event| {
        let tiled = event.new_window_state().intersects(
            gtk::gdk::WindowState::TILED
                | gtk::gdk::WindowState::TOP_TILED
                | gtk::gdk::WindowState::RIGHT_TILED
                | gtk::gdk::WindowState::BOTTOM_TILED
                | gtk::gdk::WindowState::LEFT_TILED,
        );
        let script = if tiled {
            "document.documentElement.dataset.windowTiled = 'true'"
        } else {
            "document.documentElement.dataset.windowTiled = 'false'"
        };
        let _ = frontend.eval(script);
        gtk::glib::Propagation::Proceed
    });
    Ok(())
}

#[cfg(target_os = "macos")]
fn configure_macos_native_window(window: &tauri::WebviewWindow) -> tauri::Result<()> {
    use objc2::{class, msg_send, runtime::AnyObject};

    // A transparent NSWindow alone is insufficient on some WebKit versions: its
    // native content view may still paint beyond the web content. Clip that view
    // directly, so the 12pt radius is the physical window edge on macOS.
    const WINDOW_CORNER_RADIUS: f64 = 12.0;
    const NS_WINDOW_CLOSE_BUTTON: usize = 0;
    const NS_WINDOW_MINIATURIZE_BUTTON: usize = 1;
    const NS_WINDOW_ZOOM_BUTTON: usize = 2;
    let ns_window = window.ns_window()?.cast::<AnyObject>();
    let ns_view = window.ns_view()?.cast::<AnyObject>();

    unsafe {
        for (button_kind, button_name) in [
            (NS_WINDOW_CLOSE_BUTTON, "closeButton"),
            (NS_WINDOW_MINIATURIZE_BUTTON, "miniaturizeButton"),
            (NS_WINDOW_ZOOM_BUTTON, "zoomButton"),
        ] {
            let button: *mut AnyObject = msg_send![ns_window, standardWindowButton: button_kind];
            if button.is_null() {
                return Err(std::io::Error::other(format!(
                    "NSWindow standardWindowButton(.{button_name}) is unavailable"
                ))
                .into());
            }
            let _: () = msg_send![button, setHidden: false];
            let _: () = msg_send![button, setEnabled: true];
        }

        let _: () = msg_send![ns_window, setOpaque: false];
        let clear_color: *mut AnyObject = msg_send![class!(NSColor), clearColor];
        let _: () = msg_send![ns_window, setBackgroundColor: clear_color];
        let _: () = msg_send![ns_view, setWantsLayer: true];
        let layer: *mut AnyObject = msg_send![ns_view, layer];
        if !layer.is_null() {
            let _: () = msg_send![layer, setCornerRadius: WINDOW_CORNER_RADIUS];
            let _: () = msg_send![layer, setMasksToBounds: true];
        }
    }

    Ok(())
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let builder = tauri::Builder::default();

    #[cfg(desktop)]
    let builder = builder.plugin(
        tauri_plugin_window_state::Builder::new()
            .with_state_flags(tauri_plugin_window_state::StateFlags::SIZE)
            .build(),
    );

    builder
        .plugin(tauri_plugin_clipboard_manager::init())
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_system_fonts::init())
        .manage(LlmManager::default())
        .setup(|app| {
            // Warm the shared dictionary and its prefix index before the hidden
            // window is revealed. This prevents the first few keystrokes from
            // racing several SQLite initialization writes.
            drop(database(&app.handle()).map_err(std::io::Error::other)?);
            vocabulary::ensure_vocabulary_book(&app.handle()).map_err(std::io::Error::other)?;
            if let Some(window) = app.get_webview_window("main") {
                #[cfg(target_os = "linux")]
                configure_linux_native_window(&window).map_err(std::io::Error::other)?;

                // Windows retains its native opaque base. On Linux the CSS
                // canvas is already opaque; the native surface must be clear
                // for the rounded edge and client-rendered shadow.
                #[cfg(target_os = "windows")]
                window
                    .set_background_color(Some((247, 246, 255, 255).into()))
                    .map_err(std::io::Error::other)?;

                // Keep both the native surface and webview background clear.
                #[cfg(any(target_os = "macos", target_os = "linux"))]
                window
                    .set_background_color(Some((0, 0, 0, 0).into()))
                    .map_err(std::io::Error::other)?;
                #[cfg(target_os = "macos")]
                configure_macos_native_window(&window).map_err(std::io::Error::other)?;

                // The frontend normally reveals the window immediately after
                // its first React commit. Keep a native fallback so a frontend
                // initialization error can never leave the app hidden forever.
                let fallback_window = window.clone();
                tauri::async_runtime::spawn(async move {
                    tokio::time::sleep(Duration::from_millis(900)).await;
                    if fallback_window.is_visible().unwrap_or(false) {
                        return;
                    }
                    let _ = fallback_window.show();
                    let _ = fallback_window.set_focus();
                });
            }
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            lookup_local,
            word_graph::lookup_word_graph,
            suggest_local_words,
            lookup_online,
            dictionary_status,
            youdao_is_available,
            llm::llm_status,
            llm::prepare_llm,
            llm::download_llm_model,
            llm::delete_llm_model,
            llm::lookup_llm,
            llm::generate_graph_mnemonic,
            llm::translate_llm,
            vocabulary::list_vocabulary_entries,
            vocabulary::add_vocabulary_entry,
            vocabulary::save_vocabulary_entry,
            vocabulary::delete_vocabulary_entry,
            vocabulary::vocabulary_book_status,
            vocabulary::export_vocabulary_book,
            vocabulary::import_vocabulary_book,
            vocabulary::move_vocabulary_book,
            vocabulary::open_vocabulary_book,
            vocabulary_quiz::generate_vocabulary_quiz_question
        ])
        .run(tauri::generate_context!())
        .expect("error while running Aurora Dictionary");
}
