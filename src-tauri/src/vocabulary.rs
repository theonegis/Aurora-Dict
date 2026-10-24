use rusqlite::{params, Connection, OpenFlags, OptionalExtension, Row};
use serde::{Deserialize, Serialize};
use std::{
    fs,
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};
use tauri::{AppHandle, Manager};

const VOCABULARY_FILE: &str = "aurora-vocabulary.sqlite3";
const LOCATION_CONFIG_FILE: &str = "vocabulary-location.json";
const SCHEMA_VERSION: &str = "1";

#[derive(Debug, Deserialize, Serialize)]
struct VocabularyLocation {
    path: String,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VocabularyEntryInput {
    pub id: Option<i64>,
    pub word: String,
    #[serde(default)]
    pub phonetic: String,
    #[serde(default)]
    pub definition_markdown: String,
    #[serde(default)]
    pub examples_markdown: String,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct VocabularyEntry {
    pub id: i64,
    pub word: String,
    pub phonetic: String,
    pub definition_markdown: String,
    pub examples_markdown: String,
    pub created_at: i64,
    pub updated_at: i64,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct VocabularyBookStatus {
    pub path: String,
    pub entry_count: i64,
    pub backup_path: Option<String>,
}

fn now_millis() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .min(i64::MAX as u128) as i64
}

fn vocabulary_directory(app: &AppHandle) -> Result<PathBuf, String> {
    let directory = app
        .path()
        .app_data_dir()
        .map_err(|error| format!("无法取得应用数据目录：{error}"))?;
    fs::create_dir_all(&directory).map_err(|error| format!("无法创建应用数据目录：{error}"))?;
    Ok(directory)
}

fn location_config_path(app: &AppHandle) -> Result<PathBuf, String> {
    Ok(vocabulary_directory(app)?.join(LOCATION_CONFIG_FILE))
}

fn default_vocabulary_path(app: &AppHandle) -> Result<PathBuf, String> {
    Ok(vocabulary_directory(app)?.join(VOCABULARY_FILE))
}

fn vocabulary_path(app: &AppHandle) -> Result<PathBuf, String> {
    let config_path = location_config_path(app)?;
    if !config_path.exists() {
        return default_vocabulary_path(app);
    }
    let content = fs::read_to_string(&config_path)
        .map_err(|error| format!("无法读取生词本位置配置：{error}"))?;
    let location: VocabularyLocation =
        serde_json::from_str(&content).map_err(|error| format!("生词本位置配置已损坏：{error}"))?;
    if location.path.trim().is_empty() {
        return Err("生词本位置配置为空。".to_string());
    }
    Ok(PathBuf::from(location.path))
}

fn save_vocabulary_path(app: &AppHandle, path: &Path) -> Result<(), String> {
    let config_path = location_config_path(app)?;
    let content = serde_json::to_vec_pretty(&VocabularyLocation {
        path: path.to_string_lossy().into_owned(),
    })
    .map_err(|error| format!("无法生成生词本位置配置：{error}"))?;
    fs::write(&config_path, content).map_err(|error| format!("无法保存生词本位置配置：{error}"))
}

fn initialize_schema(connection: &Connection) -> Result<(), String> {
    connection
        .execute_batch(
            "PRAGMA journal_mode = WAL;
             PRAGMA synchronous = NORMAL;
             CREATE TABLE IF NOT EXISTS vocabulary_metadata (
               key TEXT PRIMARY KEY,
               value TEXT NOT NULL
             );
             INSERT OR IGNORE INTO vocabulary_metadata(key, value)
               VALUES ('schema_version', '1');
             CREATE TABLE IF NOT EXISTS vocabulary_entries (
               id INTEGER PRIMARY KEY AUTOINCREMENT,
               word TEXT NOT NULL COLLATE NOCASE UNIQUE,
               phonetic TEXT NOT NULL DEFAULT '',
               definition_markdown TEXT NOT NULL DEFAULT '',
               examples_markdown TEXT NOT NULL DEFAULT '',
               created_at INTEGER NOT NULL,
               updated_at INTEGER NOT NULL
             );
             CREATE INDEX IF NOT EXISTS idx_vocabulary_updated_at
               ON vocabulary_entries(updated_at DESC);",
        )
        .map_err(|error| format!("无法初始化生词本：{error}"))
}

fn open_vocabulary(path: &Path) -> Result<Connection, String> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|error| format!("无法创建生词本目录：{error}"))?;
    }
    let connection = Connection::open(path).map_err(|error| format!("无法打开生词本：{error}"))?;
    initialize_schema(&connection)?;
    Ok(connection)
}

pub fn ensure_vocabulary_book(app: &AppHandle) -> Result<(), String> {
    let path = vocabulary_path(app)?;
    if location_config_path(app)?.exists() && !path.exists() {
        // A custom location may temporarily be unavailable (for example, an
        // unplugged external drive). Do not silently create a blank book there.
        return Ok(());
    }
    open_vocabulary(&path).map(|_| ())
}

pub(crate) fn open_active_vocabulary(app: &AppHandle) -> Result<(PathBuf, Connection), String> {
    let path = vocabulary_path(app)?;
    if location_config_path(app)?.exists() && !path.exists() {
        return Err(format!(
            "自定义生词本不存在或暂时不可用：{}",
            path.display()
        ));
    }
    let connection = open_vocabulary(&path)?;
    Ok((path, connection))
}

fn entry_from_row(row: &Row<'_>) -> rusqlite::Result<VocabularyEntry> {
    Ok(VocabularyEntry {
        id: row.get(0)?,
        word: row.get(1)?,
        phonetic: row.get(2)?,
        definition_markdown: row.get(3)?,
        examples_markdown: row.get(4)?,
        created_at: row.get(5)?,
        updated_at: row.get(6)?,
    })
}

pub(crate) fn entry_by_id(connection: &Connection, id: i64) -> Result<VocabularyEntry, String> {
    connection
        .query_row(
            "SELECT id, word, phonetic, definition_markdown, examples_markdown,
                    created_at, updated_at
             FROM vocabulary_entries WHERE id = ?1",
            [id],
            entry_from_row,
        )
        .map_err(|error| format!("无法读取生词：{error}"))
}

fn entry_by_word(connection: &Connection, word: &str) -> Result<VocabularyEntry, String> {
    connection
        .query_row(
            "SELECT id, word, phonetic, definition_markdown, examples_markdown,
                    created_at, updated_at
             FROM vocabulary_entries WHERE word = ?1 COLLATE NOCASE",
            [word],
            entry_from_row,
        )
        .map_err(|error| format!("无法读取生词：{error}"))
}

fn validate_input(input: &VocabularyEntryInput) -> Result<VocabularyEntryInput, String> {
    let word = input.word.trim();
    if word.is_empty() {
        return Err("单词不能为空。".to_string());
    }
    if word.chars().count() > 120 {
        return Err("单词长度不能超过 120 个字符。".to_string());
    }
    if input.definition_markdown.chars().count() > 100_000
        || input.examples_markdown.chars().count() > 100_000
    {
        return Err("单条生词的 Markdown 内容不能超过 100000 个字符。".to_string());
    }
    Ok(VocabularyEntryInput {
        id: input.id,
        word: word.to_string(),
        phonetic: input.phonetic.trim().to_string(),
        definition_markdown: input.definition_markdown.trim().to_string(),
        examples_markdown: input.examples_markdown.trim().to_string(),
    })
}

#[tauri::command]
pub fn list_vocabulary_entries(
    app: AppHandle,
    search: Option<String>,
) -> Result<Vec<VocabularyEntry>, String> {
    let (_, connection) = open_active_vocabulary(&app)?;
    let search = search.unwrap_or_default().trim().to_string();
    let (sql, parameter) = if search.is_empty() {
        (
            "SELECT id, word, phonetic, definition_markdown, examples_markdown,
                    created_at, updated_at
             FROM vocabulary_entries ORDER BY updated_at DESC, word COLLATE NOCASE",
            None,
        )
    } else {
        (
            "SELECT id, word, phonetic, definition_markdown, examples_markdown,
                    created_at, updated_at
             FROM vocabulary_entries
             WHERE word LIKE ?1 ESCAPE '\\' COLLATE NOCASE
                OR definition_markdown LIKE ?1 ESCAPE '\\' COLLATE NOCASE
                OR examples_markdown LIKE ?1 ESCAPE '\\' COLLATE NOCASE
             ORDER BY updated_at DESC, word COLLATE NOCASE",
            Some(format!(
                "%{}%",
                search
                    .replace('\\', "\\\\")
                    .replace('%', "\\%")
                    .replace('_', "\\_")
            )),
        )
    };
    let mut statement = connection
        .prepare(sql)
        .map_err(|error| format!("无法读取生词本：{error}"))?;
    let rows = match parameter {
        Some(value) => statement.query_map([value], entry_from_row),
        None => statement.query_map([], entry_from_row),
    }
    .map_err(|error| format!("无法读取生词本：{error}"))?;
    rows.collect::<Result<Vec<_>, _>>()
        .map_err(|error| format!("无法读取生词本：{error}"))
}

#[tauri::command]
pub fn add_vocabulary_entry(
    app: AppHandle,
    entry: VocabularyEntryInput,
) -> Result<VocabularyEntry, String> {
    let entry = validate_input(&entry)?;
    let (_, connection) = open_active_vocabulary(&app)?;
    let timestamp = now_millis();
    connection
        .execute(
            "INSERT INTO vocabulary_entries(
               word, phonetic, definition_markdown, examples_markdown, created_at, updated_at
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?5)
             ON CONFLICT(word) DO NOTHING",
            params![
                entry.word,
                entry.phonetic,
                entry.definition_markdown,
                entry.examples_markdown,
                timestamp
            ],
        )
        .map_err(|error| format!("无法加入生词本：{error}"))?;
    entry_by_word(&connection, &entry.word)
}

#[tauri::command]
pub fn save_vocabulary_entry(
    app: AppHandle,
    entry: VocabularyEntryInput,
) -> Result<VocabularyEntry, String> {
    let entry = validate_input(&entry)?;
    let id = entry
        .id
        .ok_or_else(|| "缺少要保存的生词编号。".to_string())?;
    let (_, connection) = open_active_vocabulary(&app)?;
    let changed = connection
        .execute(
            "UPDATE vocabulary_entries
             SET word = ?1, phonetic = ?2, definition_markdown = ?3,
                 examples_markdown = ?4, updated_at = ?5
             WHERE id = ?6",
            params![
                entry.word,
                entry.phonetic,
                entry.definition_markdown,
                entry.examples_markdown,
                now_millis(),
                id
            ],
        )
        .map_err(|error| {
            if error.to_string().contains("UNIQUE constraint failed") {
                "生词本中已经存在同名单词。".to_string()
            } else {
                format!("无法保存生词：{error}")
            }
        })?;
    if changed == 0 {
        return Err("要保存的生词不存在，可能已被删除。".to_string());
    }
    entry_by_id(&connection, id)
}

#[tauri::command]
pub fn delete_vocabulary_entry(app: AppHandle, id: i64) -> Result<(), String> {
    let (_, connection) = open_active_vocabulary(&app)?;
    connection
        .execute("DELETE FROM vocabulary_entries WHERE id = ?1", [id])
        .map_err(|error| format!("无法删除生词：{error}"))?;
    Ok(())
}

fn validate_vocabulary_file(path: &Path) -> Result<i64, String> {
    let connection = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)
        .map_err(|error| format!("无法打开所选 SQLite 文件：{error}"))?;
    let integrity: String = connection
        .query_row("PRAGMA quick_check", [], |row| row.get(0))
        .map_err(|error| format!("无法校验 SQLite 文件：{error}"))?;
    if integrity != "ok" {
        return Err(format!("所选 SQLite 文件未通过完整性校验：{integrity}"));
    }
    let version: Option<String> = connection
        .query_row(
            "SELECT value FROM vocabulary_metadata WHERE key = 'schema_version'",
            [],
            |row| row.get(0),
        )
        .optional()
        .map_err(|_| "所选文件不是 Aurora Dict 生词本。".to_string())?;
    if version.as_deref() != Some(SCHEMA_VERSION) {
        return Err("生词本版本不受支持。".to_string());
    }
    connection
        .query_row("SELECT COUNT(*) FROM vocabulary_entries", [], |row| {
            row.get(0)
        })
        .map_err(|_| "所选文件缺少生词数据表。".to_string())
}

fn status_for_path(
    path: &Path,
    backup_path: Option<PathBuf>,
) -> Result<VocabularyBookStatus, String> {
    Ok(VocabularyBookStatus {
        path: path.to_string_lossy().into_owned(),
        entry_count: validate_vocabulary_file(path)?,
        backup_path: backup_path.map(|value| value.to_string_lossy().into_owned()),
    })
}

#[tauri::command]
pub fn vocabulary_book_status(app: AppHandle) -> Result<VocabularyBookStatus, String> {
    let (path, connection) = open_active_vocabulary(&app)?;
    drop(connection);
    status_for_path(&path, None)
}

#[tauri::command]
pub fn export_vocabulary_book(
    app: AppHandle,
    destination_path: String,
) -> Result<VocabularyBookStatus, String> {
    let (source, connection) = open_active_vocabulary(&app)?;
    connection
        .execute_batch("PRAGMA wal_checkpoint(TRUNCATE);")
        .map_err(|error| format!("无法准备导出生词本：{error}"))?;
    drop(connection);
    let destination = PathBuf::from(destination_path);
    if destination.as_os_str().is_empty() {
        return Err("没有选择导出位置。".to_string());
    }
    if source == destination {
        return status_for_path(&source, None);
    }
    if let Some(parent) = destination.parent() {
        fs::create_dir_all(parent).map_err(|error| format!("无法创建导出目录：{error}"))?;
    }
    fs::copy(&source, &destination).map_err(|error| format!("无法导出生词本：{error}"))?;
    status_for_path(&destination, None)
}

#[tauri::command]
pub fn import_vocabulary_book(
    app: AppHandle,
    source_path: String,
) -> Result<VocabularyBookStatus, String> {
    let source = PathBuf::from(source_path);
    if source.as_os_str().is_empty() {
        return Err("没有选择要导入的文件。".to_string());
    }
    validate_vocabulary_file(&source)?;
    let (destination, current) = open_active_vocabulary(&app)?;
    current
        .execute_batch("PRAGMA wal_checkpoint(TRUNCATE);")
        .map_err(|error| format!("无法准备导入生词本：{error}"))?;
    drop(current);
    if source == destination {
        return status_for_path(&destination, None);
    }
    let staging = destination.with_extension("importing.sqlite3");
    fs::copy(&source, &staging).map_err(|error| format!("无法复制生词本：{error}"))?;
    if let Err(error) = validate_vocabulary_file(&staging) {
        let _ = fs::remove_file(&staging);
        return Err(error);
    }
    let backup = if destination.exists() {
        let backup = destination
            .with_file_name(format!("aurora-vocabulary.backup-{}.sqlite3", now_millis()));
        fs::copy(&destination, &backup).map_err(|error| format!("无法备份当前生词本：{error}"))?;
        Some(backup)
    } else {
        None
    };
    for suffix in ["-wal", "-shm"] {
        let _ = fs::remove_file(format!("{}{}", destination.display(), suffix));
    }
    if destination.exists() {
        fs::remove_file(&destination).map_err(|error| format!("无法替换当前生词本：{error}"))?;
    }
    fs::rename(&staging, &destination).map_err(|error| format!("无法启用导入的生词本：{error}"))?;
    status_for_path(&destination, backup)
}

#[tauri::command]
pub fn move_vocabulary_book(
    app: AppHandle,
    destination_path: String,
) -> Result<VocabularyBookStatus, String> {
    let destination = PathBuf::from(destination_path);
    if destination.as_os_str().is_empty() {
        return Err("没有选择新的生词本位置。".to_string());
    }
    let (source, connection) = open_active_vocabulary(&app)?;
    connection
        .execute_batch("PRAGMA wal_checkpoint(TRUNCATE);")
        .map_err(|error| format!("无法准备迁移生词本：{error}"))?;
    drop(connection);
    if source == destination {
        save_vocabulary_path(&app, &destination)?;
        return status_for_path(&destination, None);
    }
    if let Some(parent) = destination.parent() {
        fs::create_dir_all(parent).map_err(|error| format!("无法创建生词本目录：{error}"))?;
    }
    let backup = if destination.exists() {
        validate_vocabulary_file(&destination)?;
        let backup = destination.with_file_name(format!(
            "{}.backup-{}.sqlite3",
            destination
                .file_stem()
                .and_then(|value| value.to_str())
                .unwrap_or("aurora-vocabulary"),
            now_millis()
        ));
        fs::copy(&destination, &backup).map_err(|error| format!("无法备份目标生词本：{error}"))?;
        Some(backup)
    } else {
        None
    };
    let staging = destination.with_extension("moving.sqlite3");
    fs::copy(&source, &staging).map_err(|error| format!("无法迁移生词本：{error}"))?;
    validate_vocabulary_file(&staging)?;
    if destination.exists() {
        fs::remove_file(&destination).map_err(|error| format!("无法替换目标生词本：{error}"))?;
    }
    fs::rename(&staging, &destination).map_err(|error| format!("无法启用新的生词本：{error}"))?;
    save_vocabulary_path(&app, &destination)?;
    status_for_path(&destination, backup)
}

#[tauri::command]
pub fn open_vocabulary_book(
    app: AppHandle,
    source_path: String,
) -> Result<VocabularyBookStatus, String> {
    let source = PathBuf::from(source_path);
    if source.as_os_str().is_empty() {
        return Err("没有选择生词本文件。".to_string());
    }
    validate_vocabulary_file(&source)?;
    if let Ok((_, current)) = open_active_vocabulary(&app) {
        current
            .execute_batch("PRAGMA wal_checkpoint(TRUNCATE);")
            .map_err(|error| format!("无法保存当前生词本：{error}"))?;
    }
    save_vocabulary_path(&app, &source)?;
    status_for_path(&source, None)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temporary_path(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "aurora-vocabulary-{name}-{}-{}.sqlite3",
            std::process::id(),
            now_millis()
        ))
    }

    #[test]
    fn schema_is_portable_and_validated() {
        let path = temporary_path("schema");
        let connection = open_vocabulary(&path).unwrap();
        connection
            .execute(
                "INSERT INTO vocabulary_entries(word, definition_markdown, examples_markdown, created_at, updated_at)
                 VALUES ('serendipity', '## 释义', '## 例句', 1, 1)",
                [],
            )
            .unwrap();
        drop(connection);
        assert_eq!(validate_vocabulary_file(&path).unwrap(), 1);
        let _ = fs::remove_file(path);
    }

    #[test]
    fn input_rejects_blank_words() {
        let input = VocabularyEntryInput {
            id: None,
            word: "   ".into(),
            phonetic: String::new(),
            definition_markdown: String::new(),
            examples_markdown: String::new(),
        };
        assert!(validate_input(&input).is_err());
    }
}
