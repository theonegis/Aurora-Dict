//! Versioned, transactional record sync. Timestamps are display data, never conflict clocks.
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};

pub const SCHEMA: &str = include_str!("schema.sql");
const COLUMNS: &str = "uid,word,phonetic,definition_markdown,examples_markdown,created_at,updated_at,deleted,seq,version,base_version";
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Record {
    pub uid: String,
    pub word: String,
    pub phonetic: String,
    pub definition_markdown: String,
    pub examples_markdown: String,
    pub created_at: i64,
    pub updated_at: i64,
    pub deleted: bool,
    pub seq: i64,
    pub version: String,
    #[serde(default)]
    pub base_version: String,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Conflict {
    pub id: String,
    pub device: String,
    pub candidate: Record,
    pub remote: Record,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Ack {
    pub source_uid: String,
    pub op_id: String,
    pub record: Record,
    pub conflict: Option<Conflict>,
}
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Exchange {
    pub protocol: u32,
    pub book_id: String,
    pub cursor: i64,
    pub changes: Vec<Record>,
}
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Reply {
    pub protocol: u32,
    pub book_id: String,
    pub cursor: i64,
    pub more: bool,
    pub acks: Vec<Ack>,
    pub records: Vec<Record>,
    pub conflicts: Vec<Conflict>,
}

pub fn init(c: &Connection) -> Result<(), String> {
    c.execute_batch(SCHEMA).map_err(|e| e.to_string())
}
pub fn book_id(c: &Connection) -> Result<String, String> {
    c.query_row(
        "SELECT book_id FROM aurora_sync_meta WHERE singleton=1",
        [],
        |r| r.get(0),
    )
    .map_err(|e| e.to_string())
}
/// Called inside the import transaction. Old peers must pair explicitly with the replacement book.
pub fn reset(c: &Connection) -> Result<(), String> {
    c.execute_batch("DELETE FROM aurora_sync_records; DELETE FROM aurora_sync_receipts; DELETE FROM aurora_sync_conflicts; DELETE FROM aurora_sync_client; UPDATE aurora_sync_meta SET book_id=lower(hex(randomblob(16))),counter=0,applying=0;").map_err(|e|e.to_string())?;
    init(c)
}
fn row(r: &rusqlite::Row<'_>) -> rusqlite::Result<Record> {
    Ok(Record {
        uid: r.get(0)?,
        word: r.get(1)?,
        phonetic: r.get(2)?,
        definition_markdown: r.get(3)?,
        examples_markdown: r.get(4)?,
        created_at: r.get(5)?,
        updated_at: r.get(6)?,
        deleted: r.get(7)?,
        seq: r.get(8)?,
        version: r.get(9)?,
        base_version: r.get(10)?,
    })
}
pub fn get(c: &Connection, uid: &str) -> Result<Option<Record>, String> {
    c.query_row(
        &format!("SELECT {COLUMNS} FROM aurora_sync_records WHERE uid=?1"),
        [uid],
        row,
    )
    .optional()
    .map_err(|e| e.to_string())
}
pub fn pending(c: &Connection) -> Result<Vec<Record>, String> {
    let mut q = c
        .prepare(&format!(
            "SELECT {COLUMNS} FROM aurora_sync_records WHERE dirty=1 ORDER BY seq LIMIT 100"
        ))
        .map_err(|e| e.to_string())?;
    let values = q
        .query_map([], row)
        .map_err(|e| e.to_string())?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| e.to_string())?;
    Ok(values)
}
pub fn conflicts(c: &Connection) -> Result<Vec<Conflict>, String> {
    let mut q=c.prepare("SELECT id,device,candidate,remote FROM aurora_sync_conflicts WHERE resolved=0 ORDER BY rowid LIMIT 100").map_err(|e|e.to_string())?;
    let rows = q
        .query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, String>(3)?,
            ))
        })
        .map_err(|e| e.to_string())?;
    rows.map(|r| {
        let (id, device, a, b) = r.map_err(|e| e.to_string())?;
        Ok(Conflict {
            id,
            device,
            candidate: serde_json::from_str(&a).map_err(|e| e.to_string())?,
            remote: {
                let saved: Record = serde_json::from_str(&b).map_err(|e| e.to_string())?;
                get(c, &saved.uid)?.unwrap_or(saved)
            },
        })
    })
    .collect()
}
fn valid_id(s: &str) -> bool {
    s.len() == 32 && s.bytes().all(|b| b.is_ascii_hexdigit())
}
fn validate(r: &Record) -> Result<(), String> {
    if !valid_id(&r.uid)
        || !valid_id(&r.version)
        || (!r.base_version.is_empty() && !valid_id(&r.base_version))
        || r.word.trim().is_empty()
        || r.word != r.word.trim()
        || r.word.chars().count() > 200
        || r.phonetic.len() > 4096
        || r.definition_markdown.chars().count() > 100_000
        || r.examples_markdown.chars().count() > 100_000
        || r.created_at < 0
        || r.updated_at < 0
    {
        return Err("无效的同步词条。".into());
    }
    Ok(())
}
fn same_content(a: &Record, b: &Record) -> bool {
    a.deleted == b.deleted
        && a.word.eq_ignore_ascii_case(&b.word)
        && a.phonetic == b.phonetic
        && a.definition_markdown == b.definition_markdown
        && a.examples_markdown == b.examples_markdown
}
fn apply(c: &Connection, uid: &str, r: &Record) -> Result<Record, String> {
    let mut local: Option<i64> = c
        .query_row(
            "SELECT local_id FROM aurora_sync_records WHERE uid=?1",
            [uid],
            |r| r.get(0),
        )
        .optional()
        .map_err(|e| e.to_string())?
        .flatten();
    c.execute(
        "UPDATE aurora_sync_meta SET applying=1,counter=counter+1 WHERE singleton=1",
        [],
    )
    .map_err(|e| e.to_string())?;
    if r.deleted {
        if let Some(id) = local {
            c.execute("DELETE FROM vocabulary_entries WHERE id=?1", [id])
                .map_err(|e| e.to_string())?;
        }
        local = None;
    } else if let Some(id) = local {
        c.execute("UPDATE vocabulary_entries SET word=?1,phonetic=?2,definition_markdown=?3,examples_markdown=?4,created_at=?5,updated_at=?6 WHERE id=?7",params![r.word,r.phonetic,r.definition_markdown,r.examples_markdown,r.created_at,r.updated_at,id]).map_err(|e|e.to_string())?;
    } else {
        c.execute("INSERT INTO vocabulary_entries(word,phonetic,definition_markdown,examples_markdown,created_at,updated_at) VALUES(?1,?2,?3,?4,?5,?6)",params![r.word,r.phonetic,r.definition_markdown,r.examples_markdown,r.created_at,r.updated_at]).map_err(|e|e.to_string())?;
        local = Some(c.last_insert_rowid());
    }
    c.execute("INSERT INTO aurora_sync_records(uid,local_id,word,phonetic,definition_markdown,examples_markdown,created_at,updated_at,deleted,seq,version,dirty) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,(SELECT counter FROM aurora_sync_meta),lower(hex(randomblob(16))),0) ON CONFLICT(uid) DO UPDATE SET local_id=excluded.local_id,word=excluded.word,phonetic=excluded.phonetic,definition_markdown=excluded.definition_markdown,examples_markdown=excluded.examples_markdown,created_at=excluded.created_at,updated_at=excluded.updated_at,deleted=excluded.deleted,seq=excluded.seq,version=excluded.version,dirty=0",params![uid,local,r.word,r.phonetic,r.definition_markdown,r.examples_markdown,r.created_at,r.updated_at,r.deleted]).map_err(|e|e.to_string())?;
    c.execute("UPDATE aurora_sync_meta SET applying=0", [])
        .map_err(|e| e.to_string())?;
    get(c, uid)?.ok_or("同步词条写入失败。".into())
}
fn push(c: &Connection, device: &str, r: &Record) -> Result<Ack, String> {
    if let Some(saved) = c
        .query_row(
            "SELECT response FROM aurora_sync_receipts WHERE device=?1 AND op=?2",
            params![device, r.version],
            |r| r.get::<_, String>(0),
        )
        .optional()
        .map_err(|e| e.to_string())?
    {
        return serde_json::from_str(&saved).map_err(|e| e.to_string());
    }
    let mut current = get(c, &r.uid)?;
    // Initial pairing of independently-created books: reconcile same words, not local row IDs.
    if current.is_none() {
        current=c.query_row(&format!("SELECT {COLUMNS} FROM aurora_sync_records WHERE word=?1 COLLATE NOCASE ORDER BY deleted,seq DESC LIMIT 1"),[&r.word],row).optional().map_err(|e|e.to_string())?;
    }
    let collision:bool=c.query_row("SELECT EXISTS(SELECT 1 FROM aurora_sync_records WHERE deleted=0 AND word=?1 COLLATE NOCASE AND uid<>?2)",params![r.word,current.as_ref().map(|v|v.uid.as_str()).unwrap_or(&r.uid)],|r|r.get(0)).map_err(|e|e.to_string())?;
    let (record, conflict) = match current {
        Some(old) if same_content(&old, r) => (old, None),
        Some(old) if old.version == r.base_version && !collision => (apply(c, &old.uid, r)?, None),
        Some(old) => {
            let id: String = c
                .query_row("SELECT lower(hex(randomblob(16)))", [], |r| r.get(0))
                .map_err(|e| e.to_string())?;
            let mut candidate = r.clone();
            candidate.uid = old.uid.clone();
            let conflict = Conflict {
                id,
                device: device.into(),
                candidate,
                remote: old.clone(),
            };
            c.execute(
                "INSERT INTO aurora_sync_conflicts(id,device,candidate,remote) VALUES(?1,?2,?3,?4)",
                params![
                    conflict.id,
                    device,
                    serde_json::to_string(&conflict.candidate).unwrap(),
                    serde_json::to_string(&old).unwrap()
                ],
            )
            .map_err(|e| e.to_string())?;
            (old, Some(conflict))
        }
        None => (apply(c, &r.uid, r)?, None),
    };
    let ack = Ack {
        source_uid: r.uid.clone(),
        op_id: r.version.clone(),
        record,
        conflict,
    };
    c.execute(
        "INSERT INTO aurora_sync_receipts(device,op,response) VALUES(?1,?2,?3)",
        params![device, r.version, serde_json::to_string(&ack).unwrap()],
    )
    .map_err(|e| e.to_string())?;
    Ok(ack)
}
fn bound_batch<T: Serialize>(items: &mut Vec<T>) -> Result<(), String> {
    let mut bytes = 0;
    let mut count = 0;
    for item in items.iter() {
        let size = serde_json::to_vec(item).map_err(|e| e.to_string())?.len();
        if count > 0 && bytes + size > 2 * 1024 * 1024 {
            break;
        }
        bytes += size;
        count += 1;
    }
    items.truncate(count);
    Ok(())
}

pub fn exchange(c: &mut Connection, device: &str, req: &Exchange) -> Result<Reply, String> {
    if req.protocol != 1 || req.changes.len() > 100 || req.cursor < 0 {
        return Err("不支持的同步请求。".into());
    }
    for r in &req.changes {
        validate(r)?;
    }
    let tx = c
        .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
        .map_err(|e| e.to_string())?;
    if book_id(&tx)? != req.book_id {
        return Err("生词本已更换，请重新配对。".into());
    }
    let before: i64 = tx
        .query_row("SELECT counter FROM aurora_sync_meta", [], |r| r.get(0))
        .map_err(|e| e.to_string())?;
    if req.cursor > before {
        return Err("同步版本已回退，请重新配对。".into());
    }
    // A small request can conflict with very large existing notes. Bound each
    // response section; unacknowledged operations remain pending on the client.
    let mut acks = Vec::new();
    let mut ack_bytes = 0;
    for r in &req.changes {
        let ack = push(&tx, device, r)?;
        ack_bytes += serde_json::to_vec(&ack).map_err(|e| e.to_string())?.len();
        acks.push(ack);
        if ack_bytes >= 2 * 1024 * 1024 {
            break;
        }
    }
    let mut records = {
        let mut q = tx
            .prepare(&format!(
                "SELECT {COLUMNS} FROM aurora_sync_records WHERE seq>?1 ORDER BY seq LIMIT 100"
            ))
            .map_err(|e| e.to_string())?;
        let v = q
            .query_map([req.cursor], row)
            .map_err(|e| e.to_string())?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| e.to_string())?;
        v
    };
    bound_batch(&mut records)?;
    let cursor = records.last().map(|r| r.seq).unwrap_or(req.cursor);
    let more: bool = tx
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM aurora_sync_records WHERE seq>?1)",
            [cursor],
            |r| r.get(0),
        )
        .map_err(|e| e.to_string())?;
    let mut pending_conflicts = conflicts(&tx)?;
    bound_batch(&mut pending_conflicts)?;
    let reply = Reply {
        protocol: 1,
        book_id: req.book_id.clone(),
        cursor,
        more,
        acks,
        records,
        conflicts: pending_conflicts,
    };
    tx.commit().map_err(|e| e.to_string())?;
    Ok(reply)
}
pub fn resolve(
    c: &mut Connection,
    id: &str,
    use_candidate: bool,
    expected: &str,
) -> Result<(), String> {
    let tx = c
        .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
        .map_err(|e| e.to_string())?;
    let conflict = conflicts(&tx)?
        .into_iter()
        .find(|v| v.id == id)
        .ok_or("冲突已处理或不存在。")?;
    let current = get(&tx, &conflict.remote.uid)?.ok_or("词条不存在。")?;
    if use_candidate {
        if current.version != expected {
            return Err("电脑内容已再次变化，请刷新后重新选择。".into());
        }
        apply(&tx, &current.uid, &conflict.candidate)?;
    }
    tx.execute(
        "UPDATE aurora_sync_conflicts SET resolved=1 WHERE id=?1",
        [id],
    )
    .map_err(|e| e.to_string())?;
    tx.commit().map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests;
