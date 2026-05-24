use crate::transcripts::agents::opencode::open_sqlite_readonly;
use crate::transcripts::types::{TranscriptBatch, TranscriptError};
use crate::transcripts::watermark::{TimestampWatermark, WatermarkStrategy};
use chrono::{TimeZone, Utc};
use rusqlite::Connection;
use serde_json::json;
use std::collections::HashMap;
use std::path::Path;

/// Read OTEL spans incrementally from a Copilot traces SQLite DB.
///
/// Uses `end_time_ms` as the watermark column. Returns spans ordered by
/// `(end_time_ms ASC, span_id ASC)` to ensure deterministic pagination.
pub fn read_otel_spans_incremental(
    path: &Path,
    watermark: Box<dyn WatermarkStrategy>,
    batch_size: usize,
) -> Result<TranscriptBatch, TranscriptError> {
    let ts_watermark = watermark
        .as_any()
        .downcast_ref::<TimestampWatermark>()
        .ok_or_else(|| TranscriptError::Fatal {
            message: "OTEL stream requires TimestampWatermark".to_string(),
        })?;

    let watermark_millis = ts_watermark.0.timestamp_millis();
    let conn = open_sqlite_readonly(path)?;

    let spans = read_spans_after(&conn, watermark_millis, batch_size)?;
    if spans.is_empty() {
        return Ok(TranscriptBatch {
            events: vec![],
            new_watermark: Box::new(ts_watermark.clone()),
        });
    }

    let span_ids: Vec<&str> = spans.iter().map(|s| s.span_id.as_str()).collect();
    let attributes = read_attributes_for_spans(&conn, &span_ids)?;
    let events = read_events_for_spans(&conn, &span_ids)?;

    let max_end_time_ms = spans
        .iter()
        .map(|s| s.end_time_ms)
        .max()
        .unwrap_or(watermark_millis);
    let new_watermark = TimestampWatermark(
        Utc.timestamp_millis_opt(max_end_time_ms)
            .single()
            .unwrap_or(ts_watermark.0),
    );

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
    limit: usize,
) -> Result<Vec<SpanRow>, TranscriptError> {
    let mut stmt = conn
        .prepare(
            "SELECT span_id, trace_id, parent_span_id, name, start_time_ms, end_time_ms, \
             status_code, status_message, operation_name, provider_name, agent_name, \
             conversation_id, request_model, response_model, input_tokens, output_tokens, \
             cached_tokens, reasoning_tokens, tool_name, tool_call_id, tool_type, \
             chat_session_id, turn_index, ttft_ms \
             FROM spans WHERE end_time_ms > ?1 ORDER BY end_time_ms ASC, span_id ASC LIMIT ?2",
        )
        .map_err(|e| TranscriptError::Fatal {
            message: format!("Failed to prepare spans query: {}", e),
        })?;

    let rows = stmt
        .query_map(rusqlite::params![after_ms, limit as i64], |row| {
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
        "SELECT span_id, name, timestamp_ms, attributes FROM span_events \
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
        let (span_id, name, timestamp_ms, attributes_json) = row.map_err(|e| {
            TranscriptError::Fatal {
                message: format!("Failed to read event row: {}", e),
            }
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
