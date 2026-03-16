use crate::error::{AppError, Result};
use crate::paths::AppPaths;
use rusqlite::{Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

/// 用于 Slint UI 渲染的历史卡片数据（扁平化，无 Option）
#[derive(Debug, Clone)]
pub struct HistoryCardData {
    pub record_id: i32,
    pub timestamp: String,    // 格式化后的本地时间，如 "2026-03-16 14:32"
    pub text: String,         // 优先展示: rewritten > corrected_transcribed > transcribed
    pub has_audio: bool,
    pub is_llm_rewritten: bool,
    pub was_pasted: bool,     // 当前始终 false（暂无粘贴状态跟踪，预留字段）
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HistoryRecord {
    pub id: Option<i64>,
    pub created_at: u64,
    pub transcribed: String,
    pub live_transcribed: Option<String>,
    pub corrected_transcribed: Option<String>,
    pub rewritten: Option<String>,
    pub duration_ms: Option<u32>,
    pub model_id: Option<String>,
    pub live_model_id: Option<String>,
    pub refine_model_id: Option<String>,
    pub refine_enabled: bool,
    pub provider_id: Option<String>,
    pub audio_path: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HistoryListResponse {
    pub records: Vec<HistoryRecord>,
    pub total: u32,
    pub page: u32,
    pub page_size: u32,
}

pub struct HistoryDb {
    conn: Arc<Mutex<Connection>>,
}

impl HistoryDb {
    pub fn new_with_paths(paths: &AppPaths) -> Result<Self> {
        let db_path = get_db_path(paths);

        if let Some(parent) = db_path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| AppError::Internal(format!("Failed to create db directory: {}", e)))?;
        }

        let conn = Connection::open(&db_path)
            .map_err(|e| AppError::Internal(format!("Failed to open database: {}", e)))?;

        let db = Self {
            conn: Arc::new(Mutex::new(conn)),
        };
        db.init_schema()?;

        log::debug!("History database initialized at: {:?}", db_path);
        Ok(db)
    }

    #[cfg(test)]
    fn with_connection(conn: Connection) -> Result<Self> {
        let db = Self {
            conn: Arc::new(Mutex::new(conn)),
        };
        db.init_schema()?;
        Ok(db)
    }

    fn init_schema(&self) -> Result<()> {
        let conn = self.conn.lock().unwrap();

        conn.execute(
            "CREATE TABLE IF NOT EXISTS records (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                created_at INTEGER NOT NULL,
                transcribed TEXT NOT NULL,
                live_transcribed TEXT,
                corrected_transcribed TEXT,
                rewritten TEXT,
                duration_ms INTEGER,
                model_id TEXT,
                live_model_id TEXT,
                refine_model_id TEXT,
                refine_enabled INTEGER NOT NULL DEFAULT 0,
                provider_id TEXT,
                audio_path TEXT
            )",
            [],
        )
        .map_err(|e| AppError::Internal(format!("Failed to create records table: {}", e)))?;

        ensure_column(&conn, "records", "live_transcribed", "TEXT")?;
        ensure_column(&conn, "records", "corrected_transcribed", "TEXT")?;
        ensure_column(&conn, "records", "live_model_id", "TEXT")?;
        ensure_column(&conn, "records", "refine_model_id", "TEXT")?;
        ensure_column(
            &conn,
            "records",
            "refine_enabled",
            "INTEGER NOT NULL DEFAULT 0",
        )?;
        ensure_column(&conn, "records", "audio_path", "TEXT")?;

        conn.execute(
            "CREATE INDEX IF NOT EXISTS idx_records_created_at ON records(created_at DESC)",
            [],
        )
        .map_err(|e| AppError::Internal(format!("Failed to create index: {}", e)))?;

        conn.execute(
            "CREATE VIRTUAL TABLE IF NOT EXISTS records_fts USING fts5(
                transcribed,
                rewritten,
                content='records',
                content_rowid='id'
            )",
            [],
        )
        .map_err(|e| AppError::Internal(format!("Failed to create FTS5 table: {}", e)))?;

        conn.execute(
            "CREATE TRIGGER IF NOT EXISTS records_ai AFTER INSERT ON records BEGIN
                INSERT INTO records_fts(rowid, transcribed, rewritten)
                VALUES (new.id, new.transcribed, new.rewritten);
            END",
            [],
        )
        .map_err(|e| AppError::Internal(format!("Failed to create insert trigger: {}", e)))?;

        conn.execute(
            "CREATE TRIGGER IF NOT EXISTS records_ad AFTER DELETE ON records BEGIN
                INSERT INTO records_fts(records_fts, rowid, transcribed, rewritten)
                VALUES ('delete', old.id, old.transcribed, old.rewritten);
            END",
            [],
        )
        .map_err(|e| AppError::Internal(format!("Failed to create delete trigger: {}", e)))?;

        conn.execute(
            "CREATE TRIGGER IF NOT EXISTS records_au AFTER UPDATE ON records BEGIN
                INSERT INTO records_fts(records_fts, rowid, transcribed, rewritten)
                VALUES ('delete', old.id, old.transcribed, old.rewritten);
                INSERT INTO records_fts(rowid, transcribed, rewritten)
                VALUES (new.id, new.transcribed, new.rewritten);
            END",
            [],
        )
        .map_err(|e| AppError::Internal(format!("Failed to create update trigger: {}", e)))?;

        Ok(())
    }

    pub fn insert(&self, record: &HistoryRecord) -> Result<i64> {
        let conn = self.conn.lock().unwrap();

        conn.execute(
            "INSERT INTO records (
                created_at,
                transcribed,
                live_transcribed,
                corrected_transcribed,
                rewritten,
                duration_ms,
                model_id,
                live_model_id,
                refine_model_id,
                refine_enabled,
                provider_id,
                audio_path
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
            (
                record.created_at as i64,
                &record.transcribed,
                record.live_transcribed.as_deref().unwrap_or(""),
                record.corrected_transcribed.as_deref().unwrap_or(""),
                record.rewritten.as_deref().unwrap_or(""),
                record.duration_ms.map(|d| d as i64),
                record.model_id.as_deref().unwrap_or(""),
                record.live_model_id.as_deref().unwrap_or(""),
                record.refine_model_id.as_deref().unwrap_or(""),
                if record.refine_enabled { 1 } else { 0 },
                record.provider_id.as_deref().unwrap_or(""),
                record.audio_path.as_deref().unwrap_or(""),
            ),
        )
        .map_err(|e| AppError::Internal(format!("Failed to insert record: {}", e)))?;

        Ok(conn.last_insert_rowid())
    }

    pub fn get(&self, id: i64) -> Result<Option<HistoryRecord>> {
        let conn = self.conn.lock().unwrap();

        let record = conn
            .query_row(
                "SELECT
                    id,
                    created_at,
                    transcribed,
                    live_transcribed,
                    corrected_transcribed,
                    rewritten,
                    duration_ms,
                    model_id,
                    live_model_id,
                    refine_model_id,
                    refine_enabled,
                    provider_id,
                    audio_path
                 FROM records WHERE id = ?1",
                [&id],
                |row| {
                    Ok(HistoryRecord {
                        id: Some(row.get(0)?),
                        created_at: row.get(1)?,
                        transcribed: row.get(2)?,
                        live_transcribed: empty_to_none(row.get::<_, Option<String>>(3)?),
                        corrected_transcribed: empty_to_none(row.get::<_, Option<String>>(4)?),
                        rewritten: empty_to_none(row.get::<_, Option<String>>(5)?),
                        duration_ms: row.get(6)?,
                        model_id: empty_to_none(row.get::<_, Option<String>>(7)?),
                        live_model_id: empty_to_none(row.get::<_, Option<String>>(8)?),
                        refine_model_id: empty_to_none(row.get::<_, Option<String>>(9)?),
                        refine_enabled: row.get::<_, i64>(10)? != 0,
                        provider_id: empty_to_none(row.get::<_, Option<String>>(11)?),
                        audio_path: empty_to_none(row.get::<_, Option<String>>(12)?),
                    })
                },
            )
            .optional()
            .map_err(|e| AppError::Internal(format!("Failed to get record: {}", e)))?;

        Ok(record)
    }

    pub fn list(&self, page: u32, page_size: u32) -> Result<Vec<HistoryRecord>> {
        let conn = self.conn.lock().unwrap();
        let offset = page * page_size;

        let mut stmt = conn
            .prepare(
                "SELECT
                    id,
                    created_at,
                    transcribed,
                    live_transcribed,
                    corrected_transcribed,
                    rewritten,
                    duration_ms,
                    model_id,
                    live_model_id,
                    refine_model_id,
                    refine_enabled,
                    provider_id,
                    audio_path
                 FROM records
                 ORDER BY created_at DESC
                 LIMIT ?1 OFFSET ?2",
            )
            .map_err(|e| AppError::Internal(format!("Failed to prepare query: {}", e)))?;

        let records = stmt
            .query_map([page_size, offset], |row| {
                Ok(HistoryRecord {
                    id: Some(row.get(0)?),
                    created_at: row.get(1)?,
                    transcribed: row.get(2)?,
                    live_transcribed: empty_to_none(row.get::<_, Option<String>>(3)?),
                    corrected_transcribed: empty_to_none(row.get::<_, Option<String>>(4)?),
                    rewritten: empty_to_none(row.get::<_, Option<String>>(5)?),
                    duration_ms: row.get(6)?,
                    model_id: empty_to_none(row.get::<_, Option<String>>(7)?),
                    live_model_id: empty_to_none(row.get::<_, Option<String>>(8)?),
                    refine_model_id: empty_to_none(row.get::<_, Option<String>>(9)?),
                    refine_enabled: row.get::<_, i64>(10)? != 0,
                    provider_id: empty_to_none(row.get::<_, Option<String>>(11)?),
                    audio_path: empty_to_none(row.get::<_, Option<String>>(12)?),
                })
            })
            .map_err(|e| AppError::Internal(format!("Failed to query records: {}", e)))?
            .collect::<std::result::Result<Vec<_>, _>>()
            .map_err(|e| AppError::Internal(format!("Failed to collect records: {}", e)))?;

        Ok(records)
    }

    pub fn search(&self, query: &str, page: u32, page_size: u32) -> Result<Vec<HistoryRecord>> {
        let conn = self.conn.lock().unwrap();
        let offset = page * page_size;
        let escaped_query = escape_fts5_query(query);

        let mut stmt = conn
            .prepare(
                "SELECT
                    r.id,
                    r.created_at,
                    r.transcribed,
                    r.live_transcribed,
                    r.corrected_transcribed,
                    r.rewritten,
                    r.duration_ms,
                    r.model_id,
                    r.live_model_id,
                    r.refine_model_id,
                    r.refine_enabled,
                    r.provider_id,
                    r.audio_path
                 FROM records_fts fts
                 JOIN records r ON r.id = fts.rowid
                 WHERE records_fts MATCH ?1
                 ORDER BY r.created_at DESC
                 LIMIT ?2 OFFSET ?3",
            )
            .map_err(|e| AppError::Internal(format!("Failed to prepare search: {}", e)))?;

        let records = stmt
            .query_map(
                [&escaped_query, &page_size.to_string(), &offset.to_string()],
                |row| {
                    Ok(HistoryRecord {
                        id: Some(row.get(0)?),
                        created_at: row.get(1)?,
                        transcribed: row.get(2)?,
                        live_transcribed: empty_to_none(row.get::<_, Option<String>>(3)?),
                        corrected_transcribed: empty_to_none(row.get::<_, Option<String>>(4)?),
                        rewritten: empty_to_none(row.get::<_, Option<String>>(5)?),
                        duration_ms: row.get(6)?,
                        model_id: empty_to_none(row.get::<_, Option<String>>(7)?),
                        live_model_id: empty_to_none(row.get::<_, Option<String>>(8)?),
                        refine_model_id: empty_to_none(row.get::<_, Option<String>>(9)?),
                        refine_enabled: row.get::<_, i64>(10)? != 0,
                        provider_id: empty_to_none(row.get::<_, Option<String>>(11)?),
                        audio_path: empty_to_none(row.get::<_, Option<String>>(12)?),
                    })
                },
            )
            .map_err(|e| AppError::Internal(format!("Failed to search records: {}", e)))?
            .collect::<std::result::Result<Vec<_>, _>>()
            .map_err(|e| AppError::Internal(format!("Failed to collect search results: {}", e)))?;

        Ok(records)
    }

    pub fn count(&self) -> Result<u32> {
        let conn = self.conn.lock().unwrap();
        let count: u32 = conn
            .query_row("SELECT COUNT(*) FROM records", [], |row| row.get(0))
            .map_err(|e| AppError::Internal(format!("Failed to count records: {}", e)))?;
        Ok(count)
    }

    pub fn delete(&self, id: i64) -> Result<bool> {
        let conn = self.conn.lock().unwrap();
        let affected = conn
            .execute("DELETE FROM records WHERE id = ?1", [&id])
            .map_err(|e| AppError::Internal(format!("Failed to delete record: {}", e)))?;
        Ok(affected > 0)
    }

    /// 取最近 20 条记录并转换为 UI 卡片格式
    pub fn list_cards(&self, limit: u32) -> Result<(Vec<HistoryCardData>, u32)> {
        let total = self.count()?;
        let records = self.list(0, limit)?;
        let cards = records.into_iter().filter_map(|r| {
            let id = r.id? as i32;
            // 注意：先捕获 is_llm_rewritten，再 move rewritten 字段
            let is_llm_rewritten = r.rewritten.as_ref()
                .map(|s| !s.is_empty()).unwrap_or(false);
            let has_audio = r.audio_path.as_ref()
                .map(|p| !p.is_empty()).unwrap_or(false);
            let text = r.rewritten
                .filter(|s| !s.is_empty())
                .or_else(|| r.corrected_transcribed.filter(|s| !s.is_empty()))
                .unwrap_or(r.transcribed);
            let ts = format_timestamp(r.created_at);
            Some(HistoryCardData {
                record_id: id,
                timestamp: ts,
                text,
                has_audio,
                is_llm_rewritten,
                was_pasted: false,
            })
        }).collect();
        Ok((cards, total))
    }

    pub fn clear_all(&self) -> Result<u32> {
        let conn = self.conn.lock().unwrap();
        let affected = conn
            .execute("DELETE FROM records", [])
            .map_err(|e| AppError::Internal(format!("Failed to clear records: {}", e)))?;

        conn.execute("INSERT INTO records_fts(records_fts) VALUES('rebuild')", [])
            .map_err(|e| log::warn!("Failed to rebuild FTS5 index: {}", e))
            .ok();
        conn.execute("VACUUM", [])
            .map_err(|e| log::warn!("Failed to vacuum database: {}", e))
            .ok();

        Ok(affected as u32)
    }

    pub fn prune_old_records(&self, keep_count: u32) -> Result<u32> {
        let conn = self.conn.lock().unwrap();

        let affected = conn
            .execute(
                "DELETE FROM records WHERE id NOT IN (
                SELECT id FROM records ORDER BY created_at DESC LIMIT ?1
            )",
                [&keep_count.to_string()],
            )
            .map_err(|e| AppError::Internal(format!("Failed to prune records: {}", e)))?;

        if affected > 0 {
            conn.execute("VACUUM", [])
                .map_err(|e| log::warn!("Failed to vacuum database: {}", e))
                .ok();
        }

        Ok(affected as u32)
    }

    pub fn delete_before(&self, timestamp: u64) -> Result<u32> {
        let conn = self.conn.lock().unwrap();

        let affected = conn
            .execute(
                "DELETE FROM records WHERE created_at < ?1",
                [&timestamp.to_string()],
            )
            .map_err(|e| AppError::Internal(format!("Failed to delete old records: {}", e)))?;

        Ok(affected as u32)
    }
}

fn get_db_path(paths: &AppPaths) -> PathBuf {
    paths.data_dir.join("history.db")
}

fn ensure_column(conn: &Connection, table: &str, column: &str, definition: &str) -> Result<()> {
    if has_column(conn, table, column)? {
        return Ok(());
    }

    conn.execute(
        &format!("ALTER TABLE {table} ADD COLUMN {column} {definition}"),
        [],
    )
    .map_err(|e| AppError::Internal(format!("Failed to add column {}.{}: {}", table, column, e)))?;
    Ok(())
}

fn has_column(conn: &Connection, table: &str, column: &str) -> Result<bool> {
    let mut stmt = conn
        .prepare(&format!("PRAGMA table_info({table})"))
        .map_err(|e| AppError::Internal(format!("Failed to inspect schema: {}", e)))?;
    let columns = stmt
        .query_map([], |row| row.get::<_, String>(1))
        .map_err(|e| AppError::Internal(format!("Failed to query schema: {}", e)))?
        .collect::<std::result::Result<Vec<_>, _>>()
        .map_err(|e| AppError::Internal(format!("Failed to collect schema rows: {}", e)))?;
    Ok(columns.iter().any(|name| name == column))
}

fn empty_to_none(value: Option<String>) -> Option<String> {
    value.and_then(|value| if value.is_empty() { None } else { Some(value) })
}

fn format_timestamp(ts_ms: u64) -> String {
    use std::time::{Duration, UNIX_EPOCH};
    // ts 单位：毫秒
    let secs = ts_ms / 1000;
    let d = UNIX_EPOCH + Duration::from_secs(secs);
    let datetime: chrono::DateTime<chrono::Local> = d.into();
    datetime.format("%Y-%m-%d %H:%M").to_string()
}

fn escape_fts5_query(query: &str) -> String {
    let special_chars = ['"', '*', '(', ')', '-', '^'];
    let mut result = String::new();

    for ch in query.chars() {
        if special_chars.contains(&ch) {
            result.push('"');
            result.push(ch);
            result.push('"');
        } else {
            result.push(ch);
        }
    }

    if result.is_empty() {
        query.to_string()
    } else {
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn create_test_db() -> HistoryDb {
        let conn = Connection::open_in_memory().unwrap();
        HistoryDb::with_connection(conn).unwrap()
    }

    #[test]
    fn test_insert_and_get() {
        let db = create_test_db();
        let record = HistoryRecord {
            id: None,
            created_at: 1234567890,
            transcribed: "Hello world".to_string(),
            live_transcribed: Some("Hello world".to_string()),
            corrected_transcribed: Some("Hello world".to_string()),
            rewritten: Some("Hello, world!".to_string()),
            duration_ms: Some(5000),
            model_id: Some("test-model".to_string()),
            live_model_id: Some("test-live-model".to_string()),
            refine_model_id: Some("test-refine-model".to_string()),
            refine_enabled: true,
            provider_id: Some("test-provider".to_string()),
            audio_path: Some("/tmp/test.wav".to_string()),
        };

        let id = db.insert(&record).unwrap();
        let retrieved = db.get(id).unwrap().unwrap();
        assert_eq!(retrieved.transcribed, "Hello world");
        assert_eq!(retrieved.live_transcribed, Some("Hello world".to_string()));
        assert_eq!(retrieved.rewritten, Some("Hello, world!".to_string()));
        assert_eq!(retrieved.live_model_id, Some("test-live-model".to_string()));
        assert!(retrieved.refine_enabled);
    }

    #[test]
    fn test_list_cards_empty() {
        let db = create_test_db();
        let (cards, total) = db.list_cards(20).unwrap();
        assert_eq!(cards.len(), 0);
        assert_eq!(total, 0);
    }

    #[test]
    fn test_list_cards_prefers_rewritten() {
        let db = create_test_db();
        let record = HistoryRecord {
            id: None,
            created_at: 1000000000000, // 毫秒
            transcribed: "raw".to_string(),
            rewritten: Some("polished".to_string()),
            live_transcribed: None,
            corrected_transcribed: None,
            duration_ms: None,
            model_id: None,
            live_model_id: None,
            refine_model_id: None,
            refine_enabled: false,
            provider_id: None,
            audio_path: Some("/tmp/test.wav".to_string()),
        };
        db.insert(&record).unwrap();
        let (cards, total) = db.list_cards(20).unwrap();
        assert_eq!(total, 1);
        assert_eq!(cards[0].text, "polished");
        assert!(cards[0].has_audio);
        assert!(cards[0].is_llm_rewritten);
        assert!(!cards[0].was_pasted);
    }

    #[test]
    fn test_list_cards_no_audio_when_path_empty() {
        let db = create_test_db();
        let record = HistoryRecord {
            id: None,
            created_at: 1000000000000,
            transcribed: "hello".to_string(),
            rewritten: None,
            live_transcribed: None,
            corrected_transcribed: None,
            duration_ms: None,
            model_id: None,
            live_model_id: None,
            refine_model_id: None,
            refine_enabled: false,
            provider_id: None,
            audio_path: Some("".to_string()),
        };
        db.insert(&record).unwrap();
        let (cards, _) = db.list_cards(20).unwrap();
        assert!(!cards[0].has_audio);
    }
}
