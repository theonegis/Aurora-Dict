CREATE TABLE IF NOT EXISTS aurora_sync_meta (
 singleton INTEGER PRIMARY KEY CHECK(singleton=1),
 book_id TEXT NOT NULL, counter INTEGER NOT NULL DEFAULT 0, applying INTEGER NOT NULL DEFAULT 0
);
INSERT OR IGNORE INTO aurora_sync_meta(singleton,book_id) VALUES(1,lower(hex(randomblob(16))));
CREATE TABLE IF NOT EXISTS aurora_sync_records (
 uid TEXT PRIMARY KEY, local_id INTEGER UNIQUE,
 word TEXT NOT NULL, phonetic TEXT NOT NULL, definition_markdown TEXT NOT NULL, examples_markdown TEXT NOT NULL,
 created_at INTEGER NOT NULL, updated_at INTEGER NOT NULL,
 deleted INTEGER NOT NULL DEFAULT 0, seq INTEGER NOT NULL,
 version TEXT NOT NULL, base_version TEXT NOT NULL DEFAULT '', dirty INTEGER NOT NULL DEFAULT 1
);
CREATE INDEX IF NOT EXISTS aurora_sync_seq ON aurora_sync_records(seq);
CREATE TABLE IF NOT EXISTS aurora_sync_receipts (
 device TEXT NOT NULL, op TEXT NOT NULL, response TEXT NOT NULL, PRIMARY KEY(device,op)
);
CREATE TABLE IF NOT EXISTS aurora_sync_conflicts (
 id TEXT PRIMARY KEY, device TEXT NOT NULL, candidate TEXT NOT NULL, remote TEXT NOT NULL, resolved INTEGER NOT NULL DEFAULT 0
);
CREATE TABLE IF NOT EXISTS aurora_sync_client (key TEXT PRIMARY KEY, value TEXT NOT NULL);
CREATE TRIGGER IF NOT EXISTS aurora_sync_insert AFTER INSERT ON vocabulary_entries
WHEN (SELECT applying FROM aurora_sync_meta WHERE singleton=1)=0
BEGIN
 UPDATE aurora_sync_meta SET counter=counter+1 WHERE singleton=1;
 INSERT INTO aurora_sync_records(uid,local_id,word,phonetic,definition_markdown,examples_markdown,created_at,updated_at,seq,version)
 VALUES(lower(hex(randomblob(16))),NEW.id,NEW.word,NEW.phonetic,NEW.definition_markdown,NEW.examples_markdown,NEW.created_at,NEW.updated_at,
 (SELECT counter FROM aurora_sync_meta WHERE singleton=1),lower(hex(randomblob(16))));
END;
CREATE TRIGGER IF NOT EXISTS aurora_sync_update AFTER UPDATE ON vocabulary_entries
WHEN (SELECT applying FROM aurora_sync_meta WHERE singleton=1)=0
BEGIN
 UPDATE aurora_sync_meta SET counter=counter+1 WHERE singleton=1;
 UPDATE aurora_sync_records SET word=NEW.word,phonetic=NEW.phonetic,definition_markdown=NEW.definition_markdown,examples_markdown=NEW.examples_markdown,
 created_at=NEW.created_at,updated_at=NEW.updated_at,seq=(SELECT counter FROM aurora_sync_meta WHERE singleton=1),version=lower(hex(randomblob(16))),dirty=1
 WHERE local_id=OLD.id;
END;
CREATE TRIGGER IF NOT EXISTS aurora_sync_delete AFTER DELETE ON vocabulary_entries
WHEN (SELECT applying FROM aurora_sync_meta WHERE singleton=1)=0
BEGIN
 UPDATE aurora_sync_meta SET counter=counter+1 WHERE singleton=1;
 UPDATE aurora_sync_records SET local_id=NULL,deleted=1,seq=(SELECT counter FROM aurora_sync_meta WHERE singleton=1),version=lower(hex(randomblob(16))),dirty=1
 WHERE local_id=OLD.id;
END;
-- Backfill old books once, with unique positive sequence numbers.
INSERT INTO aurora_sync_records(uid,local_id,word,phonetic,definition_markdown,examples_markdown,created_at,updated_at,seq,version)
SELECT lower(hex(randomblob(16))),id,word,phonetic,definition_markdown,examples_markdown,created_at,updated_at,
 (SELECT counter FROM aurora_sync_meta WHERE singleton=1)+row_number() OVER(ORDER BY id),lower(hex(randomblob(16)))
FROM vocabulary_entries WHERE id NOT IN(SELECT local_id FROM aurora_sync_records WHERE local_id IS NOT NULL);
UPDATE aurora_sync_meta SET counter=max(counter,coalesce((SELECT max(seq) FROM aurora_sync_records),0)) WHERE singleton=1;
