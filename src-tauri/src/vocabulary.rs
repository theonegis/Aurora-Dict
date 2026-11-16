use aurora_core::vocabulary::{self, VocabularyBookStatus, VocabularyEntry, VocabularyEntryInput};
use rusqlite::Connection;
use serde::{Deserialize, Serialize};
use std::{
    fs,
    path::{Path, PathBuf},
    sync::Mutex,
};
use tauri::{AppHandle, Manager, State};

const VOCABULARY_FILE: &str = "aurora-vocabulary.sqlite3";
const LOCATION_CONFIG_FILE: &str = "vocabulary-location.json";

#[derive(Deserialize, Serialize)]
struct VocabularyLocation {
    path: String,
}

pub struct DesktopVocabulary {
    data_dir: PathBuf,
    operations: Mutex<()>,
}

impl DesktopVocabulary {
    pub fn new(data_dir: PathBuf) -> Self {
        Self {
            data_dir,
            operations: Mutex::new(()),
        }
    }

    fn location(&self) -> Result<Option<PathBuf>, String> {
        let content = match fs::read_to_string(self.data_dir.join(LOCATION_CONFIG_FILE)) {
            Ok(content) => content,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(format!("无法读取生词本位置配置：{error}")),
        };
        let location: VocabularyLocation = serde_json::from_str(&content)
            .map_err(|error| format!("生词本位置配置已损坏：{error}"))?;
        if location.path.trim().is_empty() {
            return Err("生词本位置配置为空。".into());
        }
        Ok(Some(PathBuf::from(location.path)))
    }

    fn active_path(&self) -> Result<PathBuf, String> {
        match self.location()? {
            Some(path) if !path.exists() => Err(format!(
                "自定义生词本不存在或暂时不可用：{}",
                path.display()
            )),
            Some(path) => Ok(path),
            None => Ok(self.data_dir.join(VOCABULARY_FILE)),
        }
    }

    pub fn prepare(&self) -> Result<(), String> {
        let path = match self.location()? {
            Some(path) if !path.exists() => return Ok(()),
            Some(path) => path,
            None => self.data_dir.join(VOCABULARY_FILE),
        };
        vocabulary::connect(&path).map(drop)
    }

    fn save_location(&self, path: &Path) -> Result<(), String> {
        let content = serde_json::to_vec_pretty(&VocabularyLocation {
            path: path.to_string_lossy().into_owned(),
        })
        .map_err(|error| format!("无法生成生词本位置配置：{error}"))?;
        let staging = self.data_dir.join("vocabulary-location.json.tmp");
        let result = (|| {
            fs::create_dir_all(&self.data_dir)?;
            fs::write(&staging, content)?;
            fs::rename(&staging, self.data_dir.join(LOCATION_CONFIG_FILE))
        })();
        if result.is_err() {
            let _ = fs::remove_file(staging);
        }
        result.map_err(|error| format!("无法保存生词本位置配置：{error}"))
    }

    pub(crate) fn with_active<T>(&self, run: impl FnOnce(&Path) -> Result<T, String>) -> Result<T, String> {
        let _operation = self
            .operations
            .lock()
            .map_err(|_| "无法取得生词本操作状态。")?;
        run(&self.active_path()?)
    }
}

// The quiz still queries SQL directly until its rules are extracted.
pub(crate) fn with_active_connection<T>(
    app: &AppHandle,
    run: impl FnOnce(&Connection) -> Result<T, String>,
) -> Result<T, String> {
    app.state::<DesktopVocabulary>()
        .with_active(|path| run(&vocabulary::connect(path)?))
}

#[tauri::command(async)]
pub fn list_vocabulary_entries(
    book: State<'_, DesktopVocabulary>,
    search: Option<String>,
) -> Result<Vec<VocabularyEntry>, String> {
    book.with_active(|path| vocabulary::list_entries(path, search))
}

#[tauri::command(async)]
pub fn add_vocabulary_entry(
    book: State<'_, DesktopVocabulary>,
    entry: VocabularyEntryInput,
) -> Result<VocabularyEntry, String> {
    book.with_active(|path| vocabulary::add_entry(path, entry))
}

#[tauri::command(async)]
pub fn save_vocabulary_entry(
    book: State<'_, DesktopVocabulary>,
    entry: VocabularyEntryInput,
    expected_version: Option<String>,
) -> Result<VocabularyEntry, String> {
    book.with_active(|path| vocabulary::save_entry_checked(path, entry, expected_version.as_deref()))
}

#[tauri::command(async)]
pub fn delete_vocabulary_entry(book: State<'_, DesktopVocabulary>, id: i64) -> Result<(), String> {
    book.with_active(|path| vocabulary::delete_entry(path, id))
}

#[tauri::command(async)]
pub fn vocabulary_book_status(
    book: State<'_, DesktopVocabulary>,
) -> Result<VocabularyBookStatus, String> {
    book.with_active(|path| {
        drop(vocabulary::connect(path)?);
        vocabulary::status(path, None)
    })
}

#[tauri::command(async)]
pub fn export_vocabulary_book(
    book: State<'_, DesktopVocabulary>,
    destination_path: String,
) -> Result<VocabularyBookStatus, String> {
    if destination_path.is_empty() {
        return Err("没有选择导出位置。".into());
    }
    book.with_active(|path| {
        drop(vocabulary::connect(path)?);
        vocabulary::export(path, Path::new(&destination_path))
    })
}

#[tauri::command(async)]
pub async fn import_vocabulary_book<R: tauri::Runtime>(
    app: AppHandle<R>,
    book: State<'_, DesktopVocabulary>,
    source_path: String,
) -> Result<VocabularyBookStatus, String> {
    crate::lan_sync::stop_for_file_change(&app).await?;
    if source_path.is_empty() {
        return Err("没有选择要导入的文件。".into());
    }
    book.with_active(|path| vocabulary::import(Path::new(&source_path), path))
}

#[tauri::command(async)]
pub async fn move_vocabulary_book<R: tauri::Runtime>(
    app: AppHandle<R>,
    book: State<'_, DesktopVocabulary>,
    destination_path: String,
) -> Result<VocabularyBookStatus, String> {
    crate::lan_sync::stop_for_file_change(&app).await?;
    if destination_path.is_empty() {
        return Err("没有选择新的生词本位置。".into());
    }
    let destination = Path::new(&destination_path);
    book.with_active(|path| {
        drop(vocabulary::connect(path)?);
        let status = vocabulary::copy_for_move(path, destination)?;
        book.save_location(destination)?;
        Ok(status)
    })
}

#[tauri::command(async)]
pub async fn open_vocabulary_book<R: tauri::Runtime>(
    app: AppHandle<R>,
    book: State<'_, DesktopVocabulary>,
    source_path: String,
) -> Result<VocabularyBookStatus, String> {
    crate::lan_sync::stop_for_file_change(&app).await?;
    if source_path.is_empty() {
        return Err("没有选择生词本文件。".into());
    }
    let source = Path::new(&source_path);
    let _operation = book
        .operations
        .lock()
        .map_err(|_| "无法取得生词本操作状态。")?;
    let status = vocabulary::status(source, None)?;
    book.save_location(source)?;
    Ok(status)
}
