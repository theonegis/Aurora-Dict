use crate::vocabulary::DesktopVocabulary;
use serde_json::{json, Value};
use std::sync::Arc;
use tauri::{AppHandle, Emitter, Manager, State};
use tokio::sync::Mutex;
#[derive(Default)]
pub struct DesktopSync(pub Mutex<Option<aurora_sync::Host>>);
fn data<R: tauri::Runtime>(app: &AppHandle<R>) -> Result<std::path::PathBuf, String> {
    app.path().app_data_dir().map_err(|e| e.to_string())
}
#[tauri::command]
pub async fn lan_sync_status(state: State<'_, DesktopSync>) -> Result<Value, String> {
    let guard = state.0.lock().await;
    Ok(guard
        .as_ref()
        .map(|h| serde_json::to_value(h.status()).unwrap())
        .unwrap_or(json!({"enabled":false,"peers":[]})))
}
#[tauri::command]
pub async fn lan_sync_enable(
    app: AppHandle,
    state: State<'_, DesktopSync>,
    enabled: bool,
) -> Result<(), String> {
    let mut guard = state.0.lock().await;
    if !enabled {
        guard.take();
        return aurora_sync::disable(&data(&app)?);
    }
    if guard.is_some() {
        return Ok(());
    }
    let book = app
        .state::<DesktopVocabulary>()
        .with_active(|p| Ok(p.to_owned()))?;
    let host = aurora_sync::Host::start(&data(&app)?, book).await?;
    let handle = app.clone();
    host.set_on_change(Arc::new(move || {
        let _ = handle.emit("vocabulary-synced", ());
    }));
    *guard = Some(host);
    Ok(())
}
pub async fn stop_for_file_change<R: tauri::Runtime>(app: &AppHandle<R>) -> Result<(), String> {
    if let Some(state) = app.try_state::<DesktopSync>() {
        state.0.lock().await.take();
        aurora_sync::disable(&data(app)?)?;
    }
    Ok(())
}
#[tauri::command]
pub async fn lan_sync_pairing(
    state: State<'_, DesktopSync>,
    host: Option<String>,
) -> Result<Value, String> {
    let guard = state.0.lock().await;
    let h = guard.as_ref().ok_or("请先开启局域网同步。")?;
    let (pairing, svg) = h.pairing(host)?;
    Ok(json!({"pairing":pairing,"svg":svg}))
}
#[tauri::command]
pub async fn lan_sync_revoke(state: State<'_, DesktopSync>, device: String) -> Result<(), String> {
    state
        .0
        .lock()
        .await
        .as_ref()
        .ok_or("同步未开启")?
        .revoke(&device)
}
#[tauri::command(async)]
pub fn lan_sync_conflicts(
    book: State<'_, DesktopVocabulary>,
) -> Result<Vec<aurora_core::sync::Conflict>, String> {
    book.with_active(|p| aurora_core::sync::conflicts(&aurora_core::vocabulary::connect(p)?))
}
#[tauri::command(async)]
pub fn lan_sync_resolve(
    app: AppHandle,
    book: State<'_, DesktopVocabulary>,
    id: String,
    use_candidate: bool,
    expected_version: String,
) -> Result<(), String> {
    book.with_active(|p| {
        aurora_core::sync::resolve(
            &mut aurora_core::vocabulary::connect(p)?,
            &id,
            use_candidate,
            &expected_version,
        )
    })?;
    let _ = app.emit("vocabulary-synced", ());
    Ok(())
}
