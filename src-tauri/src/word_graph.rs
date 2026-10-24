use reqwest::{Client, StatusCode};
use rusqlite::Connection;
use serde::Serialize;
use serde_json::Value;
use std::{collections::HashSet, time::Duration};
use tauri::AppHandle;

use crate::{database, levenshtein, USER_AGENT};

const WORDNET_BASE: &str = "https://en-word.net/api/lemma";
const KAIKKI_BASE: &str = "https://kaikki.org/dictionary/English/meaning";
const MAX_RESPONSE_BYTES: usize = 1_500_000;

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GraphWord {
    pub word: String,
    pub detail: String,
    pub source_url: String,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GraphSense {
    pub id: String,
    pub gloss: String,
    pub part_of_speech: String,
    pub synonyms: Vec<GraphWord>,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GraphMorpheme {
    pub label: String,
    pub components: Vec<String>,
    pub source_url: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WordGraph {
    pub word: String,
    pub senses: Vec<GraphSense>,
    pub morphology: Vec<GraphMorpheme>,
    pub spelling: Vec<GraphWord>,
    pub warnings: Vec<String>,
}

fn valid_word(query: &str) -> Option<String> {
    let word = query.trim().to_ascii_lowercase();
    (word.len() >= 2 && word.len() <= 40 && word.bytes().all(|byte| byte.is_ascii_alphabetic()))
        .then_some(word)
}

fn wordnet_url(word: &str) -> String {
    format!("{WORDNET_BASE}/{word}")
}

fn kaikki_url(word: &str) -> String {
    format!("{KAIKKI_BASE}/{}/{}/{}.jsonl", &word[..1], &word[..2], word)
}

async fn bounded_get(client: &Client, url: &str) -> Result<Option<Vec<u8>>, String> {
    let response = client
        .get(url)
        .send()
        .await
        .map_err(|error| error.to_string())?;
    if response.status() == StatusCode::NOT_FOUND {
        return Ok(None);
    }
    let mut response = response
        .error_for_status()
        .map_err(|error| error.to_string())?;
    if response
        .content_length()
        .is_some_and(|length| length > MAX_RESPONSE_BYTES as u64)
    {
        return Err("response exceeds size limit".into());
    }
    let mut body = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(|error| error.to_string())? {
        if body.len() + chunk.len() > MAX_RESPONSE_BYTES {
            return Err("response exceeds size limit".into());
        }
        body.extend_from_slice(&chunk);
    }
    Ok(Some(body))
}

fn parse_wordnet(word: &str, body: &[u8]) -> Result<Vec<GraphSense>, String> {
    let records: Value = serde_json::from_slice(body).map_err(|error| error.to_string())?;
    let entries = records.as_array().ok_or("unexpected WordNet response")?;
    let source_url = wordnet_url(word);
    let mut senses = Vec::new();
    let mut seen = HashSet::new();
    for entry in entries {
        let Some(id) = entry.get("id").and_then(Value::as_str) else {
            continue;
        };
        if !seen.insert(id.to_string()) {
            continue;
        }
        let gloss = entry
            .get("definition")
            .and_then(Value::as_array)
            .and_then(|items| items.first())
            .and_then(Value::as_str)
            .unwrap_or("")
            .trim()
            .chars()
            .take(200)
            .collect::<String>();
        let part_of_speech = entry
            .get("partOfSpeech")
            .and_then(Value::as_str)
            .unwrap_or("");
        let mut synonyms = Vec::new();
        let mut words = HashSet::new();
        if let Some(members) = entry.get("members").and_then(Value::as_array) {
            for member in members {
                let Some(lemma) = member
                    .as_str()
                    .or_else(|| member.get("lemma").and_then(Value::as_str))
                else {
                    continue;
                };
                let label = lemma.replace('_', " ");
                if label.eq_ignore_ascii_case(word)
                    || label.len() > 48
                    || !words.insert(label.to_lowercase())
                {
                    continue;
                }
                synonyms.push(GraphWord {
                    word: label,
                    detail: gloss.clone(),
                    source_url: source_url.clone(),
                });
                if synonyms.len() == 4 {
                    break;
                }
            }
        }
        if !synonyms.is_empty() {
            senses.push(GraphSense {
                id: id.into(),
                gloss,
                part_of_speech: part_of_speech.into(),
                synonyms,
            });
            if senses.len() == 3 {
                break;
            }
        }
    }
    Ok(senses)
}

fn parse_kaikki(word: &str, body: &[u8]) -> Vec<GraphMorpheme> {
    let url = kaikki_url(word).replace(".jsonl", ".html");
    let mut output = Vec::new();
    let mut seen = HashSet::new();
    for line in body.split(|byte| *byte == b'\n') {
        let Ok(record) = serde_json::from_slice::<Value>(line) else {
            continue;
        };
        if record.get("lang_code").and_then(Value::as_str) != Some("en")
            || !record
                .get("word")
                .and_then(Value::as_str)
                .map_or(false, |value| value.eq_ignore_ascii_case(word))
        {
            continue;
        }
        let Some(templates) = record.get("etymology_templates").and_then(Value::as_array) else {
            continue;
        };
        for template in templates {
            let name = template.get("name").and_then(Value::as_str).unwrap_or("");
            if !matches!(
                name,
                "suf" | "suffix" | "pre" | "prefix" | "af" | "affix" | "compound"
            ) {
                continue;
            }
            let Some(args) = template.get("args").and_then(Value::as_object) else {
                continue;
            };
            if args.get("1").and_then(Value::as_str) != Some("en") {
                continue;
            }
            let mut components = (2..=5)
                .filter_map(|index| args.get(&index.to_string()).and_then(Value::as_str))
                .map(str::trim)
                .filter(|value| !value.is_empty() && value.len() <= 40)
                .map(str::to_string)
                .collect::<Vec<_>>();
            if components.len() < 2 {
                continue;
            }
            if matches!(name, "suf" | "suffix") {
                for component in components.iter_mut().skip(1) {
                    if !component.starts_with('-') {
                        component.insert(0, '-');
                    }
                }
            } else if matches!(name, "pre" | "prefix") {
                for component in components.iter_mut().take(1) {
                    if !component.ends_with('-') {
                        component.push('-');
                    }
                }
            }
            let label = components.join(" + ");
            if seen.insert(label.clone()) {
                output.push(GraphMorpheme {
                    label,
                    components,
                    source_url: url.clone(),
                });
            }
            if output.len() == 3 {
                return output;
            }
        }
    }
    output
}

fn spelling_neighbours(connection: &Connection, word: &str) -> Result<Vec<GraphWord>, String> {
    let prefix = &word[..2];
    let mut statement = connection.prepare(
        "SELECT word, CAST(COALESCE(frq, 0) AS INTEGER) FROM stardict WHERE word LIKE ?1 COLLATE NOCASE AND lower(word) <> ?2 ORDER BY frq DESC LIMIT 600"
    ).map_err(|error| error.to_string())?;
    let candidates = statement
        .query_map((format!("{prefix}%"), word), |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
        })
        .map_err(|error| error.to_string())?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| error.to_string())?;
    let mut ranked = candidates
        .into_iter()
        .filter_map(|(candidate, frequency)| {
            if !candidate.bytes().all(|byte| byte.is_ascii_alphabetic()) {
                return None;
            }
            let distance = levenshtein(word, &candidate.to_ascii_lowercase());
            (distance > 0 && distance <= 2).then_some((candidate, distance, frequency))
        })
        .collect::<Vec<_>>();
    ranked.sort_by(|a, b| {
        a.1.cmp(&b.1)
            .then_with(|| b.2.cmp(&a.2))
            .then_with(|| a.0.cmp(&b.0))
    });
    Ok(ranked
        .into_iter()
        .take(5)
        .map(|(candidate, distance, _)| GraphWord {
            word: candidate,
            detail: format!("编辑距离 {distance}；仅表示拼写相近"),
            source_url: String::new(),
        })
        .collect())
}

#[tauri::command]
pub async fn lookup_word_graph(app: AppHandle, query: String) -> Result<WordGraph, String> {
    let word = valid_word(&query).ok_or("请输入 2–40 个英文字母组成的单词。")?;
    let client = Client::builder()
        .user_agent(USER_AGENT)
        .timeout(Duration::from_secs(7))
        .build()
        .map_err(|error| error.to_string())?;
    let wordnet = bounded_get(&client, &wordnet_url(&word)).await;
    let kaikki = bounded_get(&client, &kaikki_url(&word)).await;
    let mut warnings = Vec::new();
    let senses = match wordnet {
        Ok(Some(body)) => parse_wordnet(&word, &body).unwrap_or_else(|error| {
            warnings.push(format!("WordNet 数据无法解析：{error}"));
            Vec::new()
        }),
        Ok(None) => Vec::new(),
        Err(error) => {
            warnings.push(format!("WordNet 暂不可用：{error}"));
            Vec::new()
        }
    };
    let morphology = match kaikki {
        Ok(Some(body)) => parse_kaikki(&word, &body),
        Ok(None) => Vec::new(),
        Err(error) => {
            warnings.push(format!("Kaikki 暂不可用：{error}"));
            Vec::new()
        }
    };
    let spelling =
        match database(&app).and_then(|connection| spelling_neighbours(&connection, &word)) {
            Ok(words) => words,
            Err(error) => {
                warnings.push(format!("本地拼写词库暂不可用：{error}"));
                Vec::new()
            }
        };
    Ok(WordGraph {
        word,
        senses,
        morphology,
        spelling,
        warnings,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wordnet_keeps_synonyms_inside_their_sense() {
        let body = br#"[{"id":"one","definition":["a kind act"],"partOfSpeech":"n","members":["kindness","benignity"]},{"id":"two","definition":["quality"],"members":["kindness","goodness"]}]"#;
        let senses = parse_wordnet("kindness", body).unwrap();
        assert_eq!(senses.len(), 2);
        assert_eq!(senses[0].synonyms[0].word, "benignity");
        assert_eq!(senses[1].synonyms[0].word, "goodness");
    }

    #[test]
    fn kaikki_accepts_only_structured_english_affixes() {
        let body = br#"{"word":"kindness","lang_code":"en","etymology_text":"kind + -ness","etymology_templates":[{"name":"suffix","args":{"1":"en","2":"kind","3":"-ness"}}]}
{"word":"kindness","lang_code":"en","etymology_templates":[{"name":"foo","args":{"1":"en","2":"fake","3":"ness"}}]}"#;
        let parts = parse_kaikki("kindness", body);
        assert_eq!(parts.len(), 1);
        assert_eq!(parts[0].label, "kind + -ness");
    }

    #[test]
    fn invalid_graph_queries_are_rejected() {
        assert_eq!(valid_word("kindness"), Some("kindness".into()));
        assert_eq!(valid_word("two words"), None);
        assert_eq!(valid_word("../foo"), None);
    }
}
