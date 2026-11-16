mod llm;
mod vocabulary;
mod lan_sync;
mod vocabulary_quiz;
mod word_graph;
mod window_surface;

#[cfg(test)]
mod offline_workflow_tests;

use aurora_core::dictionary::{
    contains_chinese, levenshtein, normalise_query, Dictionary, DictionaryStatus, LocalLookup,
    LocalSuggestions,
};
use llm::LlmManager;
use reqwest::Client;
use rusqlite::Connection;
use scraper::{Html, Selector};
use serde::Serialize;
use std::{collections::HashSet, path::Path, time::Duration};
use tauri::{AppHandle, Manager};

const DATABASE_FILE: &str = "aurora-dictionary.sqlite3";
const BUNDLED_DICTIONARY_ARCHIVE: &str = "resources/ecdict-sqlite-28.zip";
const USER_AGENT: &str = "AuroraDictionary/0.1 (desktop dictionary; contact: local-app)";

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

fn desktop_dictionary(app: &AppHandle) -> Result<Dictionary, String> {
    let data_dir = app
        .path()
        .app_data_dir()
        .map_err(|error| format!("无法取得应用数据目录：{error}"))?;
    let development_path = Path::new(env!("CARGO_MANIFEST_DIR")).join(BUNDLED_DICTIONARY_ARCHIVE);
    let mut candidates = Vec::new();
    if let Ok(resource_dir) = app.path().resource_dir() {
        candidates.push(resource_dir.join(BUNDLED_DICTIONARY_ARCHIVE));
        candidates.push(resource_dir.join("ecdict-sqlite-28.zip"));
    }
    candidates.push(development_path);
    let archive = candidates.into_iter().find(|path| path.is_file());
    Ok(Dictionary::new(data_dir.join(DATABASE_FILE), archive))
}

fn database(app: &AppHandle) -> Result<Connection, String> {
    app.state::<Dictionary>().connect()
}

#[tauri::command]
fn lookup_local(
    dictionary: tauri::State<'_, Dictionary>,
    query: String,
) -> Result<LocalLookup, String> {
    dictionary.lookup(&query)
}

#[tauri::command]
fn suggest_local_words(
    dictionary: tauri::State<'_, Dictionary>,
    query: String,
) -> Result<LocalSuggestions, String> {
    dictionary.suggest(&query)
}

#[tauri::command]
fn dictionary_status(dictionary: tauri::State<'_, Dictionary>) -> Result<DictionaryStatus, String> {
    dictionary.status()
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

async fn bounded_response_text(mut response: reqwest::Response) -> Result<String, String> {
    const LIMIT: usize = 4 * 1024 * 1024;
    if response.content_length().is_some_and(|length| length > LIMIT as u64) { return Err("词典响应过大。".into()); }
    let mut body = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(|_| "读取词典响应失败。")? {
        if body.len() + chunk.len() > LIMIT { return Err("词典响应过大。".into()); }
        body.extend_from_slice(&chunk);
    }
    String::from_utf8(body).map_err(|_| "词典响应编码无效。".into())
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
    bounded_response_text(response).await.map_err(|error| format!("无法读取 {source} 的查询结果：{error}"))
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
    client.get("https://dict.youdao.com/").send().await.is_ok_and(|response| response.status().is_success())
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
        let payload = bounded_response_text(response).await?;
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
    let payload = bounded_response_text(response).await.map_err(|_| "无法读取 Merriam-Webster API 响应。".to_string())?;
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
            .then(|| format!("https://dict.youdao.com/dictvoice?type=1&audio={encoded_word}")),
        us_audio: (!is_chinese_query)
            .then(|| format!("https://dict.youdao.com/dictvoice?type=0&audio={encoded_word}")),
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
    let Some(object_start) = javascript_property_offsets(page, "web_trans").into_iter().next() else {
        return Vec::new();
    };
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
    let Some(array_start) = javascript_property_offsets(page, "collins_entries").into_iter().next() else {
        return empty_youdao_collins();
    };
    let Some(collins_data) = balanced_javascript_segment(page, array_start, b'[', b']') else {
        return empty_youdao_collins();
    };

    let definitions = javascript_string_values(collins_data, "tran:")
        .into_iter()
        .map(|value| clean_markup(&value))
        .filter(|value| !value.is_empty())
        .collect::<Vec<_>>();
    let mut examples = Vec::new();
    let mut seen = HashSet::new();
    for start in javascript_property_offsets(collins_data, "sent") {
        let Some(sentences) = balanced_javascript_segment(collins_data, start, b'[', b']') else { continue };
        let mut offset = 1;
        while offset < sentences.len() {
            if sentences.as_bytes()[offset] != b'{' { offset += 1; continue; }
            let Some(sentence) = balanced_javascript_segment(sentences, offset, b'{', b'}') else { break };
            offset += sentence.len();
            let Some(english) = javascript_string_values(sentence, "eng_sent:").into_iter().next() else { continue };
            let english = clean_markup(&english);
            if english.is_empty() || !seen.insert(english.clone()) { continue; }
            let translation = javascript_string_values(sentence, "chn_sent:").into_iter().next().map(|value| clean_markup(&value)).filter(|value| !value.is_empty());
            examples.push(OnlineExample { english, translation, source: Some("Collins".into()) });
        }
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

fn javascript_property_offsets(source: &str, key: &str) -> Vec<usize> {
    let bytes = source.as_bytes();
    let mut output = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        if matches!(bytes[i], b'"' | b'\'' | b'`') {
            let quote = bytes[i]; i += 1;
            while i < bytes.len() { if bytes[i] == b'\\' { i += 2; } else if bytes[i] == quote { i += 1; break; } else { i += 1; } }
            continue;
        }
        if bytes[i].is_ascii_alphabetic() || bytes[i] == b'_' {
            let start = i;
            while i < bytes.len() && (bytes[i].is_ascii_alphanumeric() || matches!(bytes[i], b'_' | b'$')) { i += 1; }
            if &source[start..i] == key {
                let mut j = i;
                while bytes.get(j).is_some_and(u8::is_ascii_whitespace) { j += 1; }
                if bytes.get(j) == Some(&b':') {
                    j += 1;
                    while bytes.get(j).is_some_and(u8::is_ascii_whitespace) { j += 1; }
                    output.push(j);
                }
            }
        } else { i += 1; }
    }
    output
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
    for value_start in javascript_property_offsets(source, key.trim_end_matches(':')) {
        if source.as_bytes().get(value_start) != Some(&b'"') { continue; }
        let mut index = value_start + 1;
        let bytes = source.as_bytes();
        while index < bytes.len() {
            if bytes[index] == b'\\' { index += 2; continue; }
            if bytes[index] == b'"' {
                if let Ok(value) = serde_json::from_str::<String>(&source[value_start..=index]) { values.push(value); }
                break;
            }
            index += 1;
        }
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
    fn collins_keeps_sentence_pairs_and_ignores_quoted_markers() {
        let page = r#"{note:"collins_entries:[]", collins_entries : [{tran: "meaning", sent: [{eng_sent:"First."},{eng_sent:"Second.",chn_sent:"第二句。"}]}]}"#;
        let result = parse_youdao_collins(page);
        assert_eq!(result.examples.len(), 2);
        assert_eq!(result.examples[0].translation, None);
        assert_eq!(result.examples[1].translation.as_deref(), Some("第二句。"));
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

    window_surface::paint_macos_canvas(window, false)?;
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
            let dictionary = desktop_dictionary(app.handle()).map_err(std::io::Error::other)?;
            dictionary.prepare().map_err(std::io::Error::other)?;
            app.manage(dictionary);
            let vocabulary = vocabulary::DesktopVocabulary::new(
                app.path().app_data_dir().map_err(std::io::Error::other)?,
            );
            vocabulary.prepare().map_err(std::io::Error::other)?;
            app.manage(vocabulary);
            app.manage(lan_sync::DesktopSync::default());
            let handle=app.handle().clone();
            tauri::async_runtime::spawn(async move {
                if let Ok(dir)=handle.path().app_data_dir(){
                    if aurora_sync::enabled(&dir){let _=lan_sync::lan_sync_enable(handle.clone(),handle.state::<lan_sync::DesktopSync>(),true).await;}
                }
            });
            if let Some(window) = app.get_webview_window("main") {
                #[cfg(target_os = "linux")]
                configure_linux_native_window(&window).map_err(std::io::Error::other)?;

                // Windows retains its native opaque base. On Linux the CSS
                // canvas is already opaque; the native surface must be clear
                // for the rounded edge and client-rendered shadow.
                #[cfg(target_os = "windows")]
                window
                    .set_background_color(Some((237, 246, 246, 255).into()))
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
            window_surface::sync_window_surface,
            lan_sync::lan_sync_status, lan_sync::lan_sync_enable, lan_sync::lan_sync_pairing, lan_sync::lan_sync_revoke, lan_sync::lan_sync_conflicts, lan_sync::lan_sync_resolve,
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
