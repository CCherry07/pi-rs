use std::collections::{HashMap, HashSet};
use std::sync::LazyLock;

use icu_segmenter::{WordSegmenter, options::WordBreakInvariantOptions};
use pi_core::ToolSpec;
use pi_plugin::ToolNamespace;
use regex::Regex;
use serde_json::Value;
use unicode_normalization::UnicodeNormalization;

const STOP_WORDS: &[&str] = &[
    "a", "an", "and", "are", "as", "at", "be", "by", "for", "from", "in", "is", "it", "of", "on",
    "or", "that", "the", "this", "to", "with",
];

/// Unicode words with Pi's identifier boundaries, English stop words and singular forms.
pub fn tokenize(text: &str) -> Vec<String> {
    static CAMEL: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"([a-z0-9])([A-Z])").expect("literal regex"));
    static ACRONYM: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"([A-Z]+)([A-Z][a-z])").expect("literal regex"));
    static NON_WORD: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"[^\p{L}\p{M}\p{N}]+").expect("literal regex"));

    let normalized = text.nfc().collect::<String>();
    let normalized = CAMEL.replace_all(&normalized, "$1 $2");
    let normalized = ACRONYM.replace_all(&normalized, "$1 $2").to_lowercase();
    let normalized = NON_WORD.replace_all(&normalized, " ");
    let words = if normalized.is_ascii() {
        normalized.split(' ').collect::<Vec<_>>()
    } else {
        let segmenter = WordSegmenter::new_dictionary(WordBreakInvariantOptions::default());
        let mut start = 0;
        let mut words = Vec::new();
        for (end, kind) in segmenter.segment_str(&normalized).iter_with_word_type() {
            if kind.is_word_like() {
                words.push(&normalized[start..end]);
            }
            start = end;
        }
        words
    };
    words
        .into_iter()
        .filter(|word| !word.is_empty() && !STOP_WORDS.contains(word))
        .map(stem)
        .collect()
}

fn stem(term: &str) -> String {
    if !term
        .bytes()
        .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit())
    {
        return term.into();
    }
    if term.len() > 4 && term.ends_with("ies") {
        return format!("{}y", &term[..term.len() - 3]);
    }
    if term.len() > 4
        && ["ches", "shes", "sses", "xes", "zes"]
            .iter()
            .any(|suffix| term.ends_with(suffix))
    {
        return term[..term.len() - 2].into();
    }
    if term.len() > 3 && term.ends_with('s') && !term.ends_with("ss") {
        return term[..term.len() - 1].into();
    }
    term.into()
}

/// Fields independently weighted by the ranker.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolSearchDocument {
    pub name: String,
    /// Description, schema descriptions and property names.
    pub text: String,
    /// Namespace name, description and usage instructions.
    pub namespace_text: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ToolSearchMatch {
    pub name: String,
    pub score: f64,
}

/// Builds a search document, following local schema references once per target.
pub fn create_tool_search_document(
    tool: &ToolSpec,
    namespace: Option<&ToolNamespace>,
) -> ToolSearchDocument {
    let mut parts = vec![tool.description.clone()];
    let mut seen = HashSet::from([&tool.parameters as *const Value]);
    schema_text(&tool.parameters, &tool.parameters, &mut parts, &mut seen);
    ToolSearchDocument {
        name: tool.name.clone(),
        text: parts
            .into_iter()
            .filter(|part| !part.trim().is_empty())
            .collect::<Vec<_>>()
            .join(" "),
        namespace_text: namespace.map(|namespace| {
            format!(
                "{} {} {}",
                namespace.name,
                namespace.description.as_deref().unwrap_or(""),
                namespace.instructions.as_deref().unwrap_or("")
            )
        }),
    }
}

fn schema_text(
    schema: &Value,
    root: &Value,
    parts: &mut Vec<String>,
    seen: &mut HashSet<*const Value>,
) {
    let Some(schema) = schema.as_object() else {
        return;
    };
    if let Some(reference) = schema.get("$ref").and_then(Value::as_str)
        && let Some(target) = resolve_schema_ref(reference, root)
        && target.is_object()
        && seen.insert(target as *const Value)
    {
        schema_text(target, root, parts, seen);
    }
    if let Some(description) = schema.get("description").and_then(Value::as_str) {
        parts.push(description.into());
    }
    if let Some(properties) = schema.get("properties").and_then(Value::as_object) {
        for (name, property) in properties {
            parts.push(name.clone());
            schema_text(property, root, parts, seen);
        }
    }
    if let Some(items) = schema.get("items") {
        schema_text(items, root, parts, seen);
    }
    for key in ["anyOf", "oneOf", "allOf"] {
        if let Some(variants) = schema.get(key).and_then(Value::as_array) {
            for variant in variants {
                schema_text(variant, root, parts, seen);
            }
        }
    }
}

fn resolve_schema_ref<'a>(reference: &str, root: &'a Value) -> Option<&'a Value> {
    if reference == "#" {
        return Some(root);
    }
    let path = reference.strip_prefix("#/")?;
    let mut current = root;
    // Pi skips empty pointer components. Decode each component before JSON Pointer unescaping.
    for part in path.split('/').filter(|part| !part.is_empty()) {
        let key = decode_uri_component(part)?
            .replace("~1", "/")
            .replace("~0", "~");
        current = current.as_object()?.get(&key)?;
    }
    Some(current)
}

fn decode_uri_component(value: &str) -> Option<String> {
    let mut bytes = value.bytes();
    let mut decoded = Vec::with_capacity(value.len());
    while let Some(byte) = bytes.next() {
        if byte == b'%' {
            let high = char::from(bytes.next()?).to_digit(16)?;
            let low = char::from(bytes.next()?).to_digit(16)?;
            decoded.push((high * 16 + low) as u8);
        } else {
            decoded.push(byte);
        }
    }
    String::from_utf8(decoded).ok()
}

struct IndexedDocument<'a> {
    name: &'a str,
    name_terms: HashSet<String>,
    local_name_terms: HashSet<String>,
    text_counts: HashMap<String, usize>,
    text_length: usize,
    namespace_terms: HashSet<String>,
}

impl<'a> IndexedDocument<'a> {
    fn new(document: &'a ToolSearchDocument) -> Self {
        let mut text_counts = HashMap::new();
        let text_terms = tokenize(&document.text);
        let text_length = text_terms.len();
        for term in text_terms {
            *text_counts.entry(term).or_insert(0) += 1;
        }
        Self {
            name: &document.name,
            name_terms: tokenize(&document.name).into_iter().collect(),
            local_name_terms: tokenize(document.name.rsplit("__").next().unwrap_or(""))
                .into_iter()
                .collect(),
            text_counts,
            text_length,
            namespace_terms: tokenize(document.namespace_text.as_deref().unwrap_or(""))
                .into_iter()
                .collect(),
        }
    }
}

/// Pi's BM25 ranking: exact names, then term coverage, then weighted relevance.
#[derive(Debug, Clone, Copy)]
pub struct Bm25Ranker {
    k1: f64,
    b: f64,
}

impl Default for Bm25Ranker {
    fn default() -> Self {
        Self { k1: 1.2, b: 0.75 }
    }
}

impl Bm25Ranker {
    /// Ranks nonzero matches, retaining document order when scores are equal.
    pub fn rank(
        &self,
        query: &str,
        documents: &[ToolSearchDocument],
        limit: usize,
    ) -> Vec<ToolSearchMatch> {
        let mut seen = HashSet::new();
        let query_terms = tokenize(query)
            .into_iter()
            .filter(|term| seen.insert(term.clone()))
            .collect::<Vec<_>>();
        if query_terms.is_empty() || documents.is_empty() || limit == 0 {
            return Vec::new();
        }
        let indexed = documents
            .iter()
            .map(IndexedDocument::new)
            .collect::<Vec<_>>();
        let average_length = indexed
            .iter()
            .map(|document| document.text_length)
            .sum::<usize>() as f64
            / indexed.len() as f64;
        let average_length = if average_length == 0.0 {
            1.0
        } else {
            average_length
        };
        let terms = query_terms
            .iter()
            .map(|term| {
                let frequency = indexed
                    .iter()
                    .filter(|document| {
                        document.name_terms.contains(term)
                            || document.text_counts.contains_key(term)
                            || document.namespace_terms.contains(term)
                    })
                    .count() as f64;
                (
                    term,
                    (1.0 + (indexed.len() as f64 - frequency + 0.5) / (frequency + 0.5)).ln(),
                )
            })
            .collect::<Vec<_>>();
        let full_name_bonus = terms.iter().map(|(_, idf)| idf).sum::<f64>();
        let exact_query = normalize_name(query);
        let mut matches = Vec::new();
        for document in indexed {
            let norm =
                self.k1 * (1.0 - self.b + self.b * document.text_length as f64 / average_length);
            let mut score = 0.0;
            let mut covered = 0;
            for (term, idf) in &terms {
                let count = document.text_counts.get(*term).copied().unwrap_or(0) as f64;
                let weight = if document.name_terms.contains(*term) {
                    3.0
                } else {
                    0.0
                } + if document.namespace_terms.contains(*term) {
                    0.5
                } else {
                    0.0
                } + count * (self.k1 + 1.0) / (count + norm);
                if weight > 0.0 {
                    covered += 1;
                }
                score += idf * weight;
            }
            if score == 0.0 {
                continue;
            }
            if !document.local_name_terms.is_empty()
                && document
                    .local_name_terms
                    .iter()
                    .all(|term| seen.contains(term))
            {
                score += full_name_bonus;
            }
            let name = normalize_name(document.name);
            let exact = name == exact_query || name.ends_with(&format!("__{exact_query}"));
            matches.push((
                ToolSearchMatch {
                    name: document.name.into(),
                    score,
                },
                covered,
                exact,
            ));
        }
        matches.sort_by(
            |(left, left_covered, left_exact), (right, right_covered, right_exact)| {
                right_exact
                    .cmp(left_exact)
                    .then_with(|| right_covered.cmp(left_covered))
                    .then_with(|| right.score.total_cmp(&left.score))
            },
        );
        matches
            .into_iter()
            .take(limit)
            .map(|(found, _, _)| found)
            .collect()
    }
}

fn normalize_name(name: &str) -> String {
    name.trim()
        .to_lowercase()
        .encode_utf16()
        .map(|unit| match u8::try_from(unit) {
            Ok(byte)
                if byte.is_ascii_lowercase()
                    || byte.is_ascii_digit()
                    || byte == b'_'
                    || byte == b'$' =>
            {
                char::from(byte)
            }
            _ => '_',
        })
        .collect()
}
