use std::{
    collections::HashSet,
    time::{SystemTime, UNIX_EPOCH},
};

use rusqlite::Connection;
use serde::{Deserialize, Serialize};
use tauri::{AppHandle, State};

use crate::{
    database,
    llm::{self, LlmManager},
    vocabulary::{self, VocabularyEntry},
};

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct QuizQuestion {
    entry_id: i64,
    word: String,
    sentence: String,
    translation: String,
    options: [String; 4],
    correct_index: usize,
    explanation: String,
}

#[derive(Deserialize)]
struct GeneratedContent {
    sentence: String,
    translation: String,
    explanation: String,
}

fn eligible(word: &str) -> bool {
    let word = word.trim();
    !word.is_empty()
        && word.len() <= 40
        && word.split('\'').count() <= 2
        && word
            .split('\'')
            .all(|part| !part.is_empty() && part.bytes().all(|b| b.is_ascii_alphabetic()))
}

fn word_spans(text: &str, word: &str) -> Vec<(usize, usize)> {
    let mut spans = Vec::new();
    let lower = text.to_ascii_lowercase();
    let needle = word.to_ascii_lowercase();
    for (start, _) in lower.match_indices(&needle) {
        let end = start + needle.len();
        let before = start == 0 || !lower.as_bytes()[start - 1].is_ascii_alphabetic();
        let after = end == lower.len() || !lower.as_bytes()[end].is_ascii_alphabetic();
        if before && after {
            spans.push((start, end));
        }
    }
    spans
}

fn parse_content(raw: &str) -> Result<GeneratedContent, String> {
    let text = raw.trim();
    let text = text
        .strip_prefix("```json")
        .or_else(|| text.strip_prefix("```"))
        .unwrap_or(text);
    let text = text.strip_suffix("```").unwrap_or(text).trim();
    if let Ok(content) = serde_json::from_str(text) {
        return Ok(content);
    }
    if let (Some(start), Some(end)) = (text.find('{'), text.rfind('}')) {
        if start < end {
            if let Ok(content) = serde_json::from_str(&text[start..=end]) {
                return Ok(content);
            }
        }
    }
    Err("本地模型未返回有效的题目格式。".to_string())
}

fn conceal_target(text: &str, word: &str) -> String {
    let spans = word_spans(text, word);
    if spans.is_empty() {
        return text.to_string();
    }
    let mut concealed = String::new();
    let mut previous = 0;
    for (start, end) in spans {
        concealed.push_str(&text[previous..start]);
        concealed.push_str("这个词");
        previous = end;
    }
    concealed.push_str(&text[previous..]);
    concealed.trim().to_string()
}

fn build_question(
    entry: &VocabularyEntry,
    options: [String; 4],
    correct_index: usize,
    raw: &str,
) -> Result<QuizQuestion, String> {
    let generated = parse_content(raw)?;
    let sentence = generated.sentence.trim();
    let translation = conceal_target(generated.translation.trim(), &entry.word);
    let explanation = conceal_target(generated.explanation.trim(), &entry.word);
    if sentence.len() > 500
        || sentence.len() < 10
        || translation.len() > 500
        || explanation.len() > 600
    {
        return Err("生成的题目长度不合适。".into());
    }
    let spans = word_spans(sentence, &entry.word);
    if spans.len() != 1 {
        return Err("生成的句子必须且只能包含一次目标词。".into());
    }
    if options
        .iter()
        .enumerate()
        .any(|(index, option)| index != correct_index && !word_spans(sentence, option).is_empty())
    {
        return Err("生成的句子泄露了其他选项。".into());
    }
    if !translation
        .chars()
        .any(|c| ('\u{4e00}'..='\u{9fff}').contains(&c))
        || !explanation
            .chars()
            .any(|c| ('\u{4e00}'..='\u{9fff}').contains(&c))
        || !word_spans(&translation, &entry.word).is_empty()
        || !word_spans(&explanation, &entry.word).is_empty()
    {
        return Err("中文译文或解析不符合要求，或泄露了答案。".into());
    }
    let (start, end) = spans[0];
    Ok(QuizQuestion {
        entry_id: entry.id,
        word: entry.word.clone(),
        sentence: format!("{}____{}", &sentence[..start], &sentence[end..]),
        translation: translation.into(),
        options,
        correct_index,
        explanation: explanation.into(),
    })
}

fn primary_pos(pos: &str) -> Option<String> {
    pos.split(|c: char| !c.is_ascii_alphabetic())
        .find(|part| !part.is_empty())
        .map(str::to_ascii_lowercase)
}

fn add_candidate(
    candidates: &mut Vec<(String, bool, bool)>,
    seen: &mut HashSet<String>,
    target_pos: Option<&str>,
    word: String,
    pos: Option<String>,
    from_book: bool,
) {
    let lowered = word.to_ascii_lowercase();
    if eligible(&word) && usable_distractor(&lowered) && seen.insert(lowered) {
        let candidate_pos = pos.as_deref().and_then(primary_pos);
        let same_pos = target_pos
            .zip(candidate_pos.as_deref())
            .is_some_and(|(a, b)| a == b);
        candidates.push((word, from_book, same_pos));
    }
}

fn candidate_words(
    book: &Connection,
    dictionary: &Connection,
    target: &str,
) -> Result<Vec<String>, String> {
    let target_pos: Option<String> = dictionary
        .query_row(
            "SELECT pos FROM stardict WHERE word = ?1 COLLATE NOCASE LIMIT 1",
            [target],
            |row| row.get(0),
        )
        .ok()
        .flatten()
        .and_then(|pos: String| primary_pos(&pos));
    let mut candidates = Vec::<(String, bool, bool)>::new();
    let mut seen = HashSet::from([target.to_ascii_lowercase()]);
    let mut statement = book.prepare("SELECT word FROM vocabulary_entries WHERE word != ?1 COLLATE NOCASE ORDER BY created_at DESC LIMIT 100")
        .map_err(|error| format!("无法读取生词本选项：{error}"))?;
    let words = statement
        .query_map([target], |row| row.get::<_, String>(0))
        .map_err(|error| format!("无法读取生词本选项：{error}"))?;
    for word in words {
        let word = word.map_err(|error| error.to_string())?;
        let known = dictionary
            .query_row(
                "SELECT pos, translation FROM stardict WHERE word = ?1 COLLATE NOCASE LIMIT 1",
                [&word],
                |row| {
                    Ok((
                        row.get::<_, Option<String>>(0)?,
                        row.get::<_, Option<String>>(1)?,
                    ))
                },
            )
            .ok();
        if let Some((pos, Some(translation))) = known {
            if !translation.trim().is_empty() {
                add_candidate(
                    &mut candidates,
                    &mut seen,
                    target_pos.as_deref(),
                    word,
                    pos,
                    true,
                );
            }
        }
    }
    if candidates.len() < 12 {
        let max_id: i64 = dictionary
            .query_row("SELECT COALESCE(MAX(id), 0) FROM stardict", [], |row| {
                row.get(0)
            })
            .map_err(|error| format!("无法读取本地词典：{error}"))?;
        let seed = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos() as i64;
        for pass in 0..5_i64 {
            let start = if max_id > 0 && pass < 4 {
                (seed.wrapping_add(pass.wrapping_mul(7919)) & i64::MAX) % max_id
            } else {
                0
            };
            let mut statement = dictionary.prepare("SELECT word, pos FROM stardict WHERE id >= ?1 AND translation IS NOT NULL AND translation != '' ORDER BY id LIMIT 300")
                .map_err(|error| format!("无法读取本地词典：{error}"))?;
            let rows = statement
                .query_map([start], |row| {
                    Ok((row.get::<_, String>(0)?, row.get::<_, Option<String>>(1)?))
                })
                .map_err(|error| format!("无法读取本地词典：{error}"))?;
            for row in rows {
                let (word, pos) = row.map_err(|error| error.to_string())?;
                if word.len().abs_diff(target.len()) <= 5 {
                    add_candidate(
                        &mut candidates,
                        &mut seen,
                        target_pos.as_deref(),
                        word,
                        pos,
                        false,
                    );
                }
            }
            if candidates.len() >= 12 {
                break;
            }
        }
    }
    candidates.sort_by_key(|(word, from_book, same_pos)| {
        (!from_book, !same_pos, word.len().abs_diff(target.len()))
    });
    if candidates.len() < 3 {
        return Err("生词本和本地词典中可用的备选单词不足。".into());
    }
    Ok(candidates
        .into_iter()
        .take(3)
        .map(|(word, _, _)| word)
        .collect())
}

const FUNCTION_WORDS: &[&str] = &[
    "about", "above", "across", "after", "again", "against", "already", "also", "although",
    "always", "among", "another", "anything", "around", "because", "before", "behind", "being",
    "below", "beside", "besides", "between", "both", "could", "during", "each", "either", "else",
    "every", "everything", "from", "have", "however", "into", "just", "like", "many", "might",
    "more", "most", "much", "must", "never", "often", "only", "onto", "other", "over", "people",
    "shall", "should", "some", "someone", "something", "still", "such", "than", "that", "their",
    "them", "then", "there", "these", "they", "this", "those", "through", "toward", "towards",
    "under", "until", "very", "were", "what", "when", "where", "which", "while", "with", "within",
    "without", "would", "your",
];

fn usable_distractor(word: &str) -> bool {
    word.chars().count() >= 4 && FUNCTION_WORDS.binary_search(&word).is_err()
}

fn definition_hint(markdown: &str) -> String {
    let mut hint = String::new();
    for line in markdown.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') || line.starts_with("```") {
            continue;
        }
        let line = line.trim_start_matches(['-', '*', '+']).trim();
        if line.is_empty() {
            continue;
        }
        if !hint.is_empty() {
            hint.push(' ');
        }
        hint.push_str(line);
        if hint.chars().count() >= 180 {
            break;
        }
    }
    if hint.is_empty() {
        return "（词义未记录）".into();
    }
    hint.chars().take(180).collect()
}

fn quiz_prompt(word: &str, definition: &str, banned: &str, correction: &str) -> String {
    format!(
        "/no_think\n为目标词写完形填空，只输出 JSON。\n目标词：{word}\n词义：{definition}\n句子不能包含：{banned}\n要求：\n- sentence 是一句新的英文，必须完整包含单词 {word} 一次，不要用其他词形，不要省略这个词。\n- translation 与 explanation 只能用简体中文。提到目标词时写“这个词”，不要写任何英文字母。\n正确示例的形状：\n{{\"sentence\":\"Her kindness warmed the quiet room.\",\"translation\":\"她的善意温暖了安静的房间。\",\"explanation\":\"语境需要表示善意的名词，所以选这个词。\"}}{correction}"
    )
}

fn options_for(target: &str, distractors: Vec<String>, seed: usize) -> [String; 4] {
    let mut options = [
        target.to_string(),
        distractors[0].clone(),
        distractors[1].clone(),
        distractors[2].clone(),
    ];
    options.rotate_right(seed % 4);
    options
}

#[tauri::command]
pub async fn generate_vocabulary_quiz_question(
    app: AppHandle,
    manager: State<'_, LlmManager>,
    entry_id: i64,
    model_id: String,
) -> Result<QuizQuestion, String> {
    let (_, book) = vocabulary::open_active_vocabulary(&app)?;
    let entry = vocabulary::entry_by_id(&book, entry_id)?;
    if !eligible(&entry.word) {
        return Err("只能用单个英文单词出题。".into());
    }
    let dictionary = database(&app)?;
    let distractors = candidate_words(&book, &dictionary, &entry.word)?;
    let seed = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .subsec_nanos() as usize;
    let options = options_for(&entry.word, distractors, seed);
    let correct_index = options.iter().position(|word| word == &entry.word).unwrap();
    let definition = definition_hint(&entry.definition_markdown);
    let banned = options
        .iter()
        .filter(|word| word.as_str() != entry.word)
        .cloned()
        .collect::<Vec<_>>()
        .join(", ");
    let mut last_error = String::from("本地模型未返回有效的题目格式。");
    for attempt in 0..3 {
        let correction = if attempt == 0 {
            String::new()
        } else {
            format!("\n上一次输出不合格：{last_error} 请改正后重新输出 JSON。")
        };
        let prompt = quiz_prompt(&entry.word, &definition, &banned, &correction);
        let temperature = [0.2_f32, 0.55, 0.8][attempt];
        let raw = llm::generate_quiz_completion(&app, &manager, &model_id, &prompt, temperature).await?;
        match build_question(&entry, options.clone(), correct_index, &raw) {
            Ok(question) => return Ok(question),
            Err(error) => last_error = error,
        }
    }
    Err(format!("本地模型连续生成了不符合要求的题目：{last_error}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry() -> VocabularyEntry {
        VocabularyEntry {
            id: 1,
            word: "kindness".into(),
            phonetic: "".into(),
            definition_markdown: "仁慈".into(),
            examples_markdown: "".into(),
            created_at: 1,
            updated_at: 1,
        }
    }

    #[test]
    fn blanks_exactly_one_whole_word_and_rejects_answer_leaks() {
        let choices = [
            "care".into(),
            "kindness".into(),
            "anger".into(),
            "hope".into(),
        ];
        let raw = r#"{"sentence":"Her kindness warmed the room.","translation":"她的善意温暖了整个房间。","explanation":"此处需要表示善意的名词。"}"#;
        let question = build_question(&entry(), choices.clone(), 1, raw).unwrap();
        assert_eq!(question.sentence, "Her ____ warmed the room.");
        assert_eq!(question.correct_index, 1);
        assert!(build_question(
            &entry(),
            choices.clone(),
            1,
            &format!("```json\n{raw}\n```")
        )
        .is_ok());
        assert!(build_question(
            &entry(),
            choices.clone(),
            1,
            &raw.replace("kindness warmed", "kindness and kindness warmed")
        )
        .is_err());
        assert!(build_question(
            &entry(),
            choices.clone(),
            1,
            &raw.replace("Her kindness", "Her kindness and anger")
        )
        .is_err());
        let concealed = build_question(
            &entry(),
            choices.clone(),
            1,
            &raw.replace("她的善意", "kindness 她的善意"),
        )
        .unwrap();
        assert!(!concealed.translation.to_ascii_lowercase().contains("kindness"));
        assert!(concealed.translation.contains("这个词"));
        let wrapped = format!("题目如下：\n{raw}\n希望这道题合适。");
        assert!(build_question(&entry(), choices, 1, &wrapped).is_ok());
    }

    #[test]
    fn skips_function_words_when_collecting_distractors() {
        let mut candidates = Vec::new();
        let mut seen = HashSet::new();
        add_candidate(&mut candidates, &mut seen, None, "because".into(), None, false);
        add_candidate(&mut candidates, &mut seen, None, "the".into(), None, false);
        add_candidate(
            &mut candidates,
            &mut seen,
            None,
            "warmth".into(),
            Some("n".into()),
            false,
        );
        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].0, "warmth");
    }

    #[test]
    fn dictionary_fills_missing_book_distractors_without_duplicates() {
        let book = Connection::open_in_memory().unwrap();
        book.execute_batch("CREATE TABLE vocabulary_entries (word TEXT, created_at INTEGER); INSERT INTO vocabulary_entries VALUES ('kindness', 1);").unwrap();
        let dictionary = Connection::open_in_memory().unwrap();
        dictionary.execute_batch("CREATE TABLE stardict (id INTEGER PRIMARY KEY, word TEXT, pos TEXT, translation TEXT); INSERT INTO stardict VALUES (1, 'kindness', 'n', '善意'), (2, 'sadness', 'n', '悲伤'), (3, 'warmth', 'n', '温暖'), (4, 'patience', 'n', '耐心'), (5, 'charity', 'n', '慈善');").unwrap();
        let words = candidate_words(&book, &dictionary, "kindness").unwrap();
        let options = options_for("kindness", words, 2);
        assert_eq!(options.iter().collect::<HashSet<_>>().len(), 4);
        assert_eq!(options[2], "kindness");
    }
}
