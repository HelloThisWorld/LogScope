//! Dataset field catalog: derived, rebuildable statistics about dynamic
//! attributes, and the trusted `CatalogView` the query language resolves
//! field identifiers through (ADR-0012).
//!
//! The builder streams `attributes_json` out of the immutable Parquet
//! segments and walks the canonical typed values in Rust — exactly the same
//! `AnyValue` semantics the importer wrote, no second JSON interpretation.

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;
use std::time::Duration;

use logscope_model::{attrs_from_canonical_json, AnyValue};
use logscope_query_lang::{
    builtin_field_names, suggest_fields, AttrFieldInfo, AttrType, CatalogView, FieldResolution,
};

use crate::cancel::{run_bounded, QueryCancelHandle};
use crate::engine::EngineConnection;
use crate::error::QueryError;
use crate::logs::files_expr;

/// Bump when the catalog computation semantics change.
pub const CATALOG_VERSION: i64 = 1;

/// Maximum nesting depth catalogued (deeper values stay reachable as their
/// parent object via existence tests).
const MAX_ATTR_DEPTH: usize = 6;
/// Bounded per-field distinct tracking; beyond this the count is estimated.
const DISTINCT_TRACK_CAP: usize = 1024;
/// Example values kept per field.
const MAX_EXAMPLES: usize = 3;
/// Maximum characters of one stored example value.
const MAX_EXAMPLE_CHARS: usize = 64;
/// Hard bound on catalogued fields per dataset (pathological inputs).
const MAX_FIELDS_PER_DATASET: usize = 2000;

/// One field's derived statistics for one dataset.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct FieldStat {
    pub display: String,
    pub path: Vec<String>,
    pub types: Vec<AttrType>,
    pub present_count: i64,
    pub distinct_est: Option<i64>,
    pub distinct_is_exact: bool,
    pub examples: Vec<String>,
    /// False when the name cannot be written in query language v1.
    pub queryable: bool,
}

struct FieldAcc {
    path: Vec<String>,
    types: BTreeSet<AttrType>,
    present: i64,
    distinct: BTreeSet<u64>,
    distinct_overflow: bool,
    examples: Vec<String>,
}

fn attr_type_of(v: &AnyValue) -> AttrType {
    match v {
        AnyValue::Empty => AttrType::Empty,
        AnyValue::Str(_) => AttrType::Str,
        AnyValue::Bool(_) => AttrType::Bool,
        AnyValue::Int(_) => AttrType::Int,
        AnyValue::Double(_) => AttrType::Double,
        AnyValue::Bytes(_) => AttrType::Bytes,
        AnyValue::Array(_) => AttrType::Array,
        AnyValue::Map(_) => AttrType::Map,
    }
}

/// Display-safe bounded example text (control characters replaced).
fn example_text(v: &AnyValue) -> String {
    let raw = v.display_string();
    let mut out: String = raw
        .chars()
        .take(MAX_EXAMPLE_CHARS)
        .map(|c| if c.is_control() { '\u{FFFD}' } else { c })
        .collect();
    if raw.chars().count() > MAX_EXAMPLE_CHARS {
        out.push('…');
    }
    out
}

/// A field name is queryable when every path segment can be written as a
/// bare word in language v1 and cannot collide with JSON-path syntax.
pub fn is_queryable_name(segments: &[String]) -> bool {
    segments.iter().all(|seg| {
        !seg.is_empty()
            && seg.chars().all(|c| {
                !c.is_whitespace()
                    && !c.is_control()
                    && !matches!(
                        c,
                        '(' | ')'
                            | '"'
                            | ':'
                            | '='
                            | '<'
                            | '>'
                            | '!'
                            | '*'
                            | '?'
                            | '\\'
                            | '/'
                            | '$'
                            | '['
                            | ']'
                            | ','
                    )
            })
    })
}

fn visit(
    acc: &mut BTreeMap<String, FieldAcc>,
    prefix_path: &mut Vec<String>,
    key: &str,
    value: &AnyValue,
    depth: usize,
) {
    prefix_path.push(key.to_string());
    let display = prefix_path.join(".");
    if acc.len() < MAX_FIELDS_PER_DATASET || acc.contains_key(&display) {
        let entry = acc.entry(display).or_insert_with(|| FieldAcc {
            path: prefix_path.clone(),
            types: BTreeSet::new(),
            present: 0,
            distinct: BTreeSet::new(),
            distinct_overflow: false,
            examples: Vec::new(),
        });
        entry.present += 1;
        entry.types.insert(attr_type_of(value));
        if !matches!(
            value,
            AnyValue::Map(_) | AnyValue::Array(_) | AnyValue::Empty
        ) {
            let text = example_text(value);
            let mut hasher = blake3::Hasher::new();
            hasher.update(text.as_bytes());
            let h = u64::from_le_bytes(hasher.finalize().as_bytes()[..8].try_into().expect("8"));
            if entry.distinct.len() < DISTINCT_TRACK_CAP {
                entry.distinct.insert(h);
            } else if !entry.distinct.contains(&h) {
                entry.distinct_overflow = true;
            }
            if entry.examples.len() < MAX_EXAMPLES && !entry.examples.contains(&text) {
                entry.examples.push(text);
            }
        }
    }
    if let AnyValue::Map(map) = value {
        if depth < MAX_ATTR_DEPTH {
            for (k, v) in map {
                visit(acc, prefix_path, k, v, depth + 1);
            }
        }
    }
    prefix_path.pop();
}

/// Computes the complete attribute catalog of the given published segments.
/// Cancellable and budget-bounded; the scan streams one row at a time.
pub fn compute_field_stats(
    engine: &EngineConnection,
    segment_files: &[PathBuf],
    cancel: &QueryCancelHandle,
    budget: Duration,
) -> Result<Vec<FieldStat>, QueryError> {
    if segment_files.is_empty() {
        return Ok(vec![]);
    }
    let sql = format!("SELECT attributes_json FROM {}", files_expr(segment_files));
    let mut acc: BTreeMap<String, FieldAcc> = BTreeMap::new();
    run_bounded(cancel, budget, || {
        let conn = engine.raw();
        let mut stmt = conn.prepare(&sql)?;
        let mut rows = stmt.query([])?;
        let mut path_buf: Vec<String> = Vec::with_capacity(MAX_ATTR_DEPTH);
        let mut n: u64 = 0;
        while let Some(row) = rows.next()? {
            n += 1;
            if n.is_multiple_of(8192) && cancel.was_cancelled() {
                return Err(QueryError::Cancelled);
            }
            let json: String = row.get(0)?;
            let attrs = attrs_from_canonical_json(&json).map_err(|e| {
                QueryError::InvalidParameter(format!("corrupt attributes_json: {e}"))
            })?;
            for (k, v) in &attrs {
                visit(&mut acc, &mut path_buf, k, v, 1);
            }
        }
        Ok(())
    })?;

    Ok(acc
        .into_iter()
        .map(|(display, a)| {
            let distinct_len = a.distinct.len() as i64;
            FieldStat {
                queryable: is_queryable_name(&a.path),
                display,
                path: a.path,
                types: a.types.into_iter().collect(),
                present_count: a.present,
                distinct_est: Some(distinct_len),
                distinct_is_exact: !a.distinct_overflow,
                examples: a.examples,
            }
        })
        .collect())
}

/// One dataset's stored catalog entry (loaded back from workspace metadata).
#[derive(Debug, Clone)]
pub struct StoredFieldStat {
    pub dataset_id: String,
    pub display: String,
    pub path: Vec<String>,
    pub types: Vec<AttrType>,
    pub present_count: i64,
    pub distinct_est: Option<i64>,
    pub distinct_is_exact: bool,
    pub examples: Vec<String>,
    pub queryable: bool,
}

#[derive(Debug, Clone)]
struct MergedField {
    path: Vec<String>,
    types: BTreeSet<AttrType>,
    per_dataset_types: BTreeMap<String, BTreeSet<AttrType>>,
    present_count: i64,
    distinct_est: i64,
    distinct_is_exact: bool,
    examples: Vec<String>,
    queryable: bool,
}

/// Field catalog for a concrete dataset selection: the only way the query
/// pipeline turns written field names into storage identifiers.
#[derive(Debug, Default, Clone)]
pub struct LoadedCatalog {
    pub dataset_ids: Vec<String>,
    fields: BTreeMap<String, MergedField>,
    /// display → conflicting path variants (flat key vs nested object).
    ambiguous: BTreeMap<String, Vec<String>>,
    /// True when every selected dataset has catalog rows.
    pub complete: bool,
}

impl LoadedCatalog {
    pub fn build(
        dataset_ids: Vec<String>,
        rows: Vec<StoredFieldStat>,
        datasets_with_catalog: &[String],
    ) -> Self {
        let complete = dataset_ids
            .iter()
            .all(|d| datasets_with_catalog.contains(d));
        let mut fields: BTreeMap<String, MergedField> = BTreeMap::new();
        let mut ambiguous: BTreeMap<String, Vec<String>> = BTreeMap::new();
        for row in rows {
            match fields.entry(row.display.clone()) {
                std::collections::btree_map::Entry::Vacant(e) => {
                    e.insert(MergedField {
                        path: row.path,
                        types: row.types.iter().copied().collect(),
                        per_dataset_types: BTreeMap::from([(
                            row.dataset_id,
                            row.types.into_iter().collect(),
                        )]),
                        present_count: row.present_count,
                        distinct_est: row.distinct_est.unwrap_or(0),
                        distinct_is_exact: row.distinct_is_exact,
                        examples: row.examples,
                        queryable: row.queryable,
                    });
                }
                std::collections::btree_map::Entry::Occupied(mut e) => {
                    let m = e.get_mut();
                    if m.path != row.path {
                        ambiguous
                            .entry(row.display.clone())
                            .or_insert_with(|| vec![format!("path {}", m.path.join(" › "))])
                            .push(format!("path {}", row.path.join(" › ")));
                        continue;
                    }
                    m.types.extend(row.types.iter().copied());
                    m.per_dataset_types
                        .entry(row.dataset_id)
                        .or_default()
                        .extend(row.types);
                    m.present_count += row.present_count;
                    m.distinct_est = m.distinct_est.max(row.distinct_est.unwrap_or(0));
                    m.distinct_is_exact &= row.distinct_is_exact;
                    for ex in row.examples {
                        if m.examples.len() < MAX_EXAMPLES && !m.examples.contains(&ex) {
                            m.examples.push(ex);
                        }
                    }
                    m.queryable &= row.queryable;
                }
            }
        }
        LoadedCatalog {
            dataset_ids,
            fields,
            ambiguous,
            complete,
        }
    }

    /// Suggestion entries: (display, types, present_count, distinct_est,
    /// distinct_is_exact, examples, queryable).
    #[allow(clippy::type_complexity)]
    pub fn field_entries(&self) -> Vec<(String, Vec<AttrType>, i64, i64, bool, Vec<String>, bool)> {
        self.fields
            .iter()
            .map(|(d, m)| {
                (
                    d.clone(),
                    m.types.iter().copied().collect(),
                    m.present_count,
                    m.distinct_est,
                    m.distinct_is_exact,
                    m.examples.clone(),
                    m.queryable,
                )
            })
            .collect()
    }

    /// Per-dataset type map for one field (conflict display in the UI).
    pub fn per_dataset_types(&self, display: &str) -> Option<Vec<(String, Vec<AttrType>)>> {
        self.fields.get(display).map(|m| {
            m.per_dataset_types
                .iter()
                .map(|(d, t)| (d.clone(), t.iter().copied().collect()))
                .collect()
        })
    }
}

impl CatalogView for LoadedCatalog {
    fn resolve_attr(&self, written: &str) -> FieldResolution {
        if let Some(candidates) = self.ambiguous.get(written) {
            return FieldResolution::Ambiguous {
                candidates: candidates.clone(),
            };
        }
        match self.fields.get(written) {
            Some(m) if m.queryable => FieldResolution::Attr(AttrFieldInfo {
                path: m.path.clone(),
                display: written.to_string(),
                types: m.types.iter().copied().collect(),
            }),
            Some(_) => FieldResolution::Unknown {
                suggestions: vec![format!(
                    "{written} exists but its name cannot be written in query language v1"
                )],
            },
            None => FieldResolution::Unknown {
                suggestions: suggest_fields(
                    written,
                    self.fields
                        .keys()
                        .map(|s| s.as_str())
                        .chain(builtin_field_names().iter().copied()),
                    3,
                ),
            },
        }
    }

    fn attr_exists(&self, written: &str) -> bool {
        self.fields.contains_key(written)
    }
}
