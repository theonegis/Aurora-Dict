use rusqlite::{Connection, OpenFlags, OptionalExtension, Row};
use serde::Serialize;
use std::{
    cmp::Ordering,
    fs::{self, File},
    io,
    path::{Path, PathBuf},
    sync::Mutex,
    time::Duration,
};
use zip::ZipArchive;

const BUNDLED_SOURCE_FILE: &str = ".ecdict-source.sqlite3";
const COMPLETE_DICTIONARY_MINIMUM_ENTRIES: i64 = 100_000;

/// The bundled lexicon and its per-instance initialization state.
pub struct Dictionary {
    path: PathBuf,
    archive: Option<PathBuf>,
    ready: Mutex<bool>,
}

impl Dictionary {
    /// `archive` is only needed when the database must be installed or rebuilt.
    pub fn new(path: PathBuf, archive: Option<PathBuf>) -> Self {
        Self {
            path,
            archive,
            ready: Mutex::new(false),
        }
    }

    pub fn prepare(&self) -> Result<(), String> {
        let mut ready = self.ready.lock().map_err(|_| "无法取得词库初始化状态。")?;
        if *ready {
            return Ok(());
        }
        // A SQLite transaction provides a crash-safe installation lock shared
        // across Dictionary instances and processes using this destination.
        let parent = self.path.parent().filter(|p| !p.as_os_str().is_empty()).unwrap_or(Path::new("."));
        fs::create_dir_all(parent).map_err(|e| format!("无法创建词库目录：{e}"))?;
        let mut lock_path = self.path.as_os_str().to_os_string();
        lock_path.push(".install-lock");
        let install_lock = Connection::open(Path::new(&lock_path)).map_err(|e| format!("无法锁定词库安装：{e}"))?;
        install_lock.busy_timeout(Duration::from_secs(30)).map_err(|e| e.to_string())?;
        install_lock.execute_batch("BEGIN EXCLUSIVE").map_err(|e| format!("词库正在安装，请稍后重试：{e}"))?;
        if !complete_dictionary_exists(&self.path)? {
            let archive = self
                .archive
                .as_deref()
                .ok_or("安装包中缺少默认 ECDICT SQLite 资源。请重新安装应用。")?;
            install_dictionary(archive, &self.path)?;
        }
        self.open_connection()?
            .execute_batch(
                "CREATE INDEX IF NOT EXISTS idx_stardict_word_nocase
                 ON stardict(word COLLATE NOCASE);",
            )
            .map_err(|error| format!("无法初始化输入建议索引：{error}"))?;
        *ready = true;
        Ok(())
    }

    fn open_connection(&self) -> Result<Connection, String> {
        let connection = Connection::open_with_flags(&self.path, OpenFlags::SQLITE_OPEN_READ_WRITE)
            .map_err(|error| format!("无法打开本地词库：{error}"))?;
        connection
            .busy_timeout(Duration::from_millis(1_200))
            .map_err(|error| format!("无法设置本地词库等待时间：{error}"))?;
        connection
            .execute_batch("PRAGMA journal_mode = WAL; PRAGMA synchronous = NORMAL;")
            .map_err(|error| format!("无法初始化本地词库：{error}"))?;
        Ok(connection)
    }

    /// Transitional access for the desktop graph and quiz queries.
    pub fn connect(&self) -> Result<Connection, String> {
        self.prepare()?;
        self.open_connection()
    }

    pub fn lookup(&self, query: &str) -> Result<LocalLookup, String> {
        lookup(&self.connect()?, query)
    }

    pub fn suggest(&self, query: &str) -> Result<LocalSuggestions, String> {
        let query = normalise_query(query);
        if query.chars().count() < 2 || contains_chinese(&query) {
            return Ok(LocalSuggestions {
                suggestions: Vec::new(),
                correction: false,
            });
        }
        autocomplete_suggestions(&self.connect()?, &query)
    }

    pub fn status(&self) -> Result<DictionaryStatus, String> {
        let entry_count: i64 = self
            .connect()?
            .query_row("SELECT COUNT(*) FROM stardict", [], |row| row.get(0))
            .map_err(|error| format!("无法读取词库状态：{error}"))?;
        Ok(DictionaryStatus {
            entry_count,
            sample_data: entry_count <= 5,
        })
    }
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DictionaryEntry {
    pub word: String,
    pub phonetic: Option<String>,
    pub uk_phonetic: Option<String>,
    pub us_phonetic: Option<String>,
    pub uk_audio: Option<String>,
    pub us_audio: Option<String>,
    pub translation: Option<String>,
    pub definition: Option<String>,
    pub pos: Option<String>,
    pub exchange: Option<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LocalLookup {
    pub query: String,
    pub entries: Vec<DictionaryEntry>,
    pub suggestions: Vec<String>,
    pub sample_data: bool,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LocalSuggestions {
    pub suggestions: Vec<String>,
    pub correction: bool,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DictionaryStatus {
    pub entry_count: i64,
    pub sample_data: bool,
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

pub fn contains_chinese(input: &str) -> bool {
    input
        .chars()
        .any(|character| ('\u{4e00}'..='\u{9fff}').contains(&character))
}

pub fn normalise_query(query: &str) -> String {
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

fn lookup(connection: &Connection, query: &str) -> Result<LocalLookup, String> {
    let query = normalise_query(query);
    if query.is_empty() {
        return Ok(LocalLookup {
            query,
            entries: Vec::new(),
            suggestions: Vec::new(),
            sample_data: sample_data(connection)?,
        });
    }

    let entries = if contains_chinese(&query) {
        search_chinese_entries(connection, &query)?
    } else {
        let exact_sql = format!(
            "SELECT {} FROM stardict WHERE word = ?1 COLLATE NOCASE LIMIT 1",
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
                 WHERE word LIKE ?1 ESCAPE '\\' COLLATE NOCASE
                 ORDER BY frq DESC, word COLLATE NOCASE LIMIT 24",
                select_columns()
            );
            let mut statement = connection
                .prepare(&prefix_sql)
                .map_err(|error| format!("无法检索本地词典：{error}"))?;
            let prefix_matches = statement
                .query_map([format!("{}%", escape_like(&query))], entry_from_row)
                .map_err(|error| format!("无法读取本地词典：{error}"))?
                .collect::<Result<Vec<_>, _>>()
                .map_err(|error| format!("无法读取本地词典：{error}"))?;
            prefix_matches
        }
    };

    let suggestions = if entries.is_empty() && !contains_chinese(&query) {
        spelling_suggestions(connection, &query)?
    } else {
        Vec::new()
    };

    Ok(LocalLookup {
        query,
        entries,
        suggestions,
        sample_data: sample_data(connection)?,
    })
}

fn autocomplete_suggestions(
    connection: &Connection,
    query: &str,
) -> Result<LocalSuggestions, String> {
    let exact_match = connection
        .query_row(
            "SELECT 1 FROM stardict WHERE word = ?1 COLLATE NOCASE LIMIT 1",
            [&query],
            |_| Ok(()),
        )
        .optional()
        .map_err(|error| format!("无法检查输入词条：{error}"))?
        .is_some();
    let mut statement = connection
        .prepare(
            "SELECT word FROM stardict
             WHERE word LIKE ?1 ESCAPE '\\' COLLATE NOCASE AND lower(word) <> lower(?2)
             ORDER BY frq DESC, word COLLATE NOCASE LIMIT 10",
        )
        .map_err(|error| format!("无法生成输入建议：{error}"))?;
    let prefix_matches = statement
        .query_map((format!("{}%", escape_like(query)), query), |row| row.get::<_, String>(0))
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

    let pattern = format!("%{}%", escape_like(query));
    let like_sql = format!(
        "SELECT {} FROM stardict
         WHERE translation LIKE ?1 ESCAPE '\\' OR definition LIKE ?1 ESCAPE '\\'
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
             WHERE word LIKE ?1 ESCAPE '\\' COLLATE NOCASE
             ORDER BY frq DESC, word COLLATE NOCASE LIMIT 240",
        )
        .map_err(|error| format!("无法生成拼写建议：{error}"))?;
    let candidates = statement
        .query_map([format!("{}%", escape_like(&first.to_string()))], |row| row.get::<_, String>(0))
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

pub fn levenshtein(left: &str, right: &str) -> usize {
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

fn complete_dictionary_exists(path: &Path) -> Result<bool, String> {
    if !path.try_exists().map_err(|e| format!("无法检查本地词库：{e}"))? {
        return Ok(false);
    }
    let connection = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)
        .map_err(|e| format!("无法验证本地词库：{e}"))?;
    let has_table = |name: &str| -> Result<bool, String> {
        connection.query_row("SELECT 1 FROM sqlite_master WHERE type='table' AND name=?1", [name], |_| Ok(()))
            .optional().map(|row| row.is_some()).map_err(|e| format!("无法验证本地词库：{e}"))
    };
    if !has_table("stardict")? { return Ok(false); }
    let entry_count: i64 = connection.query_row("SELECT COUNT(*) FROM stardict", [], |row| row.get(0))
        .map_err(|e| format!("无法验证本地词库：{e}"))?;
    Ok(entry_count >= COMPLETE_DICTIONARY_MINIMUM_ENTRIES && has_table("dictionary_fts")?)
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

fn install_dictionary(archive: &Path, destination: &Path) -> Result<(), String> {
    let parent = destination
        .parent()
        .filter(|path| !path.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    fs::create_dir_all(parent).map_err(|error| format!("无法创建词库目录：{error}"))?;
    let staging = tempfile::Builder::new()
        .prefix(BUNDLED_SOURCE_FILE)
        .tempfile_in(parent)
        .map_err(|error| format!("无法创建离线词库临时文件：{error}"))?;
    unpack_bundled_dictionary(archive, staging.path())?;
    prepare_bundled_dictionary(staging.path())?;
    // Never unlink journals belonging to another live SQLite connection.
    for suffix in ["-wal", "-shm", "-journal"] {
        let mut sidecar = destination.as_os_str().to_os_string();
        sidecar.push(suffix);
        if Path::new(&sidecar).try_exists().map_err(|e| e.to_string())? {
            return Err("词库仍有 SQLite 日志，请关闭其他使用该词库的程序后重试。".into());
        }
    }
    staging
        .persist(destination)
        .map_err(|error| format!("无法安装内置 ECDICT 数据库：{error}"))?;
    Ok(())
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
    fn wildcard_characters_are_literal_prefixes() {
        let db = Connection::open_in_memory().unwrap();
        db.execute_batch("CREATE TABLE stardict(word TEXT, frq INTEGER); INSERT INTO stardict VALUES ('apple', 1), ('apply', 2);").unwrap();
        assert!(autocomplete_suggestions(&db, "__").unwrap().suggestions.is_empty());
    }

    #[test]
    fn locked_dictionary_is_not_treated_as_missing() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("dict.db");
        let db = Connection::open(&path).unwrap();
        db.execute_batch("CREATE TABLE stardict(word TEXT); BEGIN EXCLUSIVE;").unwrap();
        let error = Dictionary::new(path, None).prepare().unwrap_err();
        assert!(error.contains("locked"), "{error}");
    }

}

fn escape_like(value: &str) -> String {
    value.replace('\\', "\\\\").replace('%', "\\%").replace('_', "\\_")
}
