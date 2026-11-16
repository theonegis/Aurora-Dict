use super::*;
use serde_json::{json, Value};
use tauri::{
    ipc::{CallbackFn, InvokeBody},
    test::{get_ipc_response, mock_builder, mock_context, noop_assets, INVOKE_KEY},
    webview::InvokeRequest,
    WebviewWindowBuilder,
};

#[test]
#[ignore = "installs and searches the full bundled ECDICT archive"]
fn desktop_commands_use_the_bundled_dictionary_and_round_trip_vocabulary() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path();
    let archive = Path::new(env!("CARGO_MANIFEST_DIR")).join(BUNDLED_DICTIONARY_ARCHIVE);
    let lexicon = root.join(DATABASE_FILE);
    let dictionary = Dictionary::new(lexicon.clone(), Some(archive));
    dictionary.prepare().unwrap();
    let book = vocabulary::DesktopVocabulary::new(root.to_owned());
    book.prepare().unwrap();
    let app = mock_builder()
        .manage(dictionary)
        .manage(book)
        .invoke_handler(tauri::generate_handler![
            lookup_local,
            suggest_local_words,
            dictionary_status,
            vocabulary::add_vocabulary_entry,
            vocabulary::list_vocabulary_entries,
            vocabulary::save_vocabulary_entry,
            vocabulary::export_vocabulary_book,
            vocabulary::import_vocabulary_book,
            vocabulary::delete_vocabulary_entry,
            vocabulary::move_vocabulary_book,
            vocabulary::open_vocabulary_book,
        ])
        .build(mock_context(noop_assets()))
        .unwrap();
    let window = WebviewWindowBuilder::new(&app, "main", Default::default())
        .build()
        .unwrap();
    let invoke = |cmd: &str, body: Value| -> Result<Value, Value> {
        let origin = if cfg!(target_os = "windows") {
            "http://tauri.localhost"
        } else {
            "tauri://localhost"
        };
        get_ipc_response(
            &window,
            InvokeRequest {
                cmd: cmd.into(),
                callback: CallbackFn(0),
                error: CallbackFn(1),
                url: origin.parse().unwrap(),
                body: InvokeBody::Json(body),
                headers: Default::default(),
                invoke_key: INVOKE_KEY.to_string(),
            },
        )
        .map(|response| response.deserialize().unwrap())
    };
    let status = invoke("dictionary_status", json!({})).unwrap();
    assert_eq!(status["sampleData"], false);
    assert!(status["entryCount"].as_i64().unwrap() > 100_000);
    let result = invoke("lookup_local", json!({"query": "  SERENDIPITY  "})).unwrap();
    let entry = &result["entries"][0];
    assert_eq!(entry["word"], "serendipity");
    assert!(entry["translation"]
        .as_str()
        .is_some_and(|text| !text.is_empty()));
    assert_eq!(
        invoke("lookup_local", json!({"query": "苹果"})).unwrap()["entries"][0]["word"],
        "appletree"
    );
    let suggestions = invoke("suggest_local_words", json!({"query": "serend"})).unwrap();
    assert!(suggestions["suggestions"]
        .as_array()
        .unwrap()
        .contains(&json!("serendipity")));
    let correction = invoke("suggest_local_words", json!({"query": "applw"})).unwrap();
    assert_eq!(correction["correction"], true);
    assert!(!correction["suggestions"].as_array().unwrap().is_empty());

    let saved = invoke(
        "add_vocabulary_entry",
        json!({"entry": {
            "word": entry["word"], "phonetic": entry["phonetic"],
            "definitionMarkdown": "## 释义\n偶然的发现", "examplesMarkdown": "## 例句\nAn example."
        }}),
    )
    .unwrap();
    let duplicate = invoke(
        "add_vocabulary_entry",
        json!({"entry": {"word": "SERENDIPITY"}}),
    )
    .unwrap();
    assert_eq!(duplicate, saved);
    let mut edit = saved.clone();
    edit["examplesMarkdown"] = json!("## 例句\nAn updated example.");
    let saved = invoke("save_vocabulary_entry", json!({"entry": edit})).unwrap();
    assert_eq!(
        invoke(
            "list_vocabulary_entries",
            json!({"search":"updated example"})
        )
        .unwrap(),
        json!([saved])
    );

    let export = root.join("portable.sqlite3");
    let export_status =
        invoke("export_vocabulary_book", json!({"destinationPath": export})).unwrap();
    assert_eq!(export_status["entryCount"], 1);
    let independent = Connection::open(&export).unwrap();
    let version: String = independent
        .query_row(
            "SELECT value FROM vocabulary_metadata WHERE key='schema_version'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(version, "1");
    assert_eq!(
        aurora_core::vocabulary::entry_by_id(&independent, saved["id"].as_i64().unwrap())
            .unwrap()
            .examples_markdown,
        "## 例句\nAn updated example."
    );
    drop(independent);

    invoke("delete_vocabulary_entry", json!({"id":saved["id"]})).unwrap();
    let replacement = invoke(
        "add_vocabulary_entry",
        json!({"entry":{"word":"backup check"}}),
    )
    .unwrap();
    let corrupt = root.join("corrupt.sqlite3");
    std::fs::write(&corrupt, b"not a database").unwrap();
    assert!(invoke("import_vocabulary_book", json!({"sourcePath":corrupt})).is_err());
    assert_eq!(
        invoke("list_vocabulary_entries", json!({})).unwrap(),
        json!([replacement])
    );

    let imported = invoke("import_vocabulary_book", json!({"sourcePath":export})).unwrap();
    let backup = Path::new(imported["backupPath"].as_str().unwrap());
    assert_eq!(
        serde_json::to_value(aurora_core::vocabulary::list_entries(backup, None).unwrap()).unwrap(),
        json!([replacement])
    );
    let restored = invoke("list_vocabulary_entries", json!({})).unwrap();
    assert_ne!(restored[0]["syncVersion"], saved["syncVersion"], "import must invalidate old editor and sync versions");
    let exported_version = saved["syncVersion"].clone();
    let mut saved = saved;
    saved["syncVersion"] = restored[0]["syncVersion"].clone();
    assert_eq!(restored, json!([saved]));
    let moved = root.join("custom").join("vocabulary.sqlite3");
    invoke("move_vocabulary_book", json!({"destinationPath":moved})).unwrap();
    assert_eq!(
        invoke("list_vocabulary_entries", json!({})).unwrap(),
        json!([saved])
    );
    invoke("open_vocabulary_book", json!({"sourcePath":export})).unwrap();
    saved["syncVersion"] = exported_version;
    assert_eq!(
        invoke("list_vocabulary_entries", json!({})).unwrap(),
        json!([saved])
    );
    assert_eq!(
        Dictionary::new(lexicon, None).status().unwrap().entry_count,
        status["entryCount"].as_i64().unwrap()
    );
}
