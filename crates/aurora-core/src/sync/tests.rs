use super::*;
fn db() -> (tempfile::TempDir, Connection) {
    let d = tempfile::tempdir().unwrap();
    let c = crate::vocabulary::connect(&d.path().join("book.db")).unwrap();
    (d, c)
}
fn insert(c: &Connection, word: &str, note: &str) {
    c.execute("INSERT INTO vocabulary_entries(word,definition_markdown,created_at,updated_at) VALUES(?1,?2,1,1)",params![word,note]).unwrap();
}
fn request(c: &Connection, changes: Vec<Record>) -> Exchange {
    Exchange {
        protocol: 1,
        book_id: book_id(c).unwrap(),
        cursor: 0,
        changes,
    }
}
#[test]
fn additions_retries_updates_and_deletions() {
    let (_d, mut desktop) = db();
    let (_m, mobile) = db();
    insert(&mobile, "kindness", "手机笔记");
    let r = pending(&mobile).unwrap().remove(0);
    let req = request(&desktop, vec![r.clone()]);
    let first = exchange(&mut desktop, "phone", &req).unwrap();
    let second = exchange(&mut desktop, "phone", &req).unwrap();
    assert_eq!(first.acks[0].record, second.acks[0].record);
    assert_eq!(first.cursor, second.cursor);
    let mut edit = r;
    edit.base_version = first.acks[0].record.version.clone();
    edit.version = "a".repeat(32);
    edit.definition_markdown = "修改后的笔记".into();
    let req = request(&desktop, vec![edit.clone()]);
    let reply = exchange(&mut desktop, "phone", &req).unwrap();
    assert!(reply.conflicts.is_empty());
    edit.base_version = reply.acks[0].record.version.clone();
    edit.version = "b".repeat(32);
    edit.deleted = true;
    let req = request(&desktop, vec![edit]);
    let reply = exchange(&mut desktop, "phone", &req).unwrap();
    assert!(reply.records[0].deleted);
    assert_eq!(
        desktop
            .query_row("SELECT count(*) FROM vocabulary_entries", [], |r| r
                .get::<_, i64>(0))
            .unwrap(),
        0
    );
}
#[test]
fn same_word_first_pairing_preserves_both_and_resolves() {
    let (_d, mut desktop) = db();
    let (_m, mobile) = db();
    insert(&desktop, "Kindness", "电脑笔记");
    insert(&mobile, "kindness", "手机笔记");
    let req = request(&desktop, pending(&mobile).unwrap());
    let reply = exchange(&mut desktop, "phone", &req).unwrap();
    assert_eq!(reply.conflicts.len(), 1);
    let conflict = &reply.conflicts[0];
    assert_eq!(conflict.candidate.definition_markdown, "手机笔记");
    assert_eq!(conflict.remote.definition_markdown, "电脑笔记");
    resolve(&mut desktop, &conflict.id, true, &conflict.remote.version).unwrap();
    assert!(conflicts(&desktop).unwrap().is_empty());
    assert_eq!(
        get(&desktop, &conflict.remote.uid)
            .unwrap()
            .unwrap()
            .definition_markdown,
        "手机笔记"
    );
}
#[test]
fn deleted_on_desktop_and_offline_edit_is_conflict_not_resurrection() {
    let (_d, mut c) = db();
    insert(&c, "apple", "original");
    let mut r = pending(&c).unwrap().remove(0);
    r.base_version = r.version.clone();
    r.version = "a".repeat(32);
    r.definition_markdown = "offline edit".into();
    c.execute("DELETE FROM vocabulary_entries", []).unwrap();
    let req = request(&c, vec![r]);
    let reply = exchange(&mut c, "phone", &req).unwrap();
    assert!(reply.records[0].deleted);
    assert_eq!(reply.conflicts.len(), 1);
}
#[test]
fn wrong_book_invalid_payload_and_rollback_leave_database_unchanged() {
    let (_d, mut c) = db();
    let (_m, m) = db();
    insert(&m, "apple", "note");
    let mut req = request(&c, pending(&m).unwrap());
    req.book_id = "another".into();
    assert!(exchange(&mut c, "phone", &req).is_err());
    req.book_id = book_id(&c).unwrap();
    req.changes[0].word = "".into();
    assert!(exchange(&mut c, "phone", &req).is_err());
    assert_eq!(
        c.query_row("SELECT counter FROM aurora_sync_meta", [], |r| r
            .get::<_, i64>(0))
            .unwrap(),
        0
    );
}
#[test]
fn local_edit_delete_and_backfill_keep_monotonic_versions() {
    let (_d, c) = db();
    insert(&c, "one", "old");
    let a = pending(&c).unwrap().remove(0);
    c.execute(
        "UPDATE vocabulary_entries SET definition_markdown='new'",
        [],
    )
    .unwrap();
    let b = pending(&c).unwrap().remove(0);
    assert_eq!(a.uid, b.uid);
    assert!(b.seq > a.seq);
    assert_ne!(a.version, b.version);
    c.execute("DELETE FROM vocabulary_entries", []).unwrap();
    let d = pending(&c).unwrap().remove(0);
    assert!(d.deleted);
    assert!(d.seq > b.seq);
    init(&c).unwrap();
    assert_eq!(pending(&c).unwrap().len(), 1);
}
#[test]
fn stale_editor_cannot_overwrite_a_synced_note() {
    let (d, c) = db();
    insert(&c, "apple", "original");
    let path = d.path().join("book.db");
    let opened = crate::vocabulary::list_entries(&path, None)
        .unwrap()
        .remove(0);
    c.execute(
        "UPDATE vocabulary_entries SET definition_markdown='saved on phone'",
        [],
    )
    .unwrap();
    let input = crate::vocabulary::VocabularyEntryInput {
        id: Some(opened.id),
        word: opened.word,
        phonetic: opened.phonetic,
        definition_markdown: "stale desktop draft".into(),
        examples_markdown: opened.examples_markdown,
    };
    assert!(
        crate::vocabulary::save_entry_checked(&path, input, Some(&opened.sync_version)).is_err()
    );
    assert_eq!(
        crate::vocabulary::entry_by_id(&c, opened.id)
            .unwrap()
            .definition_markdown,
        "saved on phone"
    );
}

#[test]
fn large_unicode_notes_paginate_without_rejection_or_oversized_responses() {
    let (_d, mut c) = db();
    for i in 0..12 {
        insert(&c, &format!("large-{i}"), &"汉".repeat(100_000));
    }
    let mut req = request(&c, vec![]);
    let mut seen = 0;
    loop {
        let reply = exchange(&mut c, "phone", &req).unwrap();
        assert!(serde_json::to_vec(&reply).unwrap().len() < 3 * 1024 * 1024);
        seen += reply.records.len();
        for record in &reply.records {
            assert!(validate(record).is_ok());
        }
        req.cursor = reply.cursor;
        if !reply.more {
            break;
        }
    }
    assert_eq!(seen, 12);
}
