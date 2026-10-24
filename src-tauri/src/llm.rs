use reqwest::{
    header::{CONTENT_LENGTH, CONTENT_RANGE, ETAG},
    Client, StatusCode,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    fs::{self, File},
    io::Write,
    net::TcpListener,
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::{
        atomic::{AtomicBool, Ordering},
        Mutex, OnceLock,
    },
    time::{Duration, Instant},
};
use tauri::{AppHandle, Emitter, Manager, State};
use tokio::sync::Mutex as AsyncMutex;

#[cfg(target_os = "windows")]
use std::os::windows::process::CommandExt;

const MODEL_DIRECTORY: &str = "local-llm";
const DOWNLOAD_EVENT: &str = "llm-download-progress";
const STREAM_EVENT: &str = "llm-stream-update";

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct PromptConfiguration {
    dictionary_system_prompt: String,
    translation_system_prompt: String,
}

fn prompt_configuration() -> &'static PromptConfiguration {
    static CONFIGURATION: OnceLock<PromptConfiguration> = OnceLock::new();
    CONFIGURATION.get_or_init(|| {
        serde_json::from_str(include_str!("../../config/prompts.json"))
            .expect("config/prompts.json must contain valid Aurora Dict prompts")
    })
}

#[derive(Clone, Copy)]
struct ModelDefinition {
    id: &'static str,
    name: &'static str,
    filename: &'static str,
    hugging_face_url: &'static str,
    mirror_url: &'static str,
}

const MODELS: [ModelDefinition; 3] = [
    ModelDefinition {
        id: "qwen3-0.6b",
        name: "Qwen3-0.6B",
        filename: "Qwen3-0.6B-Q8_0.gguf",
        hugging_face_url: "https://huggingface.co/Qwen/Qwen3-0.6B-GGUF/resolve/main/Qwen3-0.6B-Q8_0.gguf?download=true",
        mirror_url: "https://hf-mirror.com/Qwen/Qwen3-0.6B-GGUF/resolve/main/Qwen3-0.6B-Q8_0.gguf?download=true",
    },
    ModelDefinition {
        id: "qwen3-1.7b",
        name: "Qwen3-1.7B",
        filename: "Qwen3-1.7B-Q8_0.gguf",
        hugging_face_url: "https://huggingface.co/Qwen/Qwen3-1.7B-GGUF/resolve/main/Qwen3-1.7B-Q8_0.gguf?download=true",
        mirror_url: "https://hf-mirror.com/Qwen/Qwen3-1.7B-GGUF/resolve/main/Qwen3-1.7B-Q8_0.gguf?download=true",
    },
    ModelDefinition {
        id: "qwen3-4b",
        name: "Qwen3-4B",
        filename: "Qwen3-4B-Q4_K_M.gguf",
        hugging_face_url: "https://huggingface.co/Qwen/Qwen3-4B-GGUF/resolve/main/Qwen3-4B-Q4_K_M.gguf?download=true",
        mirror_url: "https://hf-mirror.com/Qwen/Qwen3-4B-GGUF/resolve/main/Qwen3-4B-Q4_K_M.gguf?download=true",
    },
];

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LlmModelStatus {
    model_id: String,
    installed: bool,
    size_bytes: u64,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LlmStatus {
    engine_available: bool,
    message: String,
    models: Vec<LlmModelStatus>,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct DownloadProgress {
    model_id: String,
    downloaded_bytes: u64,
    total_bytes: Option<u64>,
    complete: bool,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LlmLookup {
    word: String,
    model_id: String,
    model_name: String,
    content: String,
    note: String,
    performance: LlmPerformance,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LlmTranslation {
    source: String,
    translation: String,
    model_id: String,
    model_name: String,
    note: String,
    performance: LlmPerformance,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LlmPerformance {
    cold_start: bool,
    startup_ms: u64,
    first_token_ms: u64,
    total_ms: u64,
    prompt_tokens: Option<u64>,
    generated_tokens: Option<u64>,
    tokens_per_second: Option<f64>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LlmPrepareResult {
    model_id: String,
    model_name: String,
    cold_start: bool,
    startup_ms: u64,
}

#[derive(Clone, Copy)]
enum LlmOperation {
    Lookup,
    Translation,
    Quiz,
}

impl LlmOperation {
    fn as_str(self) -> &'static str {
        match self {
            Self::Lookup => "lookup",
            Self::Translation => "translation",
            Self::Quiz => "quiz",
        }
    }
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct LlmStreamUpdate<'a> {
    request_id: &'a str,
    operation: &'static str,
    model_id: &'static str,
    model_name: &'static str,
    content: &'a str,
    done: bool,
    reset: bool,
    performance: Option<LlmPerformance>,
}

struct LlmRuntime {
    model_id: String,
    base_url: String,
    child: Child,
}

pub struct LlmManager {
    runtime: Mutex<Option<LlmRuntime>>,
    startup: AsyncMutex<()>,
    downloading: AtomicBool,
}

impl Default for LlmManager {
    fn default() -> Self {
        Self {
            runtime: Mutex::new(None),
            startup: AsyncMutex::new(()),
            downloading: AtomicBool::new(false),
        }
    }
}

impl Drop for LlmManager {
    fn drop(&mut self) {
        if let Ok(runtime) = self.runtime.get_mut() {
            if let Some(mut runtime) = runtime.take() {
                let _ = runtime.child.kill();
            }
        }
    }
}

fn model_definition(model_id: &str) -> Result<ModelDefinition, String> {
    MODELS
        .iter()
        .copied()
        .find(|model| model.id == model_id)
        .ok_or_else(|| "不支持的本地 AI 模型。".to_string())
}

fn model_directory(app: &AppHandle) -> Result<PathBuf, String> {
    let directory = app
        .path()
        .app_data_dir()
        .map_err(|error| format!("无法取得应用数据目录：{error}"))?
        .join(MODEL_DIRECTORY);
    fs::create_dir_all(&directory).map_err(|error| format!("无法创建本地模型目录：{error}"))?;
    Ok(directory)
}

fn model_path(app: &AppHandle, model: ModelDefinition) -> Result<PathBuf, String> {
    Ok(model_directory(app)?.join(model.filename))
}

fn is_downloaded_model(path: &Path) -> bool {
    fs::metadata(path)
        .map(|metadata| metadata.len() > 0)
        .unwrap_or(false)
}

fn resolve_downloaded_model(
    app: &AppHandle,
    requested: ModelDefinition,
) -> Result<(ModelDefinition, PathBuf), String> {
    let requested_path = model_path(app, requested)?;
    if is_downloaded_model(&requested_path) {
        return Ok((requested, requested_path));
    }
    for model in MODELS.iter().copied() {
        if model.id == requested.id {
            continue;
        }
        let path = model_path(app, model)?;
        if is_downloaded_model(&path) {
            return Ok((model, path));
        }
    }
    Err(format!("{} 尚未下载，请在设置中下载模型。", requested.name))
}

fn target_triple() -> &'static str {
    #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
    {
        "aarch64-apple-darwin"
    }
    #[cfg(all(target_os = "macos", target_arch = "x86_64"))]
    {
        "x86_64-apple-darwin"
    }
    #[cfg(all(target_os = "windows", target_arch = "x86_64"))]
    {
        "x86_64-pc-windows-msvc"
    }
    #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
    {
        "x86_64-unknown-linux-gnu"
    }
    #[cfg(not(any(
        all(target_os = "macos", target_arch = "aarch64"),
        all(target_os = "macos", target_arch = "x86_64"),
        all(target_os = "windows", target_arch = "x86_64"),
        all(target_os = "linux", target_arch = "x86_64")
    )))]
    compile_error!("Aurora Dict only supports macOS ARM64/x64, Windows x64, and Linux x64");
}

fn engine_path(app: &AppHandle) -> Option<PathBuf> {
    if let Some(path) = std::env::var_os("AURORA_LLAMA_SERVER") {
        let path = PathBuf::from(path);
        if path.is_file() {
            return Some(path);
        }
    }
    let resource_directory = app.path().resource_dir().ok()?;
    let extension = if cfg!(target_os = "windows") {
        ".exe"
    } else {
        ""
    };
    let mut binary_names = vec![format!("llama-server-{}{}", target_triple(), extension)];
    binary_names.push(format!("llama-server{extension}"));

    let mut directories = vec![
        resource_directory.clone(),
        resource_directory.join("binaries"),
    ];
    #[cfg(target_os = "macos")]
    if let Some(contents_directory) = resource_directory.parent() {
        directories.push(contents_directory.join("MacOS"));
    }
    if let Ok(current_executable) = std::env::current_exe() {
        if let Some(executable_directory) = current_executable.parent() {
            directories.push(executable_directory.to_path_buf());
        }
    }
    directories
        .into_iter()
        .flat_map(|directory| binary_names.iter().map(move |name| directory.join(name)))
        .find(|path| path.is_file())
}

fn runnable_engine(app: &AppHandle) -> Result<PathBuf, String> {
    let path = engine_path(app).ok_or_else(|| {
        "本地 AI 引擎不可用，请重新安装 Aurora Dict，或在开发环境设置 AURORA_LLAMA_SERVER。"
            .to_string()
    })?;
    let mut command = Command::new(&path);
    command
        .arg("--version")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    #[cfg(target_os = "windows")]
    command.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    let status = command
        .status()
        .map_err(|error| format!("本地 AI 引擎无法运行：{error}"))?;
    if !status.success() {
        return Err(
            "本地 AI 引擎无法独立运行，可能缺少 libllama/libggml 运行库。请重新安装包含完整引擎的最新版 Aurora Dict。"
                .into(),
        );
    }
    Ok(path)
}

#[tauri::command]
pub fn llm_status(app: AppHandle) -> Result<LlmStatus, String> {
    let engine = runnable_engine(&app);
    let models = MODELS
        .iter()
        .copied()
        .map(|model| {
            let path = model_path(&app, model)?;
            let size_bytes = fs::metadata(&path)
                .map(|metadata| metadata.len())
                .unwrap_or(0);
            Ok(LlmModelStatus {
                model_id: model.id.into(),
                installed: size_bytes > 0,
                size_bytes,
            })
        })
        .collect::<Result<Vec<_>, String>>()?;
    Ok(LlmStatus {
        engine_available: engine.is_ok(),
        message: engine
            .err()
            .unwrap_or_else(|| "本地 AI 引擎已就绪。".into()),
        models,
    })
}

#[tauri::command]
pub async fn download_llm_model(
    app: AppHandle,
    manager: State<'_, LlmManager>,
    model_id: String,
    download_source: Option<String>,
) -> Result<(), String> {
    let model = model_definition(&model_id)?;
    if manager.downloading.swap(true, Ordering::AcqRel) {
        return Err("已有本地模型正在下载，请等待其完成。".into());
    }
    let result = download_model(&app, model, download_source.as_deref()).await;
    manager.downloading.store(false, Ordering::Release);
    result
}

async fn download_model(
    app: &AppHandle,
    model: ModelDefinition,
    download_source: Option<&str>,
) -> Result<(), String> {
    let download_url = model_download_url(model, download_source)?;
    download_model_from_url(app, model, download_url)
        .await
        .map_err(|error| {
            format!(
                "{error}\n\n{}",
                manual_download_instruction(app, model, download_url)
            )
        })
}

fn model_download_url(
    model: ModelDefinition,
    download_source: Option<&str>,
) -> Result<&'static str, String> {
    match download_source.unwrap_or("mirror") {
        "mirror" => Ok(model.mirror_url),
        "official" => Ok(model.hugging_face_url),
        _ => Err("未知的模型下载地址，请在设置中重新选择。".into()),
    }
}

fn manual_download_instruction(
    app: &AppHandle,
    model: ModelDefinition,
    download_url: &str,
) -> String {
    match model_path(app, model) {
        Ok(path) => format!(
            "自动下载失败。请手动从以下地址下载：\n{}\n\n下载后请保留文件名“{}”，并将文件放入：\n{}\n\n重启 Aurora Dict 或重新打开设置页后即可识别。",
            download_url,
            model.filename,
            path.display()
        ),
        Err(_) => format!(
            "自动下载失败。请手动从以下地址下载，并保留文件名“{}”。应用无法取得本地模型目录，请确认应用数据目录可写。\n{}",
            model.filename, download_url
        ),
    }
}

async fn download_model_from_url(
    app: &AppHandle,
    model: ModelDefinition,
    download_url: &str,
) -> Result<(), String> {
    let destination = model_path(app, model)?;
    if fs::metadata(&destination)
        .map(|metadata| metadata.len() > 0)
        .unwrap_or(false)
    {
        return Ok(());
    }
    let temporary = destination.with_extension("gguf.part");
    let client = Client::builder()
        .user_agent("AuroraDictionary/0.1 local-llm downloader")
        .timeout(Duration::from_secs(60 * 30))
        .build()
        .map_err(|error| format!("无法初始化模型下载：{error}"))?;
    let mut response = client
        .get(download_url)
        .send()
        .await
        .map_err(|error| format!("无法下载 {}：{error}", model.name))?
        .error_for_status()
        .map_err(|error| format!("无法下载 {}：{error}", model.name))?;
    let total_bytes = response
        .content_length()
        .or_else(|| numeric_header(response.headers(), CONTENT_LENGTH.as_str()))
        .or_else(|| numeric_header(response.headers(), "x-linked-size"))
        .or_else(|| content_range_total(response.headers()));
    let expected_sha256 = response
        .headers()
        .get(ETAG)
        .and_then(|value| value.to_str().ok())
        .map(|value| value.trim_matches('"').to_ascii_lowercase())
        .filter(|value| {
            value.len() == 64 && value.chars().all(|character| character.is_ascii_hexdigit())
        });
    let mut file =
        File::create(&temporary).map_err(|error| format!("无法写入模型文件：{error}"))?;
    let mut digest = Sha256::new();
    let mut downloaded_bytes = 0_u64;
    let _ = app.emit(
        DOWNLOAD_EVENT,
        DownloadProgress {
            model_id: model.id.into(),
            downloaded_bytes,
            total_bytes,
            complete: false,
        },
    );
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|error| format!("下载模型时连接中断：{error}"))?
    {
        file.write_all(&chunk)
            .map_err(|error| format!("无法写入模型文件：{error}"))?;
        digest.update(&chunk);
        downloaded_bytes += chunk.len() as u64;
        let _ = app.emit(
            DOWNLOAD_EVENT,
            DownloadProgress {
                model_id: model.id.into(),
                downloaded_bytes,
                total_bytes,
                complete: false,
            },
        );
    }
    file.flush()
        .map_err(|error| format!("无法完成模型写入：{error}"))?;
    if let Some(expected) = expected_sha256 {
        let actual = format!("{:x}", digest.finalize());
        if actual != expected {
            let _ = fs::remove_file(&temporary);
            return Err("模型文件校验失败，下载已取消。".into());
        }
    }
    fs::rename(&temporary, &destination).map_err(|error| format!("无法保存模型文件：{error}"))?;
    let _ = app.emit(
        DOWNLOAD_EVENT,
        DownloadProgress {
            model_id: model.id.into(),
            downloaded_bytes,
            total_bytes,
            complete: true,
        },
    );
    Ok(())
}

fn numeric_header(headers: &reqwest::header::HeaderMap, name: &str) -> Option<u64> {
    headers.get(name)?.to_str().ok()?.trim().parse().ok()
}

fn content_range_total(headers: &reqwest::header::HeaderMap) -> Option<u64> {
    headers
        .get(CONTENT_RANGE)?
        .to_str()
        .ok()?
        .rsplit_once('/')?
        .1
        .trim()
        .parse()
        .ok()
}

#[tauri::command]
pub fn delete_llm_model(
    app: AppHandle,
    manager: State<'_, LlmManager>,
    model_id: String,
) -> Result<(), String> {
    let model = model_definition(&model_id)?;
    if let Ok(mut runtime) = manager.runtime.lock() {
        if runtime
            .as_ref()
            .is_some_and(|current| current.model_id == model.id)
        {
            if let Some(mut current) = runtime.take() {
                let _ = current.child.kill();
            }
        }
    }
    let path = model_path(&app, model)?;
    if path.exists() {
        fs::remove_file(path).map_err(|error| format!("无法删除本地模型：{error}"))?;
    }
    Ok(())
}

#[derive(Serialize)]
struct ChatRequest<'a> {
    model: &'static str,
    messages: Vec<ChatMessage<'a>>,
    temperature: f32,
    top_p: f32,
    max_tokens: u16,
    stream: bool,
}

#[derive(Serialize)]
struct ChatMessage<'a> {
    role: &'static str,
    content: &'a str,
}

#[derive(Deserialize)]
struct ChatResponse {
    choices: Vec<ChatChoice>,
    timings: Option<LlamaTimings>,
}

#[derive(Deserialize)]
struct ChatChoice {
    message: ChatResponseMessage,
}

#[derive(Deserialize)]
struct ChatResponseMessage {
    content: Option<String>,
}

#[derive(Deserialize)]
struct ChatStreamResponse {
    #[serde(default)]
    choices: Vec<ChatStreamChoice>,
    timings: Option<LlamaTimings>,
}

#[derive(Deserialize)]
struct ChatStreamChoice {
    delta: Option<ChatStreamDelta>,
    message: Option<ChatResponseMessage>,
}

#[derive(Deserialize)]
struct ChatStreamDelta {
    content: Option<String>,
}

#[derive(Clone, Copy, Deserialize)]
struct LlamaTimings {
    cache_n: Option<u64>,
    prompt_n: Option<u64>,
    predicted_n: Option<u64>,
    predicted_per_second: Option<f64>,
}

struct CompletionResult {
    content: String,
    performance: LlmPerformance,
}

fn resolved_system_prompt(system_prompt: Option<String>, fallback: &str) -> Result<String, String> {
    let prompt = system_prompt.unwrap_or_default();
    let prompt = prompt.trim();
    if prompt.is_empty() {
        return Ok(fallback.to_string());
    }
    if prompt.chars().count() > 8_000 {
        return Err("本地 AI 系统提示词过长，请限制在 8,000 个字符以内。".into());
    }
    Ok(prompt.to_string())
}

#[tauri::command]
pub async fn prepare_llm(
    app: AppHandle,
    manager: State<'_, LlmManager>,
    model_id: String,
) -> Result<LlmPrepareResult, String> {
    let requested_model = model_definition(&model_id)?;
    let (model, path) = resolve_downloaded_model(&app, requested_model)?;
    let connection = ensure_server(&app, &manager, model, &path).await?;
    Ok(LlmPrepareResult {
        model_id: model.id.into(),
        model_name: model.name.into(),
        cold_start: connection.cold_start,
        startup_ms: connection.startup_ms,
    })
}

#[tauri::command]
pub async fn lookup_llm(
    app: AppHandle,
    manager: State<'_, LlmManager>,
    model_id: String,
    query: String,
    system_prompt: Option<String>,
    request_id: Option<String>,
) -> Result<LlmLookup, String> {
    let started = Instant::now();
    let requested_model = model_definition(&model_id)?;
    let query = query.split_whitespace().collect::<Vec<_>>().join(" ");
    if query.is_empty() {
        return Err("请输入要查询的词语。".into());
    }
    if query.chars().count() > 100 {
        return Err("查询内容过长，请限制在 100 个字符以内。".into());
    }
    let system_prompt = resolved_system_prompt(
        system_prompt,
        &prompt_configuration().dictionary_system_prompt,
    )?;
    let (model, path) = resolve_downloaded_model(&app, requested_model)?;
    let connection = ensure_server(&app, &manager, model, &path).await?;
    let user_prompt = format!("词条：{query}\n请直接给出词典结果。\n/no_think");
    let completion = complete_local_request(
        &app,
        request_id.as_deref().unwrap_or_default(),
        LlmOperation::Lookup,
        model,
        &connection,
        &system_prompt,
        &user_prompt,
        420,
        0.2,
        started,
    )
    .await?;
    Ok(LlmLookup {
        word: query,
        model_id: model.id.into(),
        model_name: model.name.into(),
        content: completion.content,
        note: "由本地 AI 模型生成，仅供语言学习参考。".into(),
        performance: completion.performance,
    })
}

/// Generate an optional memory aid from already-sourced relationships. The
/// generated text is never accepted as an edge in the graph.
#[tauri::command]
pub async fn generate_graph_mnemonic(
    app: AppHandle,
    manager: State<'_, LlmManager>,
    model_id: String,
    word: String,
    evidence: String,
) -> Result<String, String> {
    if word.len() > 40 || !word.bytes().all(|byte| byte.is_ascii_alphabetic()) {
        return Err("仅支持单个英文单词。".into());
    }
    if evidence.len() > 2_000 {
        return Err("关系资料过长。".into());
    }
    let requested_model = model_definition(&model_id)?;
    let (model, path) = resolve_downloaded_model(&app, requested_model)?;
    let connection = ensure_server(&app, &manager, model, &path).await?;
    let prompt = format!("目标词：{word}\n已核实的构词与词汇关系：\n{evidence}\n/no_think");
    let completion = complete_local_request(
        &app, "", LlmOperation::Lookup, model, &connection,
        "你是英语记忆助手。仅根据用户提供的已核实资料写一句简短中文记忆提示。不要补充未列出的词根、词源、近义词或关系；资料不足时只说资料不足。不要将提示写成事实断言。",
        &prompt, 160, 0.2, Instant::now(),
    ).await?;
    if completion.content.chars().count() > 280
        || !mnemonic_uses_known_english(&completion.content, &word, &evidence)
    {
        return Err("本地 AI 提示未通过词汇核验，请重试。".into());
    }
    Ok(completion.content)
}

pub(crate) async fn generate_quiz_completion(
    app: &AppHandle,
    manager: &LlmManager,
    model_id: &str,
    prompt: &str,
    temperature: f32,
) -> Result<String, String> {
    let requested = model_definition(model_id)?;
    let (model, path) = resolve_downloaded_model(app, requested)?;
    let connection = ensure_server(app, manager, model, &path).await?;
    let completion = complete_local_request(
        app, "", LlmOperation::Quiz, model, &connection,
        "Return only one JSON object with keys sentence, translation, explanation.",
        prompt, 280, temperature, Instant::now(),
    ).await?;
    Ok(completion.content)
}

fn mnemonic_uses_known_english(content: &str, word: &str, evidence: &str) -> bool {
    let allowed = evidence
        .split(|character: char| !character.is_ascii_alphabetic())
        .chain(std::iter::once(word))
        .map(str::to_ascii_lowercase)
        .collect::<std::collections::HashSet<_>>();
    content
        .split(|character: char| !character.is_ascii_alphabetic())
        .filter(|token| !token.is_empty())
        .all(|token| allowed.contains(&token.to_ascii_lowercase()))
}

#[tauri::command]
pub async fn translate_llm(
    app: AppHandle,
    manager: State<'_, LlmManager>,
    model_id: String,
    text: String,
    system_prompt: Option<String>,
    request_id: Option<String>,
) -> Result<LlmTranslation, String> {
    let started = Instant::now();
    let requested_model = model_definition(&model_id)?;
    let text = text.trim().to_string();
    if text.is_empty() {
        return Err("请输入要翻译的文本。".into());
    }
    if text.chars().count() > 4_000 {
        return Err("翻译文本过长，请限制在 4,000 个字符以内。".into());
    }
    let system_prompt = resolved_system_prompt(
        system_prompt,
        &prompt_configuration().translation_system_prompt,
    )?;
    let (model, path) = resolve_downloaded_model(&app, requested_model)?;
    let connection = ensure_server(&app, &manager, model, &path).await?;
    let direction = translation_direction(&text);
    let user_prompt = translation_user_prompt(&text, direction, false);
    let request_id = request_id.unwrap_or_default();
    let mut completion = complete_local_request(
        &app,
        &request_id,
        LlmOperation::Translation,
        model,
        &connection,
        &system_prompt,
        &user_prompt,
        1_024,
        0.2,
        started,
    )
    .await?;
    if translation_needs_retry(&text, &completion.content, direction) {
        emit_stream_update(
            &app,
            &request_id,
            LlmOperation::Translation,
            model,
            "",
            false,
            true,
            None,
        );
        let retry_system = format!(
            "{system_prompt}\n\n这是严格的跨语言翻译任务。输出必须使用{}，原样返回输入属于错误。",
            direction.target_language()
        );
        let retry_prompt = translation_user_prompt(&text, direction, true);
        completion = complete_local_request(
            &app,
            &request_id,
            LlmOperation::Translation,
            model,
            &connection,
            &retry_system,
            &retry_prompt,
            1_024,
            0.2,
            started,
        )
        .await?;
    }
    if translation_needs_retry(&text, &completion.content, direction) {
        return Err(
            "本地 AI 返回了原文，未完成跨语言翻译。请重试，或在设置中选择更大的本地模型。".into(),
        );
    }
    Ok(LlmTranslation {
        source: text,
        translation: completion.content,
        model_id: model.id.into(),
        model_name: model.name.into(),
        note: "由本地 AI 模型翻译。".into(),
        performance: completion.performance,
    })
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum TranslationDirection {
    ToEnglish,
    ToChinese,
}

impl TranslationDirection {
    fn source_language(self) -> &'static str {
        match self {
            Self::ToEnglish => "中文",
            Self::ToChinese => "英文",
        }
    }

    fn target_language(self) -> &'static str {
        match self {
            Self::ToEnglish => "英文",
            Self::ToChinese => "简体中文",
        }
    }
}

fn is_han(character: char) -> bool {
    matches!(character, '\u{3400}'..='\u{4dbf}' | '\u{4e00}'..='\u{9fff}' | '\u{f900}'..='\u{faff}')
}

fn translation_direction(text: &str) -> TranslationDirection {
    if text.chars().any(is_han) {
        TranslationDirection::ToEnglish
    } else {
        TranslationDirection::ToChinese
    }
}

fn translation_user_prompt(text: &str, direction: TranslationDirection, strict: bool) -> String {
    let strict_instruction = if strict {
        "上一次错误地复述了原文。这一次必须完成语言转换，绝对不能复制原文。\n"
    } else {
        ""
    };
    format!(
        "/no_think\n任务：将下方 <source> 中的{}内容翻译为{}。\n{}只输出{}译文，不要复述原文，不要解释。\n<source>\n{}\n</source>",
        direction.source_language(),
        direction.target_language(),
        strict_instruction,
        direction.target_language(),
        text
    )
}

fn comparable_translation_text(text: &str) -> String {
    text.chars()
        .filter(|character| character.is_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}

fn translation_needs_retry(
    source: &str,
    translation: &str,
    direction: TranslationDirection,
) -> bool {
    let source_comparable = comparable_translation_text(source);
    let translation_comparable = comparable_translation_text(translation);
    if source_comparable.is_empty() || translation_comparable.is_empty() {
        return true;
    }
    if source_comparable == translation_comparable {
        return true;
    }
    match direction {
        TranslationDirection::ToEnglish => {
            source
                .chars()
                .filter(|character| is_han(*character))
                .count()
                >= 2
                && translation
                    .chars()
                    .filter(|character| character.is_ascii_alphabetic())
                    .count()
                    < 2
        }
        TranslationDirection::ToChinese => {
            source
                .chars()
                .filter(|character| character.is_ascii_alphabetic())
                .count()
                >= 2
                && !translation.chars().any(is_han)
        }
    }
}

#[allow(clippy::too_many_arguments)]
async fn complete_local_request(
    app: &AppHandle,
    request_id: &str,
    operation: LlmOperation,
    model: ModelDefinition,
    connection: &ServerConnection,
    system: &str,
    user_prompt: &str,
    max_tokens: u16,
    temperature: f32,
    started: Instant,
) -> Result<CompletionResult, String> {
    let payload = ChatRequest {
        model: "local-qwen",
        messages: vec![
            ChatMessage {
                role: "system",
                content: system,
            },
            ChatMessage {
                role: "user",
                content: &user_prompt,
            },
        ],
        temperature,
        top_p: 0.85,
        max_tokens,
        stream: true,
    };
    let request_body = serde_json::to_string(&payload)
        .map_err(|error| format!("无法构造本地 AI 请求：{error}"))?;
    let client = Client::builder()
        .no_proxy()
        .timeout(Duration::from_secs(75))
        .build()
        .map_err(|error| format!("无法初始化本地 AI 请求：{error}"))?;
    let endpoint = format!("{}/v1/chat/completions", connection.base_url);
    let mut attempt = 0_u8;
    let mut response = loop {
        let response = client
            .post(&endpoint)
            .header("content-type", "application/json")
            .body(request_body.clone())
            .send()
            .await
            .map_err(|error| format!("本地 AI 模型未能响应：{error}"))?;
        if response.status() == StatusCode::SERVICE_UNAVAILABLE && attempt < 11 {
            attempt += 1;
            tokio::time::sleep(Duration::from_millis(500)).await;
            continue;
        }
        break response
            .error_for_status()
            .map_err(|error| format!("本地 AI 查询失败：{error}"))?;
    };
    let mut pending = Vec::<u8>::new();
    let mut full_response = Vec::<u8>::new();
    let mut raw_content = String::new();
    let mut visible_content = String::new();
    let mut first_token_ms = None;
    let mut timings = None;
    let mut saw_stream_data = false;
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|error| format!("读取本地 AI 流式响应失败：{error}"))?
    {
        full_response.extend_from_slice(&chunk);
        pending.extend_from_slice(&chunk);
        while let Some(newline) = pending.iter().position(|byte| *byte == b'\n') {
            let line = pending.drain(..=newline).collect::<Vec<_>>();
            saw_stream_data |= process_stream_line(
                &line,
                app,
                request_id,
                operation,
                model,
                started,
                &mut raw_content,
                &mut visible_content,
                &mut first_token_ms,
                &mut timings,
            )?;
        }
    }
    if !pending.is_empty() {
        saw_stream_data |= process_stream_line(
            &pending,
            app,
            request_id,
            operation,
            model,
            started,
            &mut raw_content,
            &mut visible_content,
            &mut first_token_ms,
            &mut timings,
        )?;
    }
    if !saw_stream_data {
        let response: ChatResponse = serde_json::from_slice(&full_response)
            .map_err(|error| format!("无法解析本地 AI 响应：{error}"))?;
        timings = response.timings;
        raw_content = response
            .choices
            .into_iter()
            .next()
            .and_then(|choice| choice.message.content)
            .unwrap_or_default();
        visible_content = strip_thinking_content(&raw_content);
        validate_generated_content(&visible_content)?;
        if !visible_content.is_empty() {
            first_token_ms = Some(elapsed_millis(started));
            emit_stream_update(
                app,
                request_id,
                operation,
                model,
                &visible_content,
                false,
                false,
                None,
            );
        }
    }
    let content = strip_thinking_content(&raw_content);
    validate_generated_content(&content)?;
    if content.is_empty() {
        return Err("本地 AI 没有返回可展示的内容。".to_string());
    }
    let total_ms = elapsed_millis(started);
    let performance = performance_from_timings(
        timings,
        connection.cold_start,
        connection.startup_ms,
        first_token_ms.unwrap_or(total_ms),
        total_ms,
    );
    emit_stream_update(
        app,
        request_id,
        operation,
        model,
        &content,
        true,
        false,
        Some(performance.clone()),
    );
    Ok(CompletionResult {
        content,
        performance,
    })
}

#[allow(clippy::too_many_arguments)]
fn process_stream_line(
    line: &[u8],
    app: &AppHandle,
    request_id: &str,
    operation: LlmOperation,
    model: ModelDefinition,
    started: Instant,
    raw_content: &mut String,
    visible_content: &mut String,
    first_token_ms: &mut Option<u64>,
    timings: &mut Option<LlamaTimings>,
) -> Result<bool, String> {
    let line = std::str::from_utf8(line)
        .map_err(|error| format!("本地 AI 返回了无效的 UTF-8 数据：{error}"))?
        .trim();
    let Some(data) = line.strip_prefix("data:") else {
        return Ok(false);
    };
    let data = data.trim();
    if data.is_empty() || data == "[DONE]" {
        return Ok(true);
    }
    let chunk: ChatStreamResponse =
        serde_json::from_str(data).map_err(|error| format!("无法解析本地 AI 流式数据：{error}"))?;
    if let Some(chunk_timings) = chunk.timings {
        *timings = Some(chunk_timings);
    }
    if let Some(content) = chunk.choices.into_iter().find_map(|choice| {
        choice
            .delta
            .and_then(|delta| delta.content)
            .or_else(|| choice.message.and_then(|message| message.content))
    }) {
        raw_content.push_str(&content);
        let next_visible = visible_stream_content(raw_content);
        validate_generated_content(&next_visible)?;
        if next_visible != *visible_content {
            if first_token_ms.is_none() && !next_visible.is_empty() {
                *first_token_ms = Some(elapsed_millis(started));
            }
            *visible_content = next_visible;
            emit_stream_update(
                app,
                request_id,
                operation,
                model,
                visible_content,
                false,
                false,
                None,
            );
        }
    }
    Ok(true)
}

#[allow(clippy::too_many_arguments)]
fn emit_stream_update(
    app: &AppHandle,
    request_id: &str,
    operation: LlmOperation,
    model: ModelDefinition,
    content: &str,
    done: bool,
    reset: bool,
    performance: Option<LlmPerformance>,
) {
    if request_id.is_empty() {
        return;
    }
    let _ = app.emit(
        STREAM_EVENT,
        LlmStreamUpdate {
            request_id,
            operation: operation.as_str(),
            model_id: model.id,
            model_name: model.name,
            content,
            done,
            reset,
            performance,
        },
    );
}

fn performance_from_timings(
    timings: Option<LlamaTimings>,
    cold_start: bool,
    startup_ms: u64,
    first_token_ms: u64,
    total_ms: u64,
) -> LlmPerformance {
    let prompt_tokens = timings.and_then(|value| {
        value
            .prompt_n
            .map(|tokens| tokens.saturating_add(value.cache_n.unwrap_or(0)))
    });
    LlmPerformance {
        cold_start,
        startup_ms,
        first_token_ms,
        total_ms,
        prompt_tokens,
        generated_tokens: timings.and_then(|value| value.predicted_n),
        tokens_per_second: timings
            .and_then(|value| value.predicted_per_second)
            .filter(|speed| speed.is_finite() && *speed > 0.0),
    }
}

fn elapsed_millis(started: Instant) -> u64 {
    started.elapsed().as_millis().min(u64::MAX as u128) as u64
}

fn visible_stream_content(content: &str) -> String {
    let content = content.trim();
    if let Some((_, answer)) = content.rsplit_once("</think>") {
        return clean_answer(answer);
    }
    if "<think>".starts_with(content) || content.starts_with("<think>") {
        return String::new();
    }
    clean_answer(content)
}

fn strip_thinking_content(content: &str) -> String {
    let content = content.trim();
    let without_thinking = content
        .rsplit_once("</think>")
        .map(|(_, answer)| answer)
        .unwrap_or(content);
    clean_answer(without_thinking)
}

// Stop a broken inference stream before it fills the UI or enters the query cache.
fn validate_generated_content(content: &str) -> Result<(), String> {
    let mut previous = None;
    let mut run = 0;
    for character in content.chars() {
        if previous == Some(character) {
            run += 1;
        } else {
            previous = Some(character);
            run = 1;
        }
        if run >= 32 && !character.is_whitespace() {
            return Err("本地 AI 推理异常：输出了大量重复字符。请重启应用后重试；若仍出现，请检查模型文件及推理引擎。".into());
        }
    }
    Ok(())
}

fn clean_answer(content: &str) -> String {
    content
        .trim()
        .trim_start_matches("```text")
        .trim_start_matches("```")
        .trim_end_matches("```")
        .trim()
        .to_string()
}

struct ServerConnection {
    base_url: String,
    cold_start: bool,
    startup_ms: u64,
}

async fn ensure_server(
    app: &AppHandle,
    manager: &LlmManager,
    model: ModelDefinition,
    model_path: &Path,
) -> Result<ServerConnection, String> {
    let started = Instant::now();
    {
        let mut runtime = manager
            .runtime
            .lock()
            .map_err(|_| "本地 AI 运行状态不可用。".to_string())?;
        if let Some(current) = runtime.as_mut() {
            let running = current.child.try_wait().ok().flatten().is_none();
            if running && current.model_id == model.id {
                return Ok(ServerConnection {
                    base_url: current.base_url.clone(),
                    cold_start: false,
                    startup_ms: elapsed_millis(started),
                });
            }
        }
    }
    let _startup_guard = manager.startup.lock().await;
    let previous = {
        let mut runtime = manager
            .runtime
            .lock()
            .map_err(|_| "本地 AI 运行状态不可用。".to_string())?;
        if let Some(current) = runtime.as_mut() {
            let running = current.child.try_wait().ok().flatten().is_none();
            if running && current.model_id == model.id {
                return Ok(ServerConnection {
                    base_url: current.base_url.clone(),
                    cold_start: false,
                    startup_ms: elapsed_millis(started),
                });
            }
        }
        runtime.take()
    };
    if let Some(mut previous) = previous {
        let _ = previous.child.kill();
    }
    let engine = runnable_engine(app)?;
    let listener = TcpListener::bind("127.0.0.1:0")
        .map_err(|error| format!("无法分配本地 AI 端口：{error}"))?;
    let port = listener
        .local_addr()
        .map_err(|error| format!("无法读取本地 AI 端口：{error}"))?
        .port();
    drop(listener);
    let mut command = Command::new(engine);
    // This engine's Metal path produces corrupt output on Intel Iris GPUs.
    // Disabling layers alone still allows KV/host operations to use Metal.
    #[cfg(all(target_os = "macos", target_arch = "x86_64"))]
    command.args([
        "--device",
        "none",
        "--gpu-layers",
        "0",
        "--no-kv-offload",
        "--no-op-offload",
    ]);
    command
        .args([
            "-m",
            model_path.to_string_lossy().as_ref(),
            "--host",
            "127.0.0.1",
            "--port",
            &port.to_string(),
            "--ctx-size",
            "2048",
            "--parallel",
            "1",
            "--jinja",
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    #[cfg(target_os = "windows")]
    command.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    let mut child = command
        .spawn()
        .map_err(|error| format!("无法启动本地 AI 引擎：{error}"))?;
    let base_url = format!("http://127.0.0.1:{port}");
    let client = Client::builder()
        .no_proxy()
        .timeout(Duration::from_secs(1))
        .build()
        .map_err(|error| format!("无法检查本地 AI 引擎：{error}"))?;
    for _ in 0..120 {
        if let Some(status) = child
            .try_wait()
            .map_err(|error| format!("无法读取本地 AI 引擎状态：{error}"))?
        {
            return Err(format!(
                "本地 AI 引擎启动后立即退出（{status}）。安装包中的推理引擎可能不完整或缺少运行库，请重新安装最新版 Aurora Dict。"
            ));
        }
        if client
            .get(format!("{base_url}/health"))
            .send()
            .await
            .is_ok_and(|response| response.status().is_success())
        {
            let mut runtime = manager
                .runtime
                .lock()
                .map_err(|_| "本地 AI 运行状态不可用。".to_string())?;
            *runtime = Some(LlmRuntime {
                model_id: model.id.into(),
                base_url: base_url.clone(),
                child,
            });
            return Ok(ServerConnection {
                base_url,
                cold_start: true,
                startup_ms: elapsed_millis(started),
            });
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
    let _ = child.kill();
    Err("本地 AI 引擎启动超时。请确认模型文件完整，或重新下载模型。".into())
}

#[cfg(test)]
mod tests {
    use super::{
        model_definition, model_download_url, performance_from_timings, prompt_configuration,
        resolved_system_prompt, strip_thinking_content, translation_direction,
        translation_needs_retry, translation_user_prompt, visible_stream_content, LlamaTimings,
        TranslationDirection,
    };

    #[test]
    fn memory_tip_cannot_introduce_unverified_english_words() {
        assert!(super::mnemonic_uses_known_english(
            "kind + -ness 帮助记住 kindness。",
            "kindness",
            "构词：kind + -ness"
        ));
        assert!(!super::mnemonic_uses_known_english(
            "kindness 还和 generosity 有关。",
            "kindness",
            "构词：kind + -ness"
        ));
    }

    #[test]
    fn rejects_repeated_output_even_after_a_valid_prefix() {
        assert!(super::validate_generated_content(&"@".repeat(32)).is_err());
        assert!(
            super::validate_generated_content(&format!("释义：阻止\n{}", "@".repeat(32))).is_err()
        );
        assert!(super::validate_generated_content(&"啊".repeat(32)).is_err());
        assert!(super::validate_generated_content(&"@".repeat(31)).is_ok());
        assert!(super::validate_generated_content("联系：user@example.com；释义：阻止。").is_ok());
        assert!(super::validate_generated_content(&" ".repeat(64)).is_ok());
    }

    #[test]
    fn loads_prompts_from_the_shared_configuration() {
        let configuration = prompt_configuration();
        assert!(configuration.dictionary_system_prompt.contains("发音："));
        assert!(configuration
            .dictionary_system_prompt
            .contains("固定搭配："));
        assert!(configuration
            .translation_system_prompt
            .contains("禁止原样复述或复制输入"));
    }

    #[test]
    fn translation_prompt_uses_an_explicit_target_language() {
        assert_eq!(
            translation_direction("你好，我今天辛勤劳动了一天"),
            TranslationDirection::ToEnglish
        );
        assert_eq!(
            translation_direction("I worked hard today."),
            TranslationDirection::ToChinese
        );
        assert!(
            translation_user_prompt("你好", TranslationDirection::ToEnglish, false)
                .contains("中文内容翻译为英文")
        );
        assert!(
            translation_user_prompt("Hello", TranslationDirection::ToChinese, false)
                .contains("英文内容翻译为简体中文")
        );
    }

    #[test]
    fn rejects_an_untranslated_local_model_response() {
        let source = "你好，我今天辛勤劳动了一天";
        assert!(translation_needs_retry(
            source,
            source,
            TranslationDirection::ToEnglish
        ));
        assert!(!translation_needs_retry(
            source,
            "Hello, I worked hard all day today.",
            TranslationDirection::ToEnglish
        ));
        assert!(!translation_needs_retry(
            "I worked hard today.",
            "我今天工作很努力。",
            TranslationDirection::ToChinese
        ));
    }

    #[test]
    fn keeps_only_the_final_qwen_answer() {
        assert_eq!(
            strip_thinking_content("<think>internal reasoning</think>\n释义：机缘巧合"),
            "释义：机缘巧合"
        );
    }

    #[test]
    fn hides_reasoning_during_streaming_and_reveals_the_answer() {
        assert_eq!(visible_stream_content("<thi"), "");
        assert_eq!(
            visible_stream_content("<think>internal reasoning in progress"),
            ""
        );
        assert_eq!(
            visible_stream_content("<think>internal reasoning</think>\n释义：机缘巧合"),
            "释义：机缘巧合"
        );
    }

    #[test]
    fn exposes_llama_cpp_performance_timings() {
        let performance = performance_from_timings(
            Some(LlamaTimings {
                cache_n: Some(10),
                prompt_n: Some(6),
                predicted_n: Some(24),
                predicted_per_second: Some(18.5),
            }),
            true,
            1_200,
            1_450,
            2_800,
        );
        assert!(performance.cold_start);
        assert_eq!(performance.startup_ms, 1_200);
        assert_eq!(performance.first_token_ms, 1_450);
        assert_eq!(performance.total_ms, 2_800);
        assert_eq!(performance.prompt_tokens, Some(16));
        assert_eq!(performance.generated_tokens, Some(24));
        assert_eq!(performance.tokens_per_second, Some(18.5));
    }

    #[test]
    fn rejects_an_unknown_model_id() {
        assert!(model_definition("not-a-model").is_err());
    }

    #[test]
    fn uses_the_download_source_selected_by_the_user() {
        let model = model_definition("qwen3-0.6b").unwrap();
        assert!(model_download_url(model, Some("mirror"))
            .unwrap()
            .starts_with("https://hf-mirror.com/"));
        assert!(model_download_url(model, Some("official"))
            .unwrap()
            .starts_with("https://huggingface.co/"));
        assert!(model_download_url(model, Some("automatic")).is_err());
    }

    #[test]
    fn uses_custom_or_fallback_system_prompt() {
        assert_eq!(
            resolved_system_prompt(Some("  custom prompt  ".into()), "fallback").unwrap(),
            "custom prompt"
        );
        assert_eq!(
            resolved_system_prompt(Some("   ".into()), "fallback").unwrap(),
            "fallback"
        );
    }

    #[test]
    fn rejects_an_oversized_system_prompt() {
        assert!(resolved_system_prompt(Some("a".repeat(8_001)), "fallback").is_err());
    }
}
