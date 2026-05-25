use crate::transcripts::agents::opencode::open_sqlite_readonly;
use crate::transcripts::types::{TranscriptBatch, TranscriptError};
use crate::transcripts::watermark::{TimestampCursorWatermark, WatermarkStrategy};
use rusqlite::Connection;
use serde_json::json;
use std::collections::HashMap;
use std::path::Path;

/// Read OTEL spans incrementally from a Copilot traces SQLite DB.
///
/// Uses keyset pagination on `(end_time_ms, span_id)` to prevent data loss
/// when multiple spans share the same `end_time_ms` at a batch boundary.
pub fn read_otel_spans_incremental(
    path: &Path,
    watermark: Box<dyn WatermarkStrategy>,
    batch_size: usize,
) -> Result<TranscriptBatch, TranscriptError> {
    let cursor = watermark
        .as_any()
        .downcast_ref::<TimestampCursorWatermark>()
        .ok_or_else(|| TranscriptError::Fatal {
            message: "OTEL stream requires TimestampCursorWatermark".to_string(),
        })?;

    let conn = open_sqlite_readonly(path)?;

    let spans = read_spans_after(&conn, cursor.timestamp_millis, &cursor.last_id, batch_size)?;
    if spans.is_empty() {
        return Ok(TranscriptBatch {
            events: vec![],
            new_watermark: Box::new(cursor.clone()),
        });
    }

    let span_ids: Vec<&str> = spans.iter().map(|s| s.span_id.as_str()).collect();
    let attributes = read_attributes_for_spans(&conn, &span_ids)?;
    let events = read_events_for_spans(&conn, &span_ids)?;

    let last_span = spans.last().unwrap();
    let new_watermark =
        TimestampCursorWatermark::new(last_span.end_time_ms, last_span.span_id.clone());

    let json_events: Vec<serde_json::Value> = spans
        .into_iter()
        .map(|span| {
            let span_attrs = attributes.get(&span.span_id).cloned().unwrap_or_default();
            let span_events = events.get(&span.span_id).cloned().unwrap_or_default();
            build_span_event_json(span, span_attrs, span_events)
        })
        .collect();

    Ok(TranscriptBatch {
        events: json_events,
        new_watermark: Box::new(new_watermark),
    })
}

struct SpanRow {
    span_id: String,
    trace_id: String,
    parent_span_id: Option<String>,
    name: String,
    start_time_ms: i64,
    end_time_ms: i64,
    status_code: i32,
    status_message: Option<String>,
    operation_name: Option<String>,
    provider_name: Option<String>,
    agent_name: Option<String>,
    conversation_id: Option<String>,
    request_model: Option<String>,
    response_model: Option<String>,
    input_tokens: Option<i64>,
    output_tokens: Option<i64>,
    cached_tokens: Option<i64>,
    reasoning_tokens: Option<i64>,
    tool_name: Option<String>,
    tool_call_id: Option<String>,
    tool_type: Option<String>,
    chat_session_id: Option<String>,
    turn_index: Option<i64>,
    ttft_ms: Option<f64>,
}

fn read_spans_after(
    conn: &Connection,
    after_ms: i64,
    after_id: &str,
    limit: usize,
) -> Result<Vec<SpanRow>, TranscriptError> {
    // Keyset pagination: skip spans at or before the cursor.
    // If after_id is empty (initial state), use simple `>` on timestamp.
    // Otherwise use compound `(ts > ?) OR (ts = ? AND id > ?)` to handle ties.
    let (sql, params): (String, Vec<Box<dyn rusqlite::types::ToSql>>) = if after_id.is_empty() {
        (
            "SELECT span_id, trace_id, parent_span_id, name, \
             CAST(start_time_ms AS INTEGER), CAST(end_time_ms AS INTEGER), \
             status_code, status_message, operation_name, provider_name, agent_name, \
             conversation_id, request_model, response_model, input_tokens, output_tokens, \
             cached_tokens, reasoning_tokens, tool_name, tool_call_id, tool_type, \
             chat_session_id, turn_index, ttft_ms \
             FROM spans WHERE end_time_ms > ?1 \
             ORDER BY end_time_ms ASC, span_id ASC LIMIT ?2"
                .to_string(),
            vec![
                Box::new(after_ms) as Box<dyn rusqlite::types::ToSql>,
                Box::new(limit as i64),
            ],
        )
    } else {
        (
            "SELECT span_id, trace_id, parent_span_id, name, \
             CAST(start_time_ms AS INTEGER), CAST(end_time_ms AS INTEGER), \
             status_code, status_message, operation_name, provider_name, agent_name, \
             conversation_id, request_model, response_model, input_tokens, output_tokens, \
             cached_tokens, reasoning_tokens, tool_name, tool_call_id, tool_type, \
             chat_session_id, turn_index, ttft_ms \
             FROM spans WHERE (end_time_ms > ?1) OR (end_time_ms = ?2 AND span_id > ?3) \
             ORDER BY end_time_ms ASC, span_id ASC LIMIT ?4"
                .to_string(),
            vec![
                Box::new(after_ms) as Box<dyn rusqlite::types::ToSql>,
                Box::new(after_ms),
                Box::new(after_id.to_string()),
                Box::new(limit as i64),
            ],
        )
    };

    let mut stmt = conn.prepare(&sql).map_err(|e| TranscriptError::Fatal {
        message: format!("Failed to prepare spans query: {}", e),
    })?;

    let rows = stmt
        .query_map(rusqlite::params_from_iter(params.iter()), |row| {
            Ok(SpanRow {
                span_id: row.get(0)?,
                trace_id: row.get(1)?,
                parent_span_id: row.get(2)?,
                name: row.get(3)?,
                start_time_ms: row.get(4)?,
                end_time_ms: row.get(5)?,
                status_code: row.get(6)?,
                status_message: row.get(7)?,
                operation_name: row.get(8)?,
                provider_name: row.get(9)?,
                agent_name: row.get(10)?,
                conversation_id: row.get(11)?,
                request_model: row.get(12)?,
                response_model: row.get(13)?,
                input_tokens: row.get(14)?,
                output_tokens: row.get(15)?,
                cached_tokens: row.get(16)?,
                reasoning_tokens: row.get(17)?,
                tool_name: row.get(18)?,
                tool_call_id: row.get(19)?,
                tool_type: row.get(20)?,
                chat_session_id: row.get(21)?,
                turn_index: row.get(22)?,
                ttft_ms: row.get(23)?,
            })
        })
        .map_err(|e| TranscriptError::Fatal {
            message: format!("Failed to query spans: {}", e),
        })?;

    rows.collect::<Result<Vec<_>, _>>()
        .map_err(|e| TranscriptError::Fatal {
            message: format!("Failed to read span row: {}", e),
        })
}

fn read_attributes_for_spans(
    conn: &Connection,
    span_ids: &[&str],
) -> Result<HashMap<String, HashMap<String, String>>, TranscriptError> {
    if span_ids.is_empty() {
        return Ok(HashMap::new());
    }
    let placeholders: String = span_ids.iter().map(|_| "?").collect::<Vec<_>>().join(",");
    let sql = format!(
        "SELECT span_id, key, value FROM span_attributes WHERE span_id IN ({})",
        placeholders
    );
    let mut stmt = conn.prepare(&sql).map_err(|e| TranscriptError::Fatal {
        message: format!("Failed to prepare attributes query: {}", e),
    })?;

    let mut result: HashMap<String, HashMap<String, String>> = HashMap::new();
    let rows = stmt
        .query_map(rusqlite::params_from_iter(span_ids.iter()), |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, Option<String>>(2)?,
            ))
        })
        .map_err(|e| TranscriptError::Fatal {
            message: format!("Failed to query attributes: {}", e),
        })?;

    for row in rows {
        let (span_id, key, value) = row.map_err(|e| TranscriptError::Fatal {
            message: format!("Failed to read attribute row: {}", e),
        })?;
        if let Some(v) = value {
            result.entry(span_id).or_default().insert(key, v);
        }
    }
    Ok(result)
}

fn read_events_for_spans(
    conn: &Connection,
    span_ids: &[&str],
) -> Result<HashMap<String, Vec<serde_json::Value>>, TranscriptError> {
    if span_ids.is_empty() {
        return Ok(HashMap::new());
    }
    let placeholders: String = span_ids.iter().map(|_| "?").collect::<Vec<_>>().join(",");
    let sql = format!(
        "SELECT span_id, name, CAST(timestamp_ms AS INTEGER), attributes FROM span_events \
         WHERE span_id IN ({}) ORDER BY timestamp_ms ASC",
        placeholders
    );
    let mut stmt = conn.prepare(&sql).map_err(|e| TranscriptError::Fatal {
        message: format!("Failed to prepare events query: {}", e),
    })?;

    let mut result: HashMap<String, Vec<serde_json::Value>> = HashMap::new();
    let rows = stmt
        .query_map(rusqlite::params_from_iter(span_ids.iter()), |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, i64>(2)?,
                row.get::<_, Option<String>>(3)?,
            ))
        })
        .map_err(|e| TranscriptError::Fatal {
            message: format!("Failed to query events: {}", e),
        })?;

    for row in rows {
        let (span_id, name, timestamp_ms, attributes_json) =
            row.map_err(|e| TranscriptError::Fatal {
                message: format!("Failed to read event row: {}", e),
            })?;
        let attrs: serde_json::Value = attributes_json
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or(serde_json::Value::Null);
        result.entry(span_id).or_default().push(json!({
            "name": name,
            "timestamp_ms": timestamp_ms,
            "attributes": attrs,
        }));
    }
    Ok(result)
}

fn build_span_event_json(
    span: SpanRow,
    attributes: HashMap<String, String>,
    events: Vec<serde_json::Value>,
) -> serde_json::Value {
    json!({
        "span": {
            "span_id": span.span_id,
            "trace_id": span.trace_id,
            "parent_span_id": span.parent_span_id,
            "name": span.name,
            "start_time_ms": span.start_time_ms,
            "end_time_ms": span.end_time_ms,
            "status_code": span.status_code,
            "status_message": span.status_message,
            "operation_name": span.operation_name,
            "provider_name": span.provider_name,
            "agent_name": span.agent_name,
            "conversation_id": span.conversation_id,
            "request_model": span.request_model,
            "response_model": span.response_model,
            "input_tokens": span.input_tokens,
            "output_tokens": span.output_tokens,
            "cached_tokens": span.cached_tokens,
            "reasoning_tokens": span.reasoning_tokens,
            "tool_name": span.tool_name,
            "tool_call_id": span.tool_call_id,
            "tool_type": span.tool_type,
            "chat_session_id": span.chat_session_id,
            "turn_index": span.turn_index,
            "ttft_ms": span.ttft_ms,
        },
        "attributes": attributes,
        "events": events,
    })
}

/// Extract per-event IDs from an OTEL span event JSON.
/// Returns (event_id=span_id, parent_event_id=parent_span_id, tool_use_id=tool_call_id).
pub fn extract_otel_event_ids(
    event: &serde_json::Value,
) -> (Option<String>, Option<String>, Option<String>) {
    let span = event.get("span");
    let event_id = span
        .and_then(|s| s.get("span_id"))
        .and_then(|v| v.as_str())
        .map(String::from);
    let parent_event_id = span
        .and_then(|s| s.get("parent_span_id"))
        .and_then(|v| v.as_str())
        .map(String::from);
    let tool_use_id = span
        .and_then(|s| s.get("tool_call_id"))
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .map(String::from);
    (event_id, parent_event_id, tool_use_id)
}

/// Extract timestamp (as Unix seconds u32) from an OTEL span event JSON.
pub fn extract_otel_event_timestamp(event: &serde_json::Value) -> Option<u32> {
    event
        .get("span")
        .and_then(|s| s.get("start_time_ms"))
        .and_then(|v| v.as_i64())
        .map(|ms| (ms / 1000) as u32)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::transcripts::watermark::TimestampCursorWatermark;

    fn create_test_otel_db() -> (tempfile::TempDir, std::path::PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("traces.db");
        let conn = rusqlite::Connection::open(&db_path).unwrap();
        conn.execute_batch(
            "CREATE TABLE spans (
                span_id TEXT PRIMARY KEY, trace_id TEXT NOT NULL, parent_span_id TEXT,
                name TEXT NOT NULL, start_time_ms INTEGER NOT NULL, end_time_ms INTEGER NOT NULL,
                status_code INTEGER NOT NULL DEFAULT 0, status_message TEXT,
                operation_name TEXT, provider_name TEXT, agent_name TEXT, conversation_id TEXT,
                request_model TEXT, response_model TEXT,
                input_tokens INTEGER, output_tokens INTEGER, cached_tokens INTEGER, reasoning_tokens INTEGER,
                tool_name TEXT, tool_call_id TEXT, tool_type TEXT,
                chat_session_id TEXT, turn_index INTEGER, ttft_ms REAL
            );
            CREATE TABLE span_attributes (
                span_id TEXT NOT NULL REFERENCES spans(span_id) ON DELETE CASCADE,
                key TEXT NOT NULL, value TEXT,
                PRIMARY KEY (span_id, key)
            );
            CREATE TABLE span_events (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                span_id TEXT NOT NULL REFERENCES spans(span_id) ON DELETE CASCADE,
                name TEXT NOT NULL, timestamp_ms INTEGER NOT NULL, attributes TEXT
            );",
        )
        .unwrap();
        (dir, db_path)
    }

    fn insert_span(
        conn: &rusqlite::Connection,
        span_id: &str,
        end_time_ms: i64,
        input_tokens: i64,
        output_tokens: i64,
    ) {
        conn.execute(
            "INSERT INTO spans (span_id, trace_id, name, start_time_ms, end_time_ms, status_code, \
             operation_name, provider_name, request_model, response_model, input_tokens, output_tokens, chat_session_id) \
             VALUES (?1, 'trace1', 'chat gpt-4.1', ?2, ?3, 0, 'chat', 'github', 'gpt-4.1', 'gpt-4.1-2025-04-14', ?4, ?5, 'session1')",
            rusqlite::params![span_id, end_time_ms - 1000, end_time_ms, input_tokens, output_tokens],
        )
        .unwrap();
    }

    #[test]
    fn test_empty_db_returns_empty_batch() {
        let (_dir, db_path) = create_test_otel_db();
        let watermark: Box<dyn WatermarkStrategy> = Box::new(TimestampCursorWatermark::initial());
        let batch = read_otel_spans_incremental(&db_path, watermark, 100).unwrap();
        assert!(batch.events.is_empty());
    }

    #[test]
    fn test_reads_spans_after_watermark() {
        let (_dir, db_path) = create_test_otel_db();
        let conn = rusqlite::Connection::open(&db_path).unwrap();
        insert_span(&conn, "span1", 1000, 100, 50);
        insert_span(&conn, "span2", 2000, 200, 100);
        insert_span(&conn, "span3", 3000, 300, 150);
        drop(conn);

        let watermark: Box<dyn WatermarkStrategy> =
            Box::new(TimestampCursorWatermark::new(1000, "span1".to_string()));
        let batch = read_otel_spans_incremental(&db_path, watermark, 100).unwrap();
        assert_eq!(batch.events.len(), 2);
        assert_eq!(batch.events[0]["span"]["span_id"], "span2");
        assert_eq!(batch.events[1]["span"]["span_id"], "span3");
    }

    #[test]
    fn test_batch_size_limits_results() {
        let (_dir, db_path) = create_test_otel_db();
        let conn = rusqlite::Connection::open(&db_path).unwrap();
        for i in 1..=5 {
            insert_span(&conn, &format!("span{}", i), i * 1000, i * 100, i * 50);
        }
        drop(conn);

        let watermark: Box<dyn WatermarkStrategy> = Box::new(TimestampCursorWatermark::initial());
        let batch = read_otel_spans_incremental(&db_path, watermark, 3).unwrap();
        assert_eq!(batch.events.len(), 3);
    }

    #[test]
    fn test_batch_resume_no_loss_no_repeats() {
        let (_dir, db_path) = create_test_otel_db();
        let conn = rusqlite::Connection::open(&db_path).unwrap();
        for i in 1..=5 {
            insert_span(&conn, &format!("span{}", i), i * 1000, i * 100, i * 50);
        }
        drop(conn);

        let mut watermark: Box<dyn WatermarkStrategy> =
            Box::new(TimestampCursorWatermark::initial());
        let mut all_ids = Vec::new();

        loop {
            let batch = read_otel_spans_incremental(&db_path, watermark, 2).unwrap();
            if batch.events.is_empty() {
                break;
            }
            for ev in &batch.events {
                all_ids.push(ev["span"]["span_id"].as_str().unwrap().to_string());
            }
            watermark = batch.new_watermark;
        }

        assert_eq!(all_ids, vec!["span1", "span2", "span3", "span4", "span5"]);
    }

    #[test]
    fn test_no_data_loss_with_duplicate_end_time_ms() {
        let (_dir, db_path) = create_test_otel_db();
        let conn = rusqlite::Connection::open(&db_path).unwrap();
        // 5 spans all sharing the same end_time_ms
        for i in 1..=5 {
            insert_span(&conn, &format!("span{}", i), 3000, i * 100, i * 50);
        }
        drop(conn);

        let mut watermark: Box<dyn WatermarkStrategy> =
            Box::new(TimestampCursorWatermark::initial());
        let mut all_ids = Vec::new();

        loop {
            let batch = read_otel_spans_incremental(&db_path, watermark, 2).unwrap();
            if batch.events.is_empty() {
                break;
            }
            for ev in &batch.events {
                all_ids.push(ev["span"]["span_id"].as_str().unwrap().to_string());
            }
            watermark = batch.new_watermark;
        }

        assert_eq!(all_ids, vec!["span1", "span2", "span3", "span4", "span5"]);
    }

    #[test]
    fn test_attributes_denormalized() {
        let (_dir, db_path) = create_test_otel_db();
        let conn = rusqlite::Connection::open(&db_path).unwrap();
        insert_span(&conn, "span1", 1000, 100, 50);
        conn.execute(
            "INSERT INTO span_attributes (span_id, key, value) VALUES ('span1', 'gen_ai.request.model', 'gpt-4.1')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO span_attributes (span_id, key, value) VALUES ('span1', 'gen_ai.agent.name', 'copilot')",
            [],
        )
        .unwrap();
        drop(conn);

        let watermark: Box<dyn WatermarkStrategy> = Box::new(TimestampCursorWatermark::initial());
        let batch = read_otel_spans_incremental(&db_path, watermark, 100).unwrap();
        assert_eq!(
            batch.events[0]["attributes"]["gen_ai.request.model"],
            "gpt-4.1"
        );
        assert_eq!(
            batch.events[0]["attributes"]["gen_ai.agent.name"],
            "copilot"
        );
    }

    #[test]
    fn test_span_events_denormalized() {
        let (_dir, db_path) = create_test_otel_db();
        let conn = rusqlite::Connection::open(&db_path).unwrap();
        insert_span(&conn, "span1", 1000, 100, 50);
        conn.execute(
            "INSERT INTO span_events (span_id, name, timestamp_ms, attributes) VALUES ('span1', 'tool_call', 500, '{\"tool\":\"read_file\"}')",
            [],
        )
        .unwrap();
        drop(conn);

        let watermark: Box<dyn WatermarkStrategy> = Box::new(TimestampCursorWatermark::initial());
        let batch = read_otel_spans_incremental(&db_path, watermark, 100).unwrap();
        let events = batch.events[0]["events"].as_array().unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0]["name"], "tool_call");
        assert_eq!(events[0]["timestamp_ms"], 500);
        assert_eq!(events[0]["attributes"]["tool"], "read_file");
    }

    #[test]
    fn test_extract_event_ids() {
        let event = serde_json::json!({
            "span": {
                "span_id": "abc123",
                "parent_span_id": "parent456",
                "tool_call_id": "call789",
            },
            "attributes": {},
            "events": [],
        });
        let (event_id, parent_id, tool_use_id) = extract_otel_event_ids(&event);
        assert_eq!(event_id, Some("abc123".to_string()));
        assert_eq!(parent_id, Some("parent456".to_string()));
        assert_eq!(tool_use_id, Some("call789".to_string()));
    }

    #[test]
    fn test_extract_event_ids_empty_tool_call_id() {
        let event = serde_json::json!({
            "span": { "span_id": "abc", "parent_span_id": null, "tool_call_id": "" },
            "attributes": {},
            "events": [],
        });
        let (event_id, parent_id, tool_use_id) = extract_otel_event_ids(&event);
        assert_eq!(event_id, Some("abc".to_string()));
        assert_eq!(parent_id, None);
        assert_eq!(tool_use_id, None);
    }

    #[test]
    fn test_extract_event_timestamp() {
        let event = serde_json::json!({
            "span": { "start_time_ms": 1716556800000_i64 },
        });
        let ts = extract_otel_event_timestamp(&event);
        assert_eq!(ts, Some(1716556800));
    }

    #[test]
    fn test_reads_from_real_fixture() {
        let fixture_path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/copilot-otel/traces.db");
        if !fixture_path.exists() {
            return;
        }
        let watermark: Box<dyn WatermarkStrategy> = Box::new(TimestampCursorWatermark::initial());
        let batch = read_otel_spans_incremental(&fixture_path, watermark, 100).unwrap();
        assert!(!batch.events.is_empty());
        let first = &batch.events[0];
        assert!(first.get("span").is_some());
        assert!(first.get("attributes").is_some());
        assert!(first.get("events").is_some());
        // Verify token fields are present
        assert!(first["span"].get("input_tokens").is_some());
        assert!(first["span"].get("output_tokens").is_some());
    }
}
