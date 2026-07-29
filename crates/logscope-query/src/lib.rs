//! Query service for LogScope.
//!
//! DuckDB over immutable Parquet segments: structured filtering, bounded
//! first-page results, cancellation, full-text search (via the SQLite FTS5
//! index), representative metric rollups, and span graph reconstruction.
//! The same service backs the UI, CLI, Agent API, dashboards, and reports.

pub mod cancel;
pub mod catalog;
pub mod compile;
pub mod engine;
pub mod error;
pub mod explore;
pub mod fts_maintenance;
pub mod logs;
pub mod metrics;
pub mod spans;

pub use cancel::{run_bounded, QueryCancelHandle};
pub use catalog::{
    compute_field_stats, FieldStat, LoadedCatalog, StoredFieldStat, CATALOG_VERSION,
};
pub use compile::{
    compile_filter, install_temp_tables, text_tokens, tokens_to_fts_expr, tokens_to_regex,
    CompiledFilter, FtsContext, TextExecMode, MAX_FTS_CANDIDATES,
};
pub use engine::EngineConnection;
pub use error::{ErrorEnvelope, QueryError};
pub use explore::{
    encode_cursor, fetch_record, query_counts, query_facets, query_field_summary, query_histogram,
    query_page, query_source_context, resolve_window, FacetResult, FacetValue, FieldSummary,
    FieldTarget, FilterCounts, Histogram, HistogramBin, PageRequest, QueryPage, ResolvedWindow,
    SourceContext, TimeStrategy, MAX_CONTEXT_NEIGHBORS, MAX_FACET_FIELDS, MAX_FACET_TOP_K,
    MAX_HISTOGRAM_BINS,
};
pub use fts_maintenance::index_segment_into_fts;
pub use logs::{query_log_page, LogPage, LogQueryRequest, LogRow, MAX_PAGE_SIZE};
pub use metrics::{rollup_gauge_or_delta_sum, RollupRow};
pub use spans::{reconstruct_trace, SpanNode, TraceGraph, TraceIntegrity};
