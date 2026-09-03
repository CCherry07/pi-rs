//! Opt-in raw JSONL anchor search, matching Hermes's Markdown request protocol.

use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};

use chrono::{DateTime, Local, NaiveDate, TimeZone, Utc};
use pi_core::{ContentBlock, TextContent, ToolResult};
use serde::Serialize;
use serde_json::{Value, json};
use walkdir::WalkDir;

const DEFAULT_LIMIT: usize = 50;
const MAX_LIMIT: usize = 100;
const MAX_FILES: usize = 5_000;
const MAX_LINES: usize = 500_000;

#[derive(Debug)]
struct Request {
    from: Option<i64>,
    to: Option<i64>,
    cwd: Option<String>,
    limit: usize,
    all: Vec<String>,
    any: Vec<String>,
    exclude: Vec<String>,
}

impl Request {
    fn has_time(&self) -> bool {
        self.from.is_some() || self.to.is_some()
    }

    fn has_text(&self) -> bool {
        !self.all.is_empty() || !self.any.is_empty()
    }
}

#[derive(Debug, Clone)]
struct Hit {
    path: PathBuf,
    line: usize,
    session_id: Option<String>,
    cwd: Option<String>,
    timestamp: Option<String>,
    text: String,
    score: usize,
    reason: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct Range {
    path: String,
    start_line: usize,
    end_line: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    session_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    cwd: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    start_time: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    end_time: Option<String>,
    score: usize,
    reason: String,
    #[serde(skip)]
    text: String,
}

pub(crate) fn execute(markdown: &str, roots: &[PathBuf]) -> Result<ToolResult, String> {
    let request = match parse_request(markdown) {
        Ok(request) => request,
        Err(error) => return failure(error),
    };
    if roots.iter().all(|root| !root.exists()) {
        let missing = roots.first().map_or_else(
            || "(not configured)".to_string(),
            |root| root.display().to_string(),
        );
        return failure(format!("sessionsDir does not exist: {missing}"));
    }
    let mut files = roots
        .iter()
        .filter(|root| root.exists())
        .flat_map(|root| {
            WalkDir::new(root)
                .follow_links(false)
                .into_iter()
                .filter_map(Result::ok)
                .filter(|entry| entry.file_type().is_file())
                .filter(|entry| {
                    entry.path().extension().and_then(|value| value.to_str()) == Some("jsonl")
                })
                .map(|entry| entry.into_path())
                .collect::<Vec<_>>()
        })
        .collect::<Vec<_>>();
    files.sort();
    files.dedup();
    if files.len() > MAX_FILES {
        return failure(format!(
            "Request too broad: {} session files exceed the configured scan cap of {MAX_FILES}. Add from/to, cwd, all, or any constraints.",
            files.len()
        ));
    }
    let mut scanned = 0_usize;
    let mut ranges = Vec::new();
    for file in files {
        match search_file(&file, &request, &mut scanned) {
            Ok(found) => ranges.extend(found),
            Err(error) => return failure(error),
        }
    }
    ranges.retain(|range| !contains_any(&range.text, &request.exclude));
    ranges.sort_by(|left, right| {
        if request.has_text() && left.score != right.score {
            return right.score.cmp(&left.score);
        }
        let left_time = parse_timestamp(left.start_time.as_deref()).unwrap_or_default();
        let right_time = parse_timestamp(right.start_time.as_deref()).unwrap_or_default();
        left_time
            .cmp(&right_time)
            .then_with(|| left.path.cmp(&right.path))
            .then_with(|| left.start_line.cmp(&right.start_line))
    });
    ranges.truncate(request.limit);
    let message = ranges
        .is_empty()
        .then(|| "No matching session anchors found.".to_string());
    let mut lines = vec![format!("count: {}", ranges.len())];
    if let Some(message) = message.as_deref() {
        lines.push(format!("message: {message}"));
    }
    if !ranges.is_empty() {
        lines.push("anchors:".to_string());
        for range in &ranges {
            let reason = compact_reason(&range.reason);
            let anchor = format!("{}:{}-{}", range.path, range.start_line, range.end_line);
            lines.push(if reason.is_empty() {
                format!("- {anchor}")
            } else {
                format!("- {anchor} — {reason}")
            });
        }
    }
    let output = lines.join("\n");
    Ok(tool_result(
        output.clone(),
        json!({"success":true,"count":ranges.len(),"message":message,"output":output,"ranges":ranges}),
        false,
    ))
}

fn failure(message: impl Into<String>) -> Result<ToolResult, String> {
    let message = message.into();
    Ok(tool_result(
        message.clone(),
        json!({"success":false,"message":message}),
        false,
    ))
}

fn parse_request(markdown: &str) -> Result<Request, String> {
    if markdown.trim().is_empty() {
        return Err("markdown is required".to_string());
    }
    let value_fields = HashSet::from(["from", "to", "cwd", "limit"]);
    let list_fields = HashSet::from(["all", "any", "exclude"]);
    let mut fields = HashMap::new();
    let mut lists = HashMap::from([
        ("all", Vec::new()),
        ("any", Vec::new()),
        ("exclude", Vec::new()),
    ]);
    let mut seen = HashSet::new();
    let mut current_list = None;
    for line in markdown.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        if let Some((field, value)) = line.split_once(':')
            && field
                .chars()
                .next()
                .is_some_and(|character| character.is_ascii_alphabetic())
            && field.chars().all(|character| {
                character.is_ascii_alphanumeric() || matches!(character, '_' | '-')
            })
        {
            if !value_fields.contains(field) && !list_fields.contains(field) {
                return Err(format!(
                    "Invalid field '{field}'. Supported fields: from, to, cwd, limit, all, any, exclude."
                ));
            }
            if !seen.insert(field.to_string()) {
                return Err(format!("Duplicate field '{field}'. Keep one value."));
            }
            if list_fields.contains(field) {
                if !value.trim().is_empty() {
                    return Err(format!(
                        "Invalid list section '{field}'. Use '{field}:' followed by '- item' lines."
                    ));
                }
                current_list = Some(field);
            } else {
                fields.insert(field, value.trim().to_string());
                current_list = None;
            }
            continue;
        }
        if let Some(term) = line.strip_prefix("- ") {
            let Some(list) = current_list else {
                return Err("List item found outside all, any, or exclude section.".to_string());
            };
            let term = term.trim();
            if term.is_empty() {
                return Err(format!(
                    "Empty term in '{list}'. Remove it or provide text."
                ));
            }
            lists
                .get_mut(list)
                .expect("known list")
                .push(term.to_string());
            continue;
        }
        return Err(format!("Invalid markdown line: {line}"));
    }
    let limit = fields.get("limit").map_or(Ok(DEFAULT_LIMIT), |value| {
        value
            .parse::<usize>()
            .ok()
            .filter(|value| *value > 0)
            .map(|value| value.min(MAX_LIMIT))
            .ok_or_else(|| "Invalid limit. Use a positive integer.".to_string())
    })?;
    let from = fields
        .get("from")
        .map(|value| {
            parse_boundary(value, false)
                .ok_or_else(|| "Invalid from. Use YYYY-MM-DD or an ISO timestamp.".to_string())
        })
        .transpose()?;
    let to = fields
        .get("to")
        .map(|value| {
            parse_boundary(value, true)
                .ok_or_else(|| "Invalid to. Use YYYY-MM-DD or an ISO timestamp.".to_string())
        })
        .transpose()?;
    if from.zip(to).is_some_and(|(from, to)| from > to) {
        return Err("Invalid time window. 'from' must be before or equal to 'to'.".to_string());
    }
    let cwd = fields.get("cwd").cloned();
    if cwd.as_deref().is_some_and(str::is_empty) {
        return Err("Invalid cwd. Provide a non-empty path.".to_string());
    }
    let all = lists.remove("all").unwrap_or_default();
    let any = lists.remove("any").unwrap_or_default();
    let exclude = lists.remove("exclude").unwrap_or_default();
    if from.is_none() && to.is_none() && cwd.is_none() && all.is_empty() && any.is_empty() {
        return Err(
            "Request needs at least one constraint: provide from/to, cwd, all, or any.".to_string(),
        );
    }
    Ok(Request {
        from,
        to,
        cwd,
        limit,
        all,
        any,
        exclude,
    })
}

fn parse_boundary(value: &str, end: bool) -> Option<i64> {
    if let Ok(date) = NaiveDate::parse_from_str(value, "%Y-%m-%d") {
        let naive = if end {
            date.and_hms_milli_opt(23, 59, 59, 999)?
        } else {
            date.and_hms_opt(0, 0, 0)?
        };
        return Local
            .from_local_datetime(&naive)
            .single()
            .map(|date| date.timestamp_millis());
    }
    DateTime::parse_from_rfc3339(value)
        .ok()
        .map(|date| date.timestamp_millis())
}

fn search_file(path: &Path, request: &Request, scanned: &mut usize) -> Result<Vec<Range>, String> {
    let raw = fs::read_to_string(path).map_err(|error| error.to_string())?;
    let mut session_id = None;
    let mut cwd = None;
    let mut hits = Vec::new();
    for (index, line) in raw.lines().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        *scanned += 1;
        if *scanned > MAX_LINES {
            return Err(format!(
                "Request too broad: scanned {scanned} session lines, exceeding the configured scan cap of {MAX_LINES}. Add from/to, cwd, all, or any constraints."
            ));
        }
        let event = serde_json::from_str::<Value>(line)
            .map_err(|_| format!("Invalid JSON in {}:{}", path.display(), index + 1))?;
        if let Some(value) = get_session_id(&event) {
            session_id = Some(value);
        }
        if let Some(value) = get_cwd(&event) {
            cwd = Some(value);
        }
        if request
            .cwd
            .as_deref()
            .is_some_and(|expected| cwd.as_deref() != Some(expected))
        {
            continue;
        }
        let timestamp = get_timestamp(&event);
        let timestamp_ms = parse_timestamp(timestamp.as_deref());
        if request.has_time()
            && (timestamp_ms.is_none()
                || request
                    .from
                    .is_some_and(|from| timestamp_ms.unwrap_or_default() < from)
                || request
                    .to
                    .is_some_and(|to| timestamp_ms.unwrap_or_default() > to))
        {
            continue;
        }
        let text = textualize(&event);
        let score = score_terms(&text, request);
        if request.has_text() && score == 0 {
            continue;
        }
        if !request.has_text() && timestamp_ms.is_none() {
            continue;
        }
        hits.push(Hit {
            path: path.to_path_buf(),
            line: index + 1,
            session_id: session_id.clone(),
            cwd: cwd.clone(),
            timestamp,
            text: text.clone(),
            score: if request.has_text() { score } else { 1 },
            reason: reason(request, &text),
        });
    }
    Ok(merge_hits(hits))
}

fn merge_hits(hits: Vec<Hit>) -> Vec<Range> {
    let mut ranges: Vec<Range> = Vec::new();
    for hit in hits {
        if let Some(last) = ranges.last_mut()
            && last.path == hit.path.to_string_lossy()
            && last.end_line + 1 == hit.line
            && last.reason == hit.reason
        {
            last.end_line = hit.line;
            last.score += hit.score;
            last.text.push('\n');
            last.text.push_str(&hit.text);
            if last.session_id.is_none() {
                last.session_id = hit.session_id;
            }
            if last.cwd.is_none() {
                last.cwd = hit.cwd;
            }
            if last.start_time.is_none() {
                last.start_time = hit.timestamp.clone();
            }
            if hit.timestamp.is_some() {
                last.end_time = hit.timestamp;
            }
            continue;
        }
        ranges.push(Range {
            path: hit.path.to_string_lossy().to_string(),
            start_line: hit.line,
            end_line: hit.line,
            session_id: hit.session_id,
            cwd: hit.cwd,
            start_time: hit.timestamp.clone(),
            end_time: hit.timestamp,
            score: hit.score,
            reason: hit.reason,
            text: hit.text,
        });
    }
    ranges
}

fn score_terms(text: &str, request: &Request) -> usize {
    let lower = text.to_lowercase();
    let matched_all = request
        .all
        .iter()
        .filter(|term| lower.contains(&term.to_lowercase()))
        .count();
    let matched_any = request
        .any
        .iter()
        .filter(|term| lower.contains(&term.to_lowercase()))
        .count();
    if !request.all.is_empty() && matched_all != request.all.len() {
        return 0;
    }
    if !request.any.is_empty() && matched_any == 0 {
        return 0;
    }
    if !request.has_text() {
        1
    } else {
        matched_all * 2 + matched_any
    }
}

fn reason(request: &Request, text: &str) -> String {
    if !request.has_text() {
        return match (request.has_time(), request.cwd.is_some()) {
            (true, true) => "cwd+time window",
            (true, false) => "time window",
            (false, true) => "cwd",
            (false, false) => "",
        }
        .to_string();
    }
    let lower = text.to_lowercase();
    let mut parts = Vec::new();
    if !request.all.is_empty() {
        parts.push(format!("matched all: {}", request.all.join(", ")));
    }
    let matched_any = request
        .any
        .iter()
        .filter(|term| lower.contains(&term.to_lowercase()))
        .cloned()
        .collect::<Vec<_>>();
    if !matched_any.is_empty() {
        parts.push(format!("matched any: {}", matched_any.join(", ")));
    }
    parts.join("; ")
}

fn contains_any(text: &str, terms: &[String]) -> bool {
    let lower = text.to_lowercase();
    terms
        .iter()
        .any(|term| lower.contains(&term.to_lowercase()))
}

fn get_timestamp(value: &Value) -> Option<String> {
    value
        .get("timestamp")
        .and_then(Value::as_str)
        .or_else(|| value.get("message")?.get("timestamp")?.as_str())
        .map(str::to_string)
}

fn get_session_id(value: &Value) -> Option<String> {
    value
        .get("sessionId")
        .or_else(|| value.get("session_id"))
        .and_then(Value::as_str)
        .or_else(|| {
            (value.get("type").and_then(Value::as_str) == Some("session"))
                .then(|| value.get("id")?.as_str())
                .flatten()
        })
        .or_else(|| value.get("session")?.get("id")?.as_str())
        .map(str::to_string)
}

fn get_cwd(value: &Value) -> Option<String> {
    value
        .get("cwd")
        .and_then(Value::as_str)
        .or_else(|| value.get("session")?.get("cwd")?.as_str())
        .map(str::to_string)
}

fn textualize(value: &Value) -> String {
    const METADATA: &[&str] = &[
        "type",
        "id",
        "parentId",
        "sessionId",
        "session_id",
        "timestamp",
        "cwd",
        "role",
        "customType",
    ];
    fn collect(value: &Value, key: Option<&str>, output: &mut Vec<String>) {
        match value {
            Value::String(value) if key.is_none_or(|key| !METADATA.contains(&key)) => {
                output.push(value.clone())
            }
            Value::Array(values) => values.iter().for_each(|value| collect(value, key, output)),
            Value::Object(values) => values
                .iter()
                .for_each(|(key, value)| collect(value, Some(key), output)),
            _ => {}
        }
    }
    let mut output = Vec::new();
    collect(value, None, &mut output);
    output.join("\n")
}

fn parse_timestamp(value: Option<&str>) -> Option<i64> {
    value
        .and_then(|value| DateTime::parse_from_rfc3339(value).ok())
        .map(|date| date.with_timezone(&Utc).timestamp_millis())
}

fn compact_reason(reason: &str) -> String {
    let one_line = reason.split_whitespace().collect::<Vec<_>>().join(" ");
    if one_line.encode_utf16().count() <= 180 {
        one_line
    } else {
        let mut units = 0;
        let prefix = one_line
            .chars()
            .take_while(|character| {
                let next = units + character.len_utf16();
                if next > 177 {
                    false
                } else {
                    units = next;
                    true
                }
            })
            .collect::<String>();
        format!("{prefix}...")
    }
}

fn tool_result(text: String, details: Value, is_error: bool) -> ToolResult {
    ToolResult {
        content: vec![ContentBlock::Text(TextContent::new(text))],
        details: Some(details),
        usage: None,
        added_tool_names: None,
        is_error,
        terminate: false,
    }
}
