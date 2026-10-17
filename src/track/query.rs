use super::{budget, store::TrackError};
use rusqlite::Connection;

#[derive(Debug, thiserror::Error)]
pub enum QueryError {
    #[error(transparent)]
    Sqlite(#[from] rusqlite::Error),
    #[error(transparent)]
    Track(#[from] TrackError),
}

#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct RunRow {
    pub id: String,
    pub rom_sha1: String,
    pub goal: Option<String>,
    pub status: String,
    pub repro_status: Option<String>,
}

#[derive(Debug, Default, Clone)]
pub struct RunFilter {
    pub rom_sha1: Option<String>,
    pub goal: Option<String>,
    pub status: Option<String>,
}

/// 필터에 맞는 run 행을 시작시각 내림차순(최근 우선)으로 반환한다.
pub fn query_runs(conn: &Connection, f: &RunFilter) -> Result<Vec<RunRow>, QueryError> {
    // Bound even direct cache callers before ORDER BY/filter work or Rust field copies.
    let inspected: i64 = conn.query_row(
        "SELECT COUNT(*) FROM (SELECT 1 FROM run LIMIT ?1)",
        [budget::MAX_SCAN_RECORDS as i64 + 1],
        |row| row.get(0),
    )?;
    if inspected > budget::MAX_SCAN_RECORDS as i64 {
        return Err(budget::limit("result rows", budget::MAX_SCAN_RECORDS).into());
    }
    let mut remaining_bytes = budget::MAX_RESULT_BYTES;
    let mut sql = String::from("SELECT id,rom_sha1,goal,status,repro_status FROM run WHERE 1=1");
    let mut args: Vec<(&str, &dyn rusqlite::ToSql)> = Vec::new();
    if let Some(v) = &f.rom_sha1 {
        sql.push_str(" AND rom_sha1 = :rom");
        args.push((":rom", v));
    }
    if let Some(v) = &f.goal {
        sql.push_str(" AND goal = :goal");
        args.push((":goal", v));
    }
    if let Some(v) = &f.status {
        sql.push_str(" AND status = :status");
        args.push((":status", v));
    }
    sql.push_str(" ORDER BY started_at DESC, id DESC");

    let mut stmt = conn.prepare(&sql)?;
    let mut rows = stmt.query(args.as_slice())?;
    let mut out = Vec::new();
    while let Some(row) = rows.next()? {
        // get_ref borrows SQLite storage; admit before String decoding/allocation.
        for column in 0..5 {
            if let rusqlite::types::ValueRef::Text(bytes) = row.get_ref(column)? {
                remaining_bytes = remaining_bytes
                    .checked_sub(bytes.len() as u64)
                    .ok_or_else(|| budget::limit("result bytes", budget::MAX_RESULT_BYTES))?;
            }
        }
        out.push(RunRow {
            id: row.get(0)?,
            rom_sha1: row.get(1)?,
            goal: row.get(2)?,
            status: row.get(3)?,
            repro_status: row.get(4)?,
        });
    }
    budget::check_result(&out)?;
    Ok(out)
}
