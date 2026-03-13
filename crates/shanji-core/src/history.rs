use crate::error::{AppError, Result};
use crate::paths::AppPaths;
use rusqlite::{Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HistoryRecord {
    pub id: Option<i64>,
    pub created_at: u64,
    pub transcribed: String,
    pub rewritten: Option<String>,
    pub duration_ms: Option<u32>,
    pub model_id: Option<String>,
    pub provider_id: Option<String>,
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

        log::info!("History database initialized at: {:?}", db_path);
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
                rewritten TEXT,
                duration_ms INTEGER,
                model_id TEXT,
                provider_id TEXT
            )",
            [],
        )
        .map_err(|e| AppError::Internal(format!("Failed to create records table: {}", e)))?;

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
            "INSERT INTO records (created_at, transcribed, rewritten, duration_ms, model_id, provider_id)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            (
                record.created_at as i64,
                &record.transcribed,
                record.rewritten.as_deref().unwrap_or(""),
                record.duration_ms.map(|d| d as i64),
                record.model_id.as_deref().unwrap_or(""),
                record.provider_id.as_deref().unwrap_or(""),
            ),
        )
        .map_err(|e| AppError::Internal(format!("Failed to insert record: {}", e)))?;

        Ok(conn.last_insert_rowid())
    }

    pub fn get(&self, id: i64) -> Result<Option<HistoryRecord>> {
        let conn = self.conn.lock().unwrap();

        let record = conn
            .query_row(
                "SELECT id, created_at, transcribed, rewritten, duration_ms, model_id, provider_id
                 FROM records WHERE id = ?1",
                [&id],
                |row| {
                    Ok(HistoryRecord {
                        id: Some(row.get(0)?),
                        created_at: row.get(1)?,
                        transcribed: row.get(2)?,
                        rewritten: row.get(3)?,
                        duration_ms: row.get(4)?,
                        model_id: row.get(5)?,
                        provider_id: row.get(6)?,
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
                "SELECT id, created_at, transcribed, rewritten, duration_ms, model_id, provider_id
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
                    rewritten: row.get(3)?,
                    duration_ms: row.get(4)?,
                    model_id: row.get(5)?,
                    provider_id: row.get(6)?,
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
                "SELECT r.id, r.created_at, r.transcribed, r.rewritten, r.duration_ms, r.model_id, r.provider_id
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
                        rewritten: row.get(3)?,
                        duration_ms: row.get(4)?,
                        model_id: row.get(5)?,
                        provider_id: row.get(6)?,
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
            rewritten: Some("Hello, world!".to_string()),
            duration_ms: Some(5000),
            model_id: Some("test-model".to_string()),
            provider_id: Some("test-provider".to_string()),
        };

        let id = db.insert(&record).unwrap();
        let retrieved = db.get(id).unwrap().unwrap();
        assert_eq!(retrieved.transcribed, "Hello world");
        assert_eq!(retrieved.rewritten, Some("Hello, world!".to_string()));
    }
}
