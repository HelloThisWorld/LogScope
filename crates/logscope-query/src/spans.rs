//! Trace reconstruction as a causal graph.
//!
//! Parent-child edges form the primary tree; links are first-class edges.
//! Orphans, missing roots, duplicates, sampled/unsampled, incomplete,
//! out-of-order, and clock-skewed spans stay visible via annotations.
//! No synthetic spans are ever created: unresolved parents are reported as
//! references, not nodes.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::engine::EngineConnection;
use crate::error::QueryError;

/// Bound on spans loaded for one trace reconstruction.
pub const MAX_SPANS_PER_TRACE: usize = 50_000;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SpanNode {
    pub record_id: String,
    pub span_id: String,
    pub parent_span_id: Option<String>,
    pub name: String,
    pub kind: String,
    pub start_time: i64,
    pub end_time: Option<i64>,
    pub duration_nanos: Option<i64>,
    pub status_code: String,
    /// W3C sampled bit from the flags word, when flags were recorded.
    pub sampled: Option<bool>,
    /// Graph annotations: orphan_parent, duplicate_span_id, missing_end,
    /// clock_skew_before_parent, out_of_order_sibling.
    pub annotations: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LinkEdge {
    /// Span (span_id) holding the link.
    pub from_span_id: String,
    pub to_trace_id: String,
    pub to_span_id: Option<String>,
    /// True when the linked span exists inside this reconstruction.
    pub resolved_in_trace: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UnresolvedParent {
    pub parent_span_id: String,
    pub referenced_by: Vec<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TraceIntegrity {
    pub span_count: usize,
    pub root_count: usize,
    pub missing_root: bool,
    pub orphan_count: usize,
    pub duplicate_span_ids: Vec<String>,
    pub incomplete_count: usize,
    pub clock_skew_count: usize,
    pub truncated: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TraceGraph {
    pub trace_id: String,
    pub nodes: Vec<SpanNode>,
    /// (child span_id, parent span_id) for parents present in the trace.
    pub parent_edges: Vec<(String, String)>,
    pub link_edges: Vec<LinkEdge>,
    /// span_ids with no parent reference (natural roots).
    pub roots: Vec<String>,
    pub unresolved_parents: Vec<UnresolvedParent>,
    pub integrity: TraceIntegrity,
}

#[derive(Debug, serde::Deserialize)]
struct StoredLink {
    trace_id: String,
    #[serde(default)]
    span_id: Option<String>,
}

/// Loads and reconstructs one trace from the given span segment files.
pub fn reconstruct_trace(
    engine: &EngineConnection,
    segment_files: &[PathBuf],
    trace_id: &str,
) -> Result<TraceGraph, QueryError> {
    if trace_id.len() != 32 || !trace_id.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(QueryError::InvalidParameter("invalid trace_id".into()));
    }
    let list = segment_files
        .iter()
        .map(|p| format!("'{}'", p.to_string_lossy().replace('\'', "''")))
        .collect::<Vec<_>>()
        .join(", ");
    let sql = format!(
        "SELECT record_id, span_id, parent_span_id, name, kind, start_time, end_time,
                duration_nanos, status_code, flags, links_json
         FROM read_parquet([{list}], union_by_name = true)
         WHERE trace_id = ?
         ORDER BY start_time, span_id
         LIMIT {}",
        MAX_SPANS_PER_TRACE + 1
    );

    struct Raw {
        node: SpanNode,
        links_json: String,
    }

    let conn = engine.raw();
    let mut stmt = conn.prepare(&sql)?;
    let mapped = stmt.query_map([trace_id], |r| {
        let flags: Option<u32> = r.get(9)?;
        Ok(Raw {
            node: SpanNode {
                record_id: r.get(0)?,
                span_id: r.get(1)?,
                parent_span_id: r.get(2)?,
                name: r.get(3)?,
                kind: r.get(4)?,
                start_time: r.get(5)?,
                end_time: r.get(6)?,
                duration_nanos: r.get(7)?,
                status_code: r.get(8)?,
                sampled: flags.map(|f| f & 1 == 1),
                annotations: vec![],
            },
            links_json: r.get(10)?,
        })
    })?;
    let mut raws: Vec<Raw> = Vec::new();
    for r in mapped {
        raws.push(r?);
    }
    let truncated = raws.len() > MAX_SPANS_PER_TRACE;
    raws.truncate(MAX_SPANS_PER_TRACE);

    let present: HashSet<String> = raws.iter().map(|r| r.node.span_id.clone()).collect();
    let mut span_id_counts: HashMap<String, usize> = HashMap::new();
    for r in &raws {
        *span_id_counts.entry(r.node.span_id.clone()).or_default() += 1;
    }
    let duplicate_span_ids: Vec<String> = {
        let mut v: Vec<String> = span_id_counts
            .iter()
            .filter(|(_, n)| **n > 1)
            .map(|(id, _)| id.clone())
            .collect();
        v.sort();
        v
    };
    let start_by_span: HashMap<String, i64> = raws
        .iter()
        .map(|r| (r.node.span_id.clone(), r.node.start_time))
        .collect();

    let mut nodes = Vec::with_capacity(raws.len());
    let mut parent_edges = Vec::new();
    let mut link_edges = Vec::new();
    let mut roots = Vec::new();
    let mut unresolved: HashMap<String, Vec<String>> = HashMap::new();
    let mut orphan_count = 0usize;
    let mut incomplete_count = 0usize;
    let mut clock_skew_count = 0usize;

    for raw in raws {
        let mut node = raw.node;
        if duplicate_span_ids.binary_search(&node.span_id).is_ok() {
            node.annotations.push("duplicate_span_id".into());
        }
        if node.end_time.is_none() {
            node.annotations.push("missing_end".into());
            incomplete_count += 1;
        }
        match &node.parent_span_id {
            None => roots.push(node.span_id.clone()),
            Some(parent) => {
                if present.contains(parent) {
                    parent_edges.push((node.span_id.clone(), parent.clone()));
                    if let Some(parent_start) = start_by_span.get(parent) {
                        if node.start_time < *parent_start {
                            node.annotations.push("clock_skew_before_parent".into());
                            clock_skew_count += 1;
                        }
                    }
                } else {
                    node.annotations.push("orphan_parent".into());
                    orphan_count += 1;
                    unresolved
                        .entry(parent.clone())
                        .or_default()
                        .push(node.span_id.clone());
                }
            }
        }
        // Links are first-class edges, never merged into the tree.
        let links: Vec<StoredLink> = serde_json::from_str(&raw.links_json).unwrap_or_default();
        for l in links {
            let resolved =
                l.trace_id == trace_id && l.span_id.as_ref().is_some_and(|s| present.contains(s));
            link_edges.push(LinkEdge {
                from_span_id: node.span_id.clone(),
                to_trace_id: l.trace_id,
                to_span_id: l.span_id,
                resolved_in_trace: resolved,
            });
        }
        nodes.push(node);
    }

    let mut unresolved_parents: Vec<UnresolvedParent> = unresolved
        .into_iter()
        .map(|(parent_span_id, mut referenced_by)| {
            referenced_by.sort();
            UnresolvedParent {
                parent_span_id,
                referenced_by,
            }
        })
        .collect();
    unresolved_parents.sort_by(|a, b| a.parent_span_id.cmp(&b.parent_span_id));

    let integrity = TraceIntegrity {
        span_count: nodes.len(),
        root_count: roots.len(),
        missing_root: roots.is_empty() && !nodes.is_empty(),
        orphan_count,
        duplicate_span_ids,
        incomplete_count,
        clock_skew_count,
        truncated,
    };

    Ok(TraceGraph {
        trace_id: trace_id.to_string(),
        nodes,
        parent_edges,
        link_edges,
        roots,
        unresolved_parents,
        integrity,
    })
}
