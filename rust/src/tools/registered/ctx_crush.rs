use std::collections::{BTreeMap, BTreeSet};

use chrono::DateTime;
use rmcp::ErrorData;
use rmcp::model::Tool;
use serde_json::{Map, Value, json};

use crate::server::tool_trait::{McpTool, ToolContext, ToolOutput};
use crate::tool_defs::tool_def;

const DEFAULT_MAX_ITEMS: usize = 5;
const MAX_ITEMS_LIMIT: usize = 1_000;
const MAX_STRING_CHARS: usize = 160;
const MAX_ARRAY_VALUES_TO_FLATTEN: usize = 10;
const ANOMALY_KEYWORDS: &[&str] = &[
    "error",
    "warning",
    "warn",
    "fail",
    "critical",
    "timeout",
    "timed out",
    "exception",
    "panic",
];

pub struct CtxCrushTool;

impl McpTool for CtxCrushTool {
    fn name(&self) -> &'static str {
        "ctx_crush"
    }

    fn tool_def(&self) -> Tool {
        tool_def(
            "ctx_crush",
            "Compress JSON arrays, nested JSON objects, and logs while preserving schema, anomalies, and representative samples.",
            json!({
                "type": "object",
                "properties": {
                    "content": {
                        "type": "string",
                        "description": "JSON, log output, or structured text to compress"
                    },
                    "mode": {
                        "type": "string",
                        "enum": ["auto", "json", "array", "log"],
                        "default": "auto",
                        "description": "Input type; auto detects JSON arrays, JSON objects, and log text"
                    },
                    "keep_anomalies": {
                        "type": "boolean",
                        "default": true,
                        "description": "Keep entries containing errors, warnings, failures, critical events, timeouts, or exceptions"
                    },
                    "max_items": {
                        "type": "integer",
                        "minimum": 1,
                        "maximum": 1000,
                        "default": 5,
                        "description": "Maximum normal sample entries; anomaly entries are retained in addition"
                    }
                },
                "required": ["content"]
            }),
        )
    }

    fn handle(
        &self,
        args: &Map<String, Value>,
        _ctx: &ToolContext,
    ) -> Result<ToolOutput, ErrorData> {
        let content = required_string(args, "content")?;
        let mode = optional_mode(args)?;
        let keep_anomalies = optional_bool(args, "keep_anomalies", true)?;
        let max_items = optional_max_items(args)?;
        let result = crush(content, mode, keep_anomalies, max_items)?;

        let response = json!({
            "compressed": result.compressed,
            "stats": {
                "original_tokens": result.original_tokens,
                "compressed_tokens": result.compressed_tokens,
                "ratio": result.ratio,
                "items_total": result.items_total,
                "items_shown": result.items_shown,
                "anomalies_found": result.anomalies_found,
                "delta_encoded": result.delta_encoded,
            }
        })
        .to_string();

        Ok(ToolOutput::with_savings(
            response,
            result.original_tokens,
            result
                .original_tokens
                .saturating_sub(result.compressed_tokens),
        ))
    }

    fn produces_machine_readable(&self, _args: Option<&Map<String, Value>>) -> bool {
        true
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum CrushMode {
    Auto,
    Json,
    Array,
    Log,
}

struct CrushResult {
    compressed: String,
    original_tokens: usize,
    compressed_tokens: usize,
    ratio: f64,
    items_total: usize,
    items_shown: usize,
    anomalies_found: usize,
    delta_encoded: bool,
}

struct RenderedContent {
    text: String,
    items_total: usize,
    items_shown: usize,
    anomalies_found: usize,
    delta_encoded: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SequentialDirection {
    Ascending,
    Descending,
}

pub type Direction = SequentialDirection;

#[derive(Clone, Debug, PartialEq)]
pub struct SequentialPattern {
    pub key_field: String,
    pub direction: SequentialDirection,
    pub step: f64,
}

#[derive(Clone, Debug, PartialEq)]
pub struct DeltaResult {
    pub header: String,
    pub deltas: Vec<String>,
    pub original_tokens: u64,
    pub compressed_tokens: u64,
}

#[derive(Clone, Debug)]
struct SchemaCompressedField {
    name: String,
    types: String,
    values: usize,
    samples: Vec<String>,
}

fn required_string<'a>(args: &'a Map<String, Value>, key: &str) -> Result<&'a str, ErrorData> {
    args.get(key)
        .and_then(Value::as_str)
        .ok_or_else(|| ErrorData::invalid_params(format!("{key} must be a string"), None))
}

fn optional_mode(args: &Map<String, Value>) -> Result<CrushMode, ErrorData> {
    let Some(value) = args.get("mode") else {
        return Ok(CrushMode::Auto);
    };
    match value.as_str() {
        Some("auto") => Ok(CrushMode::Auto),
        Some("json") => Ok(CrushMode::Json),
        Some("array") => Ok(CrushMode::Array),
        Some("log") => Ok(CrushMode::Log),
        _ => Err(ErrorData::invalid_params(
            "mode must be one of: auto, json, array, log",
            None,
        )),
    }
}

fn optional_bool(args: &Map<String, Value>, key: &str, default: bool) -> Result<bool, ErrorData> {
    match args.get(key) {
        None => Ok(default),
        Some(Value::Bool(value)) => Ok(*value),
        Some(_) => Err(ErrorData::invalid_params(
            format!("{key} must be a boolean"),
            None,
        )),
    }
}

fn optional_max_items(args: &Map<String, Value>) -> Result<usize, ErrorData> {
    let Some(value) = args.get("max_items") else {
        return Ok(DEFAULT_MAX_ITEMS);
    };
    let Some(value) = value.as_u64() else {
        return Err(ErrorData::invalid_params(
            "max_items must be an unsigned 32-bit integer",
            None,
        ));
    };
    let Ok(value) = u32::try_from(value) else {
        return Err(ErrorData::invalid_params(
            "max_items must be an unsigned 32-bit integer",
            None,
        ));
    };
    let value = value as usize;
    if value == 0 || value > MAX_ITEMS_LIMIT {
        return Err(ErrorData::invalid_params(
            format!("max_items must be between 1 and {MAX_ITEMS_LIMIT}"),
            None,
        ));
    }
    Ok(value)
}

fn crush(
    content: &str,
    mode: CrushMode,
    keep_anomalies: bool,
    max_items: usize,
) -> Result<CrushResult, ErrorData> {
    let rendered = match mode {
        CrushMode::Log => crush_logs(content, keep_anomalies, max_items),
        CrushMode::Array => {
            let value = parse_json(content, "array")?;
            let Value::Array(values) = value else {
                return Err(ErrorData::invalid_params(
                    "content must be a JSON array",
                    None,
                ));
            };
            crush_array(&values, keep_anomalies, max_items)
        }
        CrushMode::Json => crush_json(parse_json(content, "json")?, keep_anomalies, max_items),
        CrushMode::Auto => match serde_json::from_str(content) {
            Ok(value) => crush_json(value, keep_anomalies, max_items),
            Err(_) => crush_logs(content, keep_anomalies, max_items),
        },
    };

    let original_tokens = crate::core::tokens::count_tokens(content);
    let compressed_tokens = crate::core::tokens::count_tokens(&rendered.text);
    let ratio = if original_tokens == 0 {
        1.0
    } else {
        compressed_tokens as f64 / original_tokens as f64
    };

    Ok(CrushResult {
        compressed: rendered.text,
        original_tokens,
        compressed_tokens,
        ratio,
        items_total: rendered.items_total,
        items_shown: rendered.items_shown,
        anomalies_found: rendered.anomalies_found,
        delta_encoded: rendered.delta_encoded,
    })
}

fn parse_json(content: &str, expected: &str) -> Result<Value, ErrorData> {
    serde_json::from_str(content).map_err(|error| {
        ErrorData::invalid_params(
            format!("content must be valid JSON for mode={expected}: {error}"),
            None,
        )
    })
}

fn crush_json(value: Value, keep_anomalies: bool, max_items: usize) -> RenderedContent {
    match value {
        Value::Array(values) => crush_array(&values, keep_anomalies, max_items),
        Value::Object(object) => crush_object(&object),
        scalar => {
            let text = format!(
                "value = {}\n[JSON scalar preserved]",
                render_scalar(&scalar)
            );
            RenderedContent {
                text,
                items_total: 1,
                items_shown: 1,
                anomalies_found: usize::from(value_has_anomaly(&scalar)),
                delta_encoded: false,
            }
        }
    }
}

fn crush_array(values: &[Value], keep_anomalies: bool, max_items: usize) -> RenderedContent {
    if values.iter().all(Value::is_object) {
        if let Some(pattern) = is_sequential(values) {
            return crush_sequential_array(values, &pattern);
        }
        return crush_object_array(values, keep_anomalies, max_items);
    }
    crush_generic_array(values, keep_anomalies, max_items)
}

fn crush_object_array(values: &[Value], keep_anomalies: bool, max_items: usize) -> RenderedContent {
    let total = values.len();
    let anomalies: BTreeSet<usize> = values
        .iter()
        .enumerate()
        .filter_map(|(index, value)| value_has_anomaly(value).then_some(index))
        .collect();
    let constant_fields = constant_fields(values);
    let mut shown = if total > 10 {
        sample_indices(total, max_items, true)
    } else {
        (0..total).collect()
    };
    if keep_anomalies {
        shown.extend(anomalies.iter().copied());
    }

    let schema_count = if total > 10 { 1 } else { total.min(2) };
    let mut lines = Vec::new();
    if schema_count == 0 {
        lines.push("schema: []".to_string());
    } else {
        lines.push(format!("schema ({schema_count} of {total}):"));
        for (index, value) in values.iter().enumerate().take(schema_count) {
            lines.push(format!(
                "#{} {}",
                index + 1,
                compact_sample_object(value, &constant_fields)
            ));
        }
    }

    if !constant_fields.is_empty() {
        lines.push("constants:".to_string());
        for (field, value, count) in &constant_fields {
            lines.push(format!("  {field}={value} ({count}/{total})"));
        }
    }

    let sample_indices: Vec<usize> = shown
        .iter()
        .copied()
        .filter(|index| *index >= schema_count)
        .collect();
    if !sample_indices.is_empty() {
        lines.push("samples:".to_string());
        for index in sample_indices {
            let rendered = if anomalies.contains(&index) {
                compact_json(&values[index])
            } else {
                compact_sample_object(&values[index], &constant_fields)
            };
            lines.push(format!("#{} {rendered}", index + 1));
        }
    }
    lines.push(format!(
        "[{total} items, {} shown, schema preserved]",
        shown.len()
    ));

    RenderedContent {
        text: lines.join("\n"),
        items_total: total,
        items_shown: shown.len(),
        anomalies_found: anomalies.len(),
        delta_encoded: false,
    }
}

fn crush_sequential_array(values: &[Value], pattern: &SequentialPattern) -> RenderedContent {
    let schema_fields = schema_compressed_fields(values, pattern);
    let schema_field_names: BTreeSet<String> = schema_fields
        .iter()
        .map(|field| field.name.clone())
        .collect();
    let delta = delta_compress_with_schema(values, pattern, &schema_field_names);
    let anomalies_found = values
        .iter()
        .filter(|value| value_has_anomaly(value))
        .count();

    let mut lines = Vec::with_capacity(delta.deltas.len() + schema_fields.len() + 2);
    lines.push(delta.header);
    lines.extend(delta.deltas);
    if !schema_fields.is_empty() {
        let fields = schema_fields
            .iter()
            .map(|field| {
                let samples = field.samples.join(", ");
                format!(
                    "{}:{} ({} values; samples {samples})",
                    field.name, field.types, field.values
                )
            })
            .collect::<Vec<_>>()
            .join("; ");
        lines.push(format!("[schema-compressed varying fields: {fields}]"));
    }
    lines.push(format!(
        "[{} items total, {} delta entries, {anomalies_found} anomalies retained]",
        values.len(),
        values.len().saturating_sub(1)
    ));

    RenderedContent {
        text: lines.join("\n"),
        items_total: values.len(),
        items_shown: values.len(),
        anomalies_found,
        delta_encoded: true,
    }
}

/// Finds a shared numeric or RFC 3339 timestamp field with a constant non-zero step.
pub fn is_sequential(items: &[Value]) -> Option<SequentialPattern> {
    if items.len() < 5 || !items.iter().all(Value::is_object) {
        return None;
    }

    let mut patterns: Vec<SequentialPattern> = shared_object_fields(items)
        .into_iter()
        .filter_map(|field| sequential_pattern_for_field(items, &field))
        .collect();
    patterns.sort_by(|left, right| {
        sequence_key_rank(&left.key_field)
            .cmp(&sequence_key_rank(&right.key_field))
            .then_with(|| left.key_field.cmp(&right.key_field))
    });
    patterns.into_iter().next()
}

/// Encodes each entry as changes from its predecessor, retaining the first entry in full.
pub fn delta_compress(items: &[Value], pattern: &SequentialPattern) -> DeltaResult {
    delta_compress_with_schema(items, pattern, &BTreeSet::new())
}

fn delta_compress_with_schema(
    items: &[Value],
    pattern: &SequentialPattern,
    schema_compressed: &BTreeSet<String>,
) -> DeltaResult {
    let base = items
        .first()
        .map(compact_json)
        .unwrap_or_else(|| "[]".to_string());
    let header = format!(
        "[DELTA base={base}; key={} {}; step={}; count={}]",
        pattern.key_field,
        direction_name(pattern.direction),
        format_number(pattern.step),
        items.len()
    );
    let sequential_fields = sequential_field_names(items, pattern);

    let anomaly_indices: Vec<usize> = items
        .iter()
        .enumerate()
        .filter(|(_, item)| value_has_anomaly(item))
        .map(|(index, _)| index)
        .collect();

    let non_anomaly_deltas_all_predictable = items.len() > 5
        && anomaly_indices.len() <= items.len() / 4
        && non_anomaly_fields_are_predictable(items, &sequential_fields, schema_compressed);

    let mut deltas = Vec::new();

    if non_anomaly_deltas_all_predictable {
        let varying = collect_varying_field_ranges(items, &sequential_fields, schema_compressed);
        if !varying.is_empty() {
            deltas.push(format!("varying: {}", varying.join(", ")));
        }
        for &index in &anomaly_indices {
            deltas.push(format!("#{}: {}", index + 1, compact_json(&items[index])));
        }
    } else {
        for index in 1..items.len() {
            let previous = items[index - 1]
                .as_object()
                .expect("delta encoding requires object entries");
            let current = items[index]
                .as_object()
                .expect("delta encoding requires object entries");
            let preserve_schema_fields =
                value_has_anomaly(&items[index - 1]) || value_has_anomaly(&items[index]);
            let mut changes = Vec::new();

            for field in &sequential_fields {
                let (Some(previous), Some(current)) = (previous.get(field), current.get(field))
                else {
                    continue;
                };
                if previous != current {
                    changes.push(format!(
                        "{field}:{}",
                        render_sequence_delta(previous, current)
                    ));
                }
            }

            let mut other_fields = object_field_union(previous, current);
            other_fields.retain(|field| !sequential_fields.contains(field));
            for field in other_fields {
                if schema_compressed.contains(&field) && !preserve_schema_fields {
                    continue;
                }
                let previous = previous.get(&field);
                let current = current.get(&field);
                if previous != current {
                    changes.push(format!(
                        "{field}:{}->{}",
                        render_optional_scalar(previous),
                        render_optional_scalar(current)
                    ));
                }
            }

            deltas.push(format!("D{index}: {{{}}}", changes.join(",")));
        }
    }

    let original = compact_json(&Value::Array(items.to_vec()));
    let mut compressed = Vec::with_capacity(deltas.len() + 1);
    compressed.push(header.clone());
    compressed.extend(deltas.iter().cloned());
    DeltaResult {
        header,
        deltas,
        original_tokens: token_count_u64(&original),
        compressed_tokens: token_count_u64(&compressed.join("\n")),
    }
}

fn non_anomaly_fields_are_predictable(
    items: &[Value],
    sequential_fields: &[String],
    schema_compressed: &BTreeSet<String>,
) -> bool {
    for index in 1..items.len() {
        if value_has_anomaly(&items[index]) || value_has_anomaly(&items[index - 1]) {
            continue;
        }
        let (Some(prev), Some(curr)) = (items[index - 1].as_object(), items[index].as_object())
        else {
            return false;
        };
        for (field, current_val) in curr {
            if sequential_fields.contains(field) || schema_compressed.contains(field) {
                continue;
            }
            let prev_val = prev.get(field);
            if prev_val != Some(current_val) && prev_val.is_some() {
                return false;
            }
        }
    }
    true
}

fn collect_varying_field_ranges(
    items: &[Value],
    sequential_fields: &[String],
    schema_compressed: &BTreeSet<String>,
) -> Vec<String> {
    let Some(first) = items.first().and_then(Value::as_object) else {
        return Vec::new();
    };
    let Some(last) = items.last().and_then(Value::as_object) else {
        return Vec::new();
    };
    let mut ranges = Vec::new();
    for (field, first_val) in first {
        if sequential_fields.contains(field) || schema_compressed.contains(field) {
            continue;
        }
        if let Some(last_val) = last.get(field) {
            if first_val != last_val {
                ranges.push(format!(
                    "{field}=[{}..{}]",
                    render_optional_scalar(Some(first_val)),
                    render_optional_scalar(Some(last_val))
                ));
            }
        }
    }
    ranges
}

fn shared_object_fields(items: &[Value]) -> BTreeSet<String> {
    let Some(first) = items.first().and_then(Value::as_object) else {
        return BTreeSet::new();
    };
    let mut fields: BTreeSet<String> = first.keys().cloned().collect();
    for item in &items[1..] {
        let Some(object) = item.as_object() else {
            return BTreeSet::new();
        };
        fields.retain(|field| object.contains_key(field));
    }
    fields
}

fn sequential_pattern_for_field(items: &[Value], field: &str) -> Option<SequentialPattern> {
    let values: Vec<&Value> = items
        .iter()
        .map(|item| item.as_object()?.get(field))
        .collect::<Option<_>>()?;
    let numbers = values
        .iter()
        .map(|value| numeric_value(value))
        .collect::<Option<Vec<_>>>();
    let timestamps = values
        .iter()
        .map(|value| timestamp_value(value))
        .collect::<Option<Vec<_>>>();

    let (direction, step) = numbers
        .as_deref()
        .and_then(sequence_step)
        .or_else(|| timestamps.as_deref().and_then(sequence_step))?;
    Some(SequentialPattern {
        key_field: field.to_string(),
        direction,
        step,
    })
}

fn sequence_step(values: &[f64]) -> Option<(SequentialDirection, f64)> {
    let (&first, &second) = values.first().zip(values.get(1))?;
    let difference = second - first;
    if !difference.is_finite() || difference == 0.0 {
        return None;
    }
    let step = difference.abs();
    if values
        .windows(2)
        .all(|pair| approximately_equal(pair[1] - pair[0], difference))
    {
        Some((
            if difference.is_sign_positive() {
                SequentialDirection::Ascending
            } else {
                SequentialDirection::Descending
            },
            step,
        ))
    } else {
        None
    }
}

fn approximately_equal(left: f64, right: f64) -> bool {
    (left - right).abs() <= 1e-9 * left.abs().max(right.abs()).max(1.0)
}

fn timestamp_value(value: &Value) -> Option<f64> {
    let timestamp = DateTime::parse_from_rfc3339(value.as_str()?).ok()?;
    Some(timestamp.timestamp() as f64 + f64::from(timestamp.timestamp_subsec_nanos()) / 1e9)
}

fn numeric_value(value: &Value) -> Option<f64> {
    value
        .as_f64()
        .or_else(|| value.as_str()?.parse::<f64>().ok())
        .filter(|value| value.is_finite())
}

fn sequence_key_rank(field: &str) -> u8 {
    let lower = field.to_ascii_lowercase();
    u8::from(
        ![
            "id",
            "timestamp",
            "time",
            "date",
            "ts",
            "counter",
            "count",
            "sequence",
            "seq",
            "index",
        ]
        .iter()
        .any(|hint| lower == *hint || lower.contains(hint)),
    )
}

fn direction_name(direction: SequentialDirection) -> &'static str {
    match direction {
        SequentialDirection::Ascending => "ascending",
        SequentialDirection::Descending => "descending",
    }
}

fn sequential_field_names(items: &[Value], pattern: &SequentialPattern) -> Vec<String> {
    let mut fields: BTreeSet<String> = shared_object_fields(items)
        .into_iter()
        .filter(|field| sequential_pattern_for_field(items, field).is_some())
        .collect();
    fields.insert(pattern.key_field.clone());

    let mut ordered = vec![pattern.key_field.clone()];
    ordered.extend(
        fields
            .into_iter()
            .filter(|field| field != &pattern.key_field),
    );
    ordered
}

fn schema_compressed_fields(
    items: &[Value],
    pattern: &SequentialPattern,
) -> Vec<SchemaCompressedField> {
    let sequential_fields: BTreeSet<String> =
        sequential_field_names(items, pattern).into_iter().collect();
    let mut result = Vec::new();

    for field in object_fields(items) {
        if sequential_fields.contains(&field) {
            continue;
        }
        let values: Vec<&Value> = items
            .iter()
            .filter_map(|item| item.as_object()?.get(&field))
            .collect();
        let unique: BTreeSet<String> = values.iter().map(|value| compact_json(value)).collect();
        if values.len() < 3 || unique.len() < 3 || unique.len() * 5 < values.len() * 4 {
            continue;
        }

        let types: BTreeSet<&str> = values.iter().map(|value| value_kind(value)).collect();
        let mut samples = Vec::new();
        if let Some(first) = values.first() {
            samples.push(compact_json(first));
        }
        if let Some(last) = values.last() {
            let last = compact_json(last);
            if samples.last() != Some(&last) {
                samples.push(last);
            }
        }
        result.push(SchemaCompressedField {
            name: field,
            types: types.into_iter().collect::<Vec<_>>().join("|"),
            values: values.len(),
            samples,
        });
    }
    result
}

fn object_fields(items: &[Value]) -> BTreeSet<String> {
    items
        .iter()
        .filter_map(Value::as_object)
        .flat_map(|object| object.keys().cloned())
        .collect()
}

fn object_field_union(
    previous: &Map<String, Value>,
    current: &Map<String, Value>,
) -> BTreeSet<String> {
    previous.keys().chain(current.keys()).cloned().collect()
}

fn value_kind(value: &Value) -> &'static str {
    match value {
        Value::Null => "null",
        Value::Bool(_) => "bool",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}

fn render_sequence_delta(previous: &Value, current: &Value) -> String {
    if let (Some(previous), Some(current)) = (numeric_value(previous), numeric_value(current)) {
        return format_signed_number(current - previous);
    }
    if let (Some(previous), Some(current)) = (timestamp_value(previous), timestamp_value(current)) {
        return format!("{}s", format_signed_number(current - previous));
    }
    format!("{}->{}", render_scalar(previous), render_scalar(current))
}

fn render_optional_scalar(value: Option<&Value>) -> String {
    value.map_or_else(|| "<absent>".to_string(), render_scalar)
}

fn format_signed_number(value: f64) -> String {
    if value >= 0.0 {
        format!("+{}", format_number(value))
    } else {
        format_number(value)
    }
}

fn format_number(value: f64) -> String {
    if value == 0.0 {
        return "0".to_string();
    }
    if value.fract() == 0.0 {
        return format!("{value:.0}");
    }
    value.to_string()
}

fn token_count_u64(text: &str) -> u64 {
    u64::try_from(crate::core::tokens::count_tokens(text)).unwrap_or(u64::MAX)
}

fn crush_generic_array(
    values: &[Value],
    keep_anomalies: bool,
    max_items: usize,
) -> RenderedContent {
    let total = values.len();
    let anomalies: BTreeSet<usize> = values
        .iter()
        .enumerate()
        .filter_map(|(index, value)| value_has_anomaly(value).then_some(index))
        .collect();
    let mut shown = if total > 10 {
        sample_indices(total, max_items, false)
    } else {
        (0..total).collect()
    };
    if keep_anomalies {
        shown.extend(anomalies.iter().copied());
    }

    let mut lines = vec!["array samples:".to_string()];
    for index in &shown {
        lines.push(format!("#{} {}", index + 1, compact_json(&values[*index])));
    }
    lines.push(format!(
        "[{total} items total, {} shown, representative sample preserved]",
        shown.len()
    ));

    RenderedContent {
        text: lines.join("\n"),
        items_total: total,
        items_shown: shown.len(),
        anomalies_found: anomalies.len(),
        delta_encoded: false,
    }
}

fn sample_indices(total: usize, max_items: usize, preserve_schema: bool) -> BTreeSet<usize> {
    if total == 0 {
        return BTreeSet::new();
    }

    let minimum = if preserve_schema { total.min(2) } else { 1 };
    let mut capacity = max_items.max(minimum).min(total);
    if total > capacity {
        let boundary_count = if preserve_schema {
            total.min(4)
        } else {
            total.min(2)
        };
        capacity = capacity.max(boundary_count);
    }
    if total <= capacity {
        return (0..total).collect();
    }

    let mut selected = BTreeSet::new();
    if preserve_schema {
        selected.insert(0);
        if total > 1 {
            selected.insert(1);
        }
    } else {
        selected.insert(0);
    }
    selected.insert(total - 1);
    if preserve_schema && total > 2 {
        selected.insert(total - 2);
    }

    let needed = capacity.saturating_sub(selected.len());
    let start = if preserve_schema { 2 } else { 1 };
    let end = if preserve_schema {
        total.saturating_sub(2)
    } else {
        total.saturating_sub(1)
    };
    for slot in 1..=needed {
        let span = end.saturating_sub(start);
        let index = start + (span * slot / (needed + 1));
        selected.insert(index);
    }
    for index in start..end {
        if selected.len() >= capacity {
            break;
        }
        selected.insert(index);
    }
    selected
}

fn constant_fields(values: &[Value]) -> Vec<(String, String, usize)> {
    let total = values.len();
    let mut fields: BTreeMap<String, BTreeMap<String, usize>> = BTreeMap::new();

    for value in values {
        let Some(object) = value.as_object() else {
            continue;
        };
        for (field, value) in object {
            if value.is_null() || value.as_str().is_some_and(|text| text.trim().is_empty()) {
                continue;
            }
            if value.is_object() || value.is_array() {
                continue;
            }
            *fields
                .entry(field.clone())
                .or_default()
                .entry(compact_json(value))
                .or_default() += 1;
        }
    }

    fields
        .into_iter()
        .filter_map(|(field, values)| {
            values.into_iter().find_map(|(value, count)| {
                ((count as f64 / total.max(1) as f64) > 0.8).then_some((
                    field.clone(),
                    value,
                    count,
                ))
            })
        })
        .collect()
}

fn compact_sample_object(value: &Value, constant_fields: &[(String, String, usize)]) -> String {
    let Some(object) = value.as_object() else {
        return compact_json(value);
    };
    let mut compacted = object.clone();
    for (field, _, _) in constant_fields {
        compacted.remove(field);
    }
    compact_json(&Value::Object(compacted))
}

fn crush_object(object: &Map<String, Value>) -> RenderedContent {
    let mut lines = Vec::new();
    flatten_value("", &Value::Object(object.clone()), &mut lines);
    let shown = lines.len();
    lines.push(format!("[object flattened: {shown} values shown]"));

    RenderedContent {
        text: lines.join("\n"),
        items_total: 1,
        items_shown: 1,
        anomalies_found: usize::from(value_has_anomaly(&Value::Object(object.clone()))),
        delta_encoded: false,
    }
}

fn flatten_value(path: &str, value: &Value, lines: &mut Vec<String>) {
    match value {
        Value::Null => {}
        Value::String(text) if text.trim().is_empty() => {}
        Value::Object(object) => {
            for (key, value) in object {
                let child_path = if path.is_empty() {
                    key.clone()
                } else {
                    format!("{path}.{key}")
                };
                flatten_value(&child_path, value, lines);
            }
        }
        Value::Array(values) => {
            if values.is_empty() {
                return;
            }
            let shown = values.len().min(MAX_ARRAY_VALUES_TO_FLATTEN);
            if shown < values.len() {
                lines.push(format!(
                    "{path} = [{} items; first {shown} shown]",
                    values.len()
                ));
            }
            for (index, value) in values.iter().take(shown).enumerate() {
                flatten_value(&format!("{path}[{index}]"), value, lines);
            }
        }
        scalar => lines.push(format!("{path} = {}", render_scalar(scalar))),
    }
}

fn render_scalar(value: &Value) -> String {
    match value {
        Value::String(text) => compact_json(&Value::String(abbreviate(text))),
        _ => compact_json(value),
    }
}

fn abbreviate(text: &str) -> String {
    let length = text.chars().count();
    if length <= MAX_STRING_CHARS {
        return text.to_string();
    }
    let prefix: String = text.chars().take(MAX_STRING_CHARS).collect();
    format!("{prefix}… (+{} chars)", length - MAX_STRING_CHARS)
}

fn compact_json(value: &Value) -> String {
    serde_json::to_string(value).unwrap_or_else(|_| "<unserializable JSON>".to_string())
}

fn crush_logs(content: &str, keep_anomalies: bool, max_items: usize) -> RenderedContent {
    let mut groups = Vec::<LogGroup>::new();
    let mut positions = BTreeMap::<String, usize>::new();
    let mut anomalies_found = 0;

    for line in content
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
    {
        let anomaly = text_has_anomaly(line);
        anomalies_found += usize::from(anomaly);
        let pattern = log_pattern(line);
        if let Some(index) = positions.get(&pattern) {
            groups[*index].count += 1;
            groups[*index].anomaly |= anomaly;
        } else {
            positions.insert(pattern, groups.len());
            groups.push(LogGroup {
                line: line.to_string(),
                count: 1,
                anomaly,
            });
        }
    }

    let normal_indices: Vec<usize> = groups
        .iter()
        .enumerate()
        .filter_map(|(index, group)| (!group.anomaly || !keep_anomalies).then_some(index))
        .collect();
    let mut shown: BTreeSet<usize> = sample_positions(&normal_indices, max_items);
    if keep_anomalies {
        shown.extend(
            groups
                .iter()
                .enumerate()
                .filter_map(|(index, group)| group.anomaly.then_some(index)),
        );
    }

    let mut lines = vec!["log samples:".to_string()];
    for index in &shown {
        let group = &groups[*index];
        let suffix = (group.count > 1).then(|| format!(" [x{}]", group.count));
        lines.push(format!("{}{}", group.line, suffix.unwrap_or_default()));
    }
    let total_lines: usize = groups.iter().map(|group| group.count).sum();
    let collapsed = total_lines.saturating_sub(groups.len());
    lines.push(format!(
        "[{total_lines} lines total, {} patterns shown, {collapsed} duplicate lines collapsed]",
        shown.len()
    ));

    RenderedContent {
        text: lines.join("\n"),
        items_total: total_lines,
        items_shown: shown.len(),
        anomalies_found,
        delta_encoded: false,
    }
}

struct LogGroup {
    line: String,
    count: usize,
    anomaly: bool,
}

fn sample_positions(positions: &[usize], max_items: usize) -> BTreeSet<usize> {
    if positions.len() <= max_items {
        return positions.iter().copied().collect();
    }
    let sampled = sample_indices(positions.len(), max_items, false);
    sampled
        .into_iter()
        .filter_map(|index| positions.get(index).copied())
        .collect()
}

fn log_pattern(line: &str) -> String {
    let mut pattern = String::with_capacity(line.len());
    let mut in_digits = false;
    for character in line.chars() {
        if character.is_ascii_digit() {
            if !in_digits {
                pattern.push_str("<n>");
                in_digits = true;
            }
        } else {
            pattern.push(character);
            in_digits = false;
        }
    }
    pattern
}

fn value_has_anomaly(value: &Value) -> bool {
    text_has_anomaly(&compact_json(value))
}

fn text_has_anomaly(text: &str) -> bool {
    let lower = text.to_ascii_lowercase();
    ANOMALY_KEYWORDS
        .iter()
        .any(|keyword| lower.contains(keyword))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::server::tool_trait::McpTool;

    fn crush_auto(content: &str) -> CrushResult {
        crush(content, CrushMode::Auto, true, DEFAULT_MAX_ITEMS).unwrap()
    }

    #[test]
    fn array_compression_preserves_schema_samples_and_repetitive_fields() {
        let values: Vec<Value> = (1..=15)
            .map(|id| json!({"id": id * id, "status": "ok", "source": "search"}))
            .collect();
        let input = serde_json::to_string(&values).unwrap();

        let result = crush_auto(&input);

        assert!(result.compressed.contains("schema ("));
        assert!(result.compressed.contains("status=") && result.compressed.contains("15/15"));
        assert!(
            result.compressed.contains("[15 items")
                && result.compressed.contains("schema preserved]")
        );
        assert_eq!(result.items_total, 15);
        assert_eq!(result.items_shown, 5);
        assert!(result.compressed_tokens < result.original_tokens);
    }

    #[test]
    fn sequential_ids_are_detected_with_their_step_and_direction() {
        let values: Vec<Value> = (10..15)
            .map(|id| json!({"id": id, "status": "ok"}))
            .collect();

        let pattern = is_sequential(&values).expect("sequential IDs should be detected");

        assert_eq!(pattern.key_field, "id");
        assert_eq!(pattern.direction, SequentialDirection::Ascending);
        assert_eq!(pattern.step, 1.0);
    }

    #[test]
    fn iso_timestamps_are_detected_as_sequential() {
        let values: Vec<Value> = (0..5)
            .map(|offset| {
                json!({"timestamp": format!("2026-08-13T10:{offset:02}:00Z"), "event": "tick"})
            })
            .collect();

        let pattern = is_sequential(&values).expect("regular timestamps should be detected");

        assert_eq!(pattern.key_field, "timestamp");
        assert_eq!(pattern.direction, SequentialDirection::Ascending);
        assert_eq!(pattern.step, 60.0);
    }

    #[test]
    fn delta_compression_is_smaller_than_schema_sampling_for_sequential_entries() {
        let values: Vec<Value> = (1..=5)
            .map(|id| json!({"id": id, "status": "ok", "ts": id * 100}))
            .collect();
        let pattern = is_sequential(&values).expect("sequential IDs should be detected");

        let delta = delta_compress(&values, &pattern);
        let schema = crush_object_array(&values, true, DEFAULT_MAX_ITEMS);

        assert!(delta.header.starts_with("[DELTA base={"));
        assert!(delta.deltas[0].contains("id:+1"));
        assert!(delta.deltas[1].contains("ts:+100"));
        assert!(delta.compressed_tokens < token_count_u64(&schema.text));
    }

    #[test]
    fn non_sequential_data_uses_the_existing_schema_compressor() {
        let values = json!([
            {"id": 1, "status": "ok"},
            {"id": 3, "status": "ok"},
            {"id": 8, "status": "ok"},
            {"id": 14, "status": "ok"},
            {"id": 23, "status": "ok"},
            {"id": 37, "status": "ok"}
        ]);
        let input = values.to_string();

        let result = crush_auto(&input);

        assert!(!result.delta_encoded);
        assert!(result.compressed.contains("schema ("));
    }

    #[test]
    fn mixed_sequential_and_random_fields_use_delta_and_schema_compression() {
        let values: Vec<Value> = (1..=8)
            .map(|id| {
                let score = [4, 9, 2, 8, 1, 7, 3, 6][id as usize - 1];
                json!({
                    "id": id,
                    "status": if id % 2 == 0 { "retry" } else { "ok" },
                    "request_id": format!("request-{:02x}", id * 37),
                    "score": score
                })
            })
            .collect();
        let input = serde_json::to_string(&values).unwrap();

        let result = crush_auto(&input);

        assert!(result.delta_encoded);
        assert!(
            result
                .compressed
                .contains("D1: {id:+1,status:\"ok\"->\"retry\"}")
        );
        assert!(
            result
                .compressed
                .contains("[schema-compressed varying fields:")
        );
        assert!(result.compressed.contains("request_id:string (8 values;"));
        assert!(result.compressed.contains("score:number (8 values;"));
    }

    #[test]
    fn object_flattening_omits_empty_values_and_abbreviates_long_strings() {
        let input = json!({
            "user": {"name": "Ada", "profile": {"bio": "x".repeat(200), "empty": ""}},
            "missing": null,
            "tags": ["rust", "mcp"]
        })
        .to_string();

        let result = crush_auto(&input);

        assert!(result.compressed.contains("user.name = \"Ada\""));
        assert!(result.compressed.contains("user.profile.bio ="));
        assert!(result.compressed.contains("… (+40 chars)"));
        assert!(result.compressed.contains("tags[0] = \"rust\""));
        assert!(!result.compressed.contains("missing"));
        assert!(!result.compressed.contains("empty"));
    }

    #[test]
    fn anomaly_entries_are_retained_beyond_the_normal_sample_limit() {
        let values: Vec<Value> = (1..=20)
            .map(|id| {
                if id == 9 {
                    json!({"id": id, "status": "error", "message": "timeout contacting API"})
                } else {
                    json!({"id": id, "status": "ok"})
                }
            })
            .collect();
        let input = serde_json::to_string(&values).unwrap();

        let result = crush_auto(&input);

        assert!(
            result.compressed.contains("error") && result.compressed.contains("timeout"),
            "anomaly entry with error/timeout must be preserved in output"
        );
        assert_eq!(result.anomalies_found, 1);
        assert!(result.items_shown > DEFAULT_MAX_ITEMS);
    }

    #[test]
    fn log_deduplication_collapses_repeated_patterns_and_keeps_anomalies() {
        let input = "INFO worker 101 started\nINFO worker 102 started\nWARN cache miss\nERROR request timed out\n";

        let result = crush_auto(input);

        assert!(result.compressed.contains("INFO worker 101 started [x2]"));
        assert!(result.compressed.contains("WARN cache miss"));
        assert!(result.compressed.contains("ERROR request timed out"));
        assert!(
            result
                .compressed
                .contains("[4 lines total, 3 patterns shown, 1 duplicate lines collapsed]")
        );
        assert_eq!(result.anomalies_found, 2);
    }

    #[test]
    fn auto_detection_routes_json_arrays_and_plain_text_to_the_right_compressor() {
        let array = crush_auto(r#"[{"kind":"result"},{"kind":"result"}]"#);
        let logs = crush_auto("INFO ready\nINFO ready");

        assert!(array.compressed.contains("schema ("));
        assert!(logs.compressed.starts_with("log samples:"));
    }

    #[test]
    fn schema_and_handler_return_machine_readable_stats() {
        let tool = CtxCrushTool;
        let schema = serde_json::to_value(tool.tool_def().input_schema).unwrap();
        assert_eq!(schema["required"], json!(["content"]));
        assert_eq!(schema["properties"]["mode"]["default"], "auto");

        let mut args = Map::new();
        args.insert("content".to_string(), json!("INFO ready\nINFO ready"));
        let output = tool.handle(&args, &ToolContext::default()).unwrap();
        let response: Value = serde_json::from_str(&output.text).unwrap();
        assert!(response["compressed"].is_string());
        assert!(response["stats"]["original_tokens"].is_u64());
        assert!(response["stats"]["ratio"].is_number());
        assert!(response["stats"]["delta_encoded"].is_boolean());
    }

    #[test]
    fn measure_compression_ratio_20_items() {
        let input: Vec<Value> = (1..=20)
            .map(|id| {
                if id == 4 {
                    json!({"id": id, "status": "error", "timestamp": "2026-08-13T10:03:00Z", "user": "dave", "action": "login", "error": "timeout"})
                } else {
                    json!({"id": id, "status": "ok", "timestamp": format!("2026-08-13T10:{:02}:00Z", id - 1), "user": format!("user{id}"), "action": "login"})
                }
            })
            .collect();
        let json_str = serde_json::to_string(&input).unwrap();
        let result = crush_auto(&json_str);
        let orig_tokens = json_str.len() / 4;
        let compressed_tokens = result.compressed.len() / 4;
        let ratio = 1.0 - (compressed_tokens as f64 / orig_tokens as f64);
        eprintln!("\n=== ctx_crush Performance ===");
        eprintln!(
            "  Input: {} chars ({} est. tokens)",
            json_str.len(),
            orig_tokens
        );
        eprintln!(
            "  Output: {} chars ({} est. tokens)",
            result.compressed.len(),
            compressed_tokens
        );
        eprintln!("  Compression: {:.1}%", ratio * 100.0);
        eprintln!(
            "  Items: {}/{} shown, {} anomalies",
            result.items_shown, result.items_total, result.anomalies_found
        );
        eprintln!("  Headroom SmartCrusher target: 60-95%");
        eprintln!("===========================\n");
        assert!(
            ratio > 0.60,
            "should achieve >60% compression on 20-item array (got {:.1}%)",
            ratio * 100.0
        );
    }

    #[test]
    fn homogeneous_100_items_achieves_seventy_percent() {
        let input: Vec<Value> = (1..=100)
            .map(|id| {
                json!({
                    "id": id,
                    "status": "ok",
                    "region": "eu-west-1",
                    "timestamp": format!("2026-08-13T{:02}:{:02}:00Z", id / 60, id % 60),
                    "latency_ms": 42,
                    "method": "POST",
                    "path": "/v1/chat/completions"
                })
            })
            .collect();
        let json_str = serde_json::to_string(&input).unwrap();
        let result = crush_auto(&json_str);
        let ratio = 1.0 - (result.compressed.len() as f64 / json_str.len() as f64);
        eprintln!(
            "\n=== 100-item homogeneous: {:.1}% compression ({} -> {} chars) ===\n",
            ratio * 100.0,
            json_str.len(),
            result.compressed.len()
        );
        assert!(
            ratio > 0.70,
            "homogeneous 100-item array should achieve >70% compression (got {:.1}%)",
            ratio * 100.0
        );
    }
}
