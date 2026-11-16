use rusqlite::{params, Connection, DatabaseName, OpenFlags, OptionalExtension, Row};
use serde::{Deserialize, Serialize};
use std::{
    fs,
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};
use tempfile::{Builder, NamedTempFile};

const SCHEMA_VERSION: &str = "1";

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
    pub sync_version: String,
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

pub fn connect(path: &Path) -> Result<Connection, String> {
    if let Some(parent) = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        fs::create_dir_all(parent).map_err(|error| format!("无法创建生词本目录：{error}"))?;
    }
    let existing = fs::metadata(path).map(|m| m.len() > 0).unwrap_or(false);
    let connection = Connection::open(path).map_err(|error| format!("无法打开生词本：{error}"))?;
    connection.busy_timeout(std::time::Duration::from_secs(5)).map_err(|e| e.to_string())?;
    if existing { validate_schema(&connection)?; }
    initialize_schema(&connection)?;
    crate::sync::init(&connection)?;
    Ok(connection)
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
        sync_version: row.get(7)?,
    })
}

pub fn entry_by_id(connection: &Connection, id: i64) -> Result<VocabularyEntry, String> {
    connection
        .query_row(
            "SELECT id, word, phonetic, definition_markdown, examples_markdown,
                    created_at, updated_at,
                    (SELECT version FROM aurora_sync_records WHERE local_id=vocabulary_entries.id)
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
                    created_at, updated_at,
                    (SELECT version FROM aurora_sync_records WHERE local_id=vocabulary_entries.id)
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

pub fn list_entries(path: &Path, search: Option<String>) -> Result<Vec<VocabularyEntry>, String> {
    let connection = connect(path)?;
    let search = search.unwrap_or_default().trim().to_string();
    let (sql, parameter) = if search.is_empty() {
        (
            "SELECT id, word, phonetic, definition_markdown, examples_markdown,
                    created_at, updated_at,
                    (SELECT version FROM aurora_sync_records WHERE local_id=vocabulary_entries.id)
             FROM vocabulary_entries ORDER BY updated_at DESC, word COLLATE NOCASE",
            None,
        )
    } else {
        (
            "SELECT id, word, phonetic, definition_markdown, examples_markdown,
                    created_at, updated_at,
                    (SELECT version FROM aurora_sync_records WHERE local_id=vocabulary_entries.id)
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

pub fn add_entry(path: &Path, entry: VocabularyEntryInput) -> Result<VocabularyEntry, String> {
    let entry = validate_input(&entry)?;
    let connection = connect(path)?;
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

pub fn save_entry(path: &Path, entry: VocabularyEntryInput) -> Result<VocabularyEntry, String> {
    save_entry_checked(path, entry, None)
}

pub fn save_entry_checked(path: &Path, entry: VocabularyEntryInput, expected: Option<&str>) -> Result<VocabularyEntry, String> {
    let entry = validate_input(&entry)?;
    let id = entry
        .id
        .ok_or_else(|| "缺少要保存的生词编号。".to_string())?;
    let mut db = connect(path)?;
    let connection = db.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate).map_err(|e|e.to_string())?;
    if let Some(expected) = expected {
        let current = entry_by_id(&connection, id)?;
        if current.sync_version != expected {
            return Err("此笔记已在其他设备更新，未覆盖新版。请保留当前草稿，重新打开笔记后合并。".into());
        }
    }
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
    let saved = entry_by_id(&connection, id)?;
    connection.commit().map_err(|e|e.to_string())?;
    Ok(saved)
}

pub fn delete_entry(path: &Path, id: i64) -> Result<(), String> {
    let connection = connect(path)?;
    connection
        .execute("DELETE FROM vocabulary_entries WHERE id = ?1", [id])
        .map_err(|error| format!("无法删除生词：{error}"))?;
    Ok(())
}

fn validate_vocabulary_file(path: &Path) -> Result<i64, String> {
    validate_connection(&read_only(path)?)
}

fn validate_schema(connection: &Connection) -> Result<(), String> {
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
    connection.prepare(
        "SELECT id, word, phonetic, definition_markdown, examples_markdown, created_at, updated_at
         FROM vocabulary_entries LIMIT 0",
    ).map_err(|_| "所选文件缺少生词数据表或字段。".to_string())?;
    let table: Option<String> = connection.query_row(
        "SELECT type FROM sqlite_master WHERE name='vocabulary_entries'", [], |row| row.get(0))
        .optional().map_err(|e| e.to_string())?;
    if table.as_deref() != Some("table") { return Err("生词本缺少可写的数据表。".into()); }
    connection.prepare("INSERT INTO vocabulary_entries(word, created_at, updated_at) VALUES ('', 0, 0) ON CONFLICT(word) DO NOTHING")
        .map_err(|_| "生词本缺少单词唯一性约束。".to_string())?;
    Ok(())
}

fn validate_connection(connection: &Connection) -> Result<i64, String> {
    let integrity: String = connection
        .query_row("PRAGMA quick_check", [], |row| row.get(0))
        .map_err(|error| format!("无法校验 SQLite 文件：{error}"))?;
    if integrity != "ok" {
        return Err(format!("所选 SQLite 文件未通过完整性校验：{integrity}"));
    }
    validate_schema(connection)?;
    let invalid: bool = connection.query_row(
        "SELECT EXISTS(SELECT 1 FROM vocabulary_entries WHERE
         typeof(id) != 'integer' OR typeof(word) != 'text' OR length(trim(word)) = 0 OR
         typeof(phonetic) != 'text' OR typeof(definition_markdown) != 'text' OR
         typeof(examples_markdown) != 'text' OR typeof(created_at) != 'integer' OR
         typeof(updated_at) != 'integer')", [], |row| row.get(0))
        .map_err(|e| format!("无法校验生词内容：{e}"))?;
    if invalid { return Err("生词本包含类型不正确的单词或时间字段。".into()); }
    connection
        .query_row("SELECT COUNT(*) FROM vocabulary_entries", [], |row| {
            row.get(0)
        })
        .map_err(|_| "所选文件缺少生词数据表。".to_string())
}

pub fn status(path: &Path, backup_path: Option<PathBuf>) -> Result<VocabularyBookStatus, String> {
    Ok(VocabularyBookStatus {
        path: path.to_string_lossy().into_owned(),
        entry_count: validate_vocabulary_file(path)?,
        backup_path: backup_path.map(|value| value.to_string_lossy().into_owned()),
    })
}

fn read_only(path: &Path) -> Result<Connection, String> {
    Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)
        .map_err(|error| format!("无法打开所选 SQLite 文件：{error}"))
}

fn parent_directory(path: &Path) -> &Path {
    path.parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."))
}

fn same_path(source: &Path, destination: &Path) -> bool {
    source == destination
        || matches!(
            (source.canonicalize(), destination.canonicalize()),
            (Ok(source), Ok(destination)) if source == destination
        )
}

// SQLite copies a consistent snapshot, including commits still in the WAL.
fn snapshot(source: &Path, directory: &Path, prefix: &str) -> Result<NamedTempFile, String> {
    let source = read_only(source)?;
    validate_connection(&source)?;
    fs::create_dir_all(directory).map_err(|error| format!("无法创建生词本目录：{error}"))?;
    let staged = Builder::new()
        .prefix(prefix)
        .suffix(".sqlite3")
        .tempfile_in(directory)
        .map_err(|error| format!("无法创建生词本副本：{error}"))?;
    source
        .backup(DatabaseName::Main, staged.path(), None)
        .map_err(|error| format!("无法复制生词本：{error}"))?;
    validate_vocabulary_file(staged.path())?;
    staged
        .as_file()
        .sync_all()
        .map_err(|error| format!("无法写入生词本副本：{error}"))?;
    Ok(staged)
}

fn backup_existing(path: &Path) -> Result<PathBuf, String> {
    let stem = path
        .file_stem()
        .and_then(|name| name.to_str())
        .unwrap_or("aurora-vocabulary");
    let prefix = format!("{stem}.backup-{}-", now_millis());
    let (_, backup) = snapshot(path, parent_directory(path), &prefix)?
        .keep()
        .map_err(|error| format!("无法保留生词本备份：{error}"))?;
    Ok(backup)
}

// Avoid replacing a main database file while a WAL journal is still in use.
fn ensure_replaceable(destination: &Path) -> Result<(), String> {
    for suffix in ["-wal", "-shm", "-journal"] {
        let mut sidecar = destination.as_os_str().to_os_string();
        sidecar.push(suffix);
        if Path::new(&sidecar)
            .try_exists()
            .map_err(|error| format!("无法检查目标生词本：{error}"))?
        {
            return Err("目标生词本仍有 SQLite 日志，请关闭使用该文件的应用后重试。".into());
        }
    }
    Ok(())
}

fn publish_snapshot(staged: NamedTempFile, destination: &Path) -> Result<(), String> {
    ensure_replaceable(destination)?;
    staged
        .persist_noclobber(destination)
        .map_err(|error| format!("无法保存生词本副本：{error}"))?;
    Ok(())
}

pub fn export(source: &Path, destination: &Path) -> Result<VocabularyBookStatus, String> {
    if same_path(source, destination) {
        return status(source, None);
    }
    if destination.exists() { return import(source, destination); }
    let staged = snapshot(source, parent_directory(destination), ".aurora-export-")?;
    publish_snapshot(staged, destination)?;
    status(destination, None)
}

pub fn import(source: &Path, destination: &Path) -> Result<VocabularyBookStatus, String> {
    if same_path(source, destination) {
        return status(destination, None);
    }
    // Validate the staged snapshot before touching the active book. Copy back
    // through SQLite so a failed import rolls back and existing WAL readers
    // keep a consistent view. The backup remains available after success.
    let staged = snapshot(source, parent_directory(destination), ".aurora-import-")?;
    drop(connect(destination)?);
    let current = Connection::open(destination).map_err(|e| format!("无法打开生词本：{e}"))?;
    current.execute("ATTACH DATABASE ?1 AS incoming_book", [staged.path().to_string_lossy().as_ref()])
        .map_err(|e| format!("无法准备生词本导入：{e}"))?;
    // Hold the write reservation across both recovery snapshot and replacement.
    // Readers still see the committed original and external writers cannot slip
    // an acknowledged change between the backup and the destructive import.
    current.execute_batch("BEGIN IMMEDIATE").map_err(|e| format!("生词本正在使用：{e}"))?;
    let backup = backup_existing(destination)?;
    current.execute_batch(
        "DELETE FROM vocabulary_entries;
         DELETE FROM sqlite_sequence WHERE name = 'vocabulary_entries';
         INSERT INTO vocabulary_entries(id, word, phonetic, definition_markdown, examples_markdown, created_at, updated_at)
         SELECT id, word, phonetic, definition_markdown, examples_markdown, created_at, updated_at FROM incoming_book.vocabulary_entries;
         UPDATE aurora_sync_meta SET applying=0;"
    ).map_err(|e| format!("无法导入生词本：{e}"))?;
    crate::sync::reset(&current)?;
    current.execute_batch("COMMIT;").map_err(|e|e.to_string())?;
    // Dropping an uncommitted connection rolls back on every error path.
    drop(current);
    status(destination, Some(backup))
}

pub fn copy_for_move(source: &Path, destination: &Path) -> Result<VocabularyBookStatus, String> {
    if same_path(source, destination) {
        return status(source, None);
    }
    export(source, destination)
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
        let connection = connect(&path).unwrap();
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

    #[test]
    fn snapshots_include_commits_that_a_reader_keeps_in_the_wal() {
        let directory = tempfile::tempdir().unwrap();
        let source = directory.path().join("active.sqlite3");
        let writer = connect(&source).unwrap();
        let reader = read_only(&source).unwrap();
        reader.execute_batch("BEGIN").unwrap();
        let count = |connection: &Connection| {
            connection
                .query_row("SELECT COUNT(*) FROM vocabulary_entries", [], |row| {
                    row.get::<_, i64>(0)
                })
                .unwrap()
        };
        assert_eq!(count(&reader), 0);
        writer.execute(
            "INSERT INTO vocabulary_entries(word, phonetic, definition_markdown, examples_markdown, created_at, updated_at)
             VALUES ('serendipity', '/ipa/', '## 释义', '## 例句', 10, 20)",
            [],
        ).unwrap();
        assert_eq!(count(&reader), 0);

        let exported = directory.path().join("exported.sqlite3");
        assert_eq!(export(&source, &exported).unwrap().entry_count, 1);
        let target = directory.path().join("target.sqlite3");
        assert_eq!(import(&source, &target).unwrap().entry_count, 1);
        for path in [&exported, &target] {
            let entry = &list_entries(path, None).unwrap()[0];
            assert_eq!(entry.word, "serendipity");
            assert_eq!(entry.phonetic, "/ipa/");
            assert_eq!(entry.definition_markdown, "## 释义");
            assert_eq!(entry.examples_markdown, "## 例句");
            assert_eq!((entry.created_at, entry.updated_at), (10, 20));
        }
    }

    #[test]
    fn import_accepts_a_v1_book_with_a_different_sqlite_page_size() {
        let directory = tempfile::tempdir().unwrap();
        let source = directory.path().join("older.sqlite3");
        let target = directory.path().join("active.sqlite3");
        let older = Connection::open(&source).unwrap();
        older.execute_batch("PRAGMA page_size = 1024;").unwrap();
        initialize_schema(&older).unwrap();
        older.execute_batch(
            "INSERT INTO vocabulary_entries(word, created_at, updated_at) VALUES ('legacy', 10, 20);",
        ).unwrap();
        drop(older);
        drop(connect(&target).unwrap());
        assert_eq!(import(&source, &target).unwrap().entry_count, 1);
        assert_eq!(list_entries(&target, None).unwrap()[0].word, "legacy");
    }
    #[test]
    fn malformed_rows_cannot_replace_the_current_book() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("invalid.db");
        let target = dir.path().join("current.db");
        let db = connect(&source).unwrap();
        db.execute_batch("INSERT INTO vocabulary_entries(word, created_at, updated_at) VALUES ('invalid', 'not-a-time', 1);").unwrap();
        drop(db);
        drop(connect(&target).unwrap());
        assert!(import(&source, &target).is_err());
        assert!(list_entries(&target, None).unwrap().is_empty());
    }

    #[test]
    fn newer_books_are_rejected_before_mutation() {
        let dir = tempfile::tempdir().unwrap(); let path = dir.path().join("newer.db");
        let db = connect(&path).unwrap();
        db.execute("UPDATE vocabulary_metadata SET value='2'", []).unwrap();
        drop(db);
        let before = fs::read(&path).unwrap();
        assert!(connect(&path).is_err());
        assert_eq!(fs::read(path).unwrap(), before);
    }

    #[test]
    fn imported_schema_requires_unique_words() {
        let dir = tempfile::tempdir().unwrap(); let path = dir.path().join("bad.db");
        let db = connect(&path).unwrap();
        db.execute_batch("ALTER TABLE vocabulary_entries RENAME TO old; CREATE TABLE vocabulary_entries AS SELECT * FROM old;").unwrap();
        assert!(validate_connection(&db).is_err());
    }

}
