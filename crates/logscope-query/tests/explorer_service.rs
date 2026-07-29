//! Explorer service integration proofs: one compiled filter drives page,
//! counts, histogram, and facets; FTS and fallback scans are equivalent;
//! keyset pagination is exact across equal and missing timestamps;
//! cancellation leaves the engine usable.

use std::collections::BTreeSet;
use std::path::PathBuf;
use std::sync::OnceLock;
use std::time::Duration;

use logscope_model::{
    AnyValue, AttrMap, IngestProvenance, LogRecord, PhysicalOrigin, RecordLocator, SourceProtocol,
    UnixNanos,
};
use logscope_query::*;
use logscope_query_lang::{analyze, LangLimits, ResolvedExpr};
use logscope_store::{FtsIndex, LogSegmentWriter};

const BASE_TS: i64 = 1_770_000_000_000_000_000;

struct Fixture {
    files_a: Vec<PathBuf>,
    files_b: Vec<PathBuf>,
    files_big: Vec<PathBuf>,
    fts_path: PathBuf,
    ids_ab: Vec<String>,
    catalog_ab: LoadedCatalog,
    catalog_b: LoadedCatalog,
    /// record ids in ds-a with no event timestamp.
    untimestamped_a: usize,
    total_ab: usize,
}

#[allow(clippy::too_many_arguments)]
fn rec(
    dataset: &str,
    file_id: &str,
    n: u64,
    ts: Option<i64>,
    sev: Option<(&str, Option<i32>)>,
    msg: &str,
    attrs: AttrMap,
    trace: bool,
) -> LogRecord {
    LogRecord {
        record_id: String::new(),
        event_time: ts.map(UnixNanos),
        observed_time: UnixNanos(BASE_TS),
        original_timestamp_text: None,
        timezone_assumption: None,
        severity_text: sev.map(|(t, _)| t.to_string()),
        severity_number: sev.and_then(|(_, n)| n),
        body: Some(AnyValue::str(msg)),
        display_message: msg.to_string(),
        event_name: None,
        trace_id: trace.then(|| {
            logscope_model::TraceId::from_hex("0123456789abcdef0123456789abcdef").unwrap()
        }),
        span_id: None,
        trace_flags: None,
        resource_id: "res-unknown".into(),
        scope_id: "scope-unknown".into(),
        operation: None,
        outcome: None,
        event_type: None,
        request_id: None,
        transaction_id: None,
        message_id: None,
        entity_id: None,
        attributes: attrs,
        dropped_attributes_count: 0,
        provenance: IngestProvenance {
            dataset_id: dataset.to_string(),
            logical_source_id: format!("src-{dataset}"),
            origin: PhysicalOrigin::File {
                file_id: file_id.to_string(),
                archive_entry: None,
            },
            locator: RecordLocator {
                record_number: Some(n),
                line_start: Some(n),
                line_end: Some(n),
                byte_start: Some(n * 100),
                byte_end: Some(n * 100 + 80),
                json_pointer: None,
                otlp: None,
            },
            parser_id: "test".into(),
            parser_version: "1".into(),
            profile_id: None,
            profile_version: None,
            normalizer_version: "1".into(),
            protocol: SourceProtocol::FileImport,
            content_type: None,
            ingest_time: UnixNanos(BASE_TS),
            raw_hash: format!("{:064x}", n),
            original_timestamp_precision: None,
            flags: vec![],
        },
    }
    .seal()
}

fn attrs(pairs: &[(&str, AnyValue)]) -> AttrMap {
    pairs
        .iter()
        .map(|(k, v)| (k.to_string(), v.clone()))
        .collect()
}

fn write_segment(dir: &std::path::Path, name: &str, records: &[LogRecord]) -> PathBuf {
    let path = dir.join(name);
    let mut w = LogSegmentWriter::create(&path).expect("segment writer");
    w.write_batch(records).expect("write batch");
    w.finish().expect("finish");
    path
}

fn fixture() -> &'static Fixture {
    static FIX: OnceLock<Fixture> = OnceLock::new();
    FIX.get_or_init(|| {
        let dir = Box::leak(Box::new(tempfile::tempdir().expect("tempdir")));
        let root = dir.path().to_path_buf();

        // ---- dataset ds-a: 24 timestamped + 3 untimestamped -------------
        let mut recs_a = Vec::new();
        for i in 1..=24u64 {
            // Groups of four share one timestamp (tiebreak coverage).
            let ts = BASE_TS + ((i / 4) as i64) * 60_000_000_000;
            let (sev_txt, sev_num) = match i % 4 {
                0 => ("ERROR", Some(17)),
                1 => ("info", Some(9)),
                2 => ("Warn", Some(13)),
                _ => ("debug", Some(5)),
            };
            let msg = match i % 5 {
                0 => "connection timed out to upstream cascade".to_string(),
                1 => "cache refreshed ok".to_string(),
                2 => "connection timed out to upstream".to_string(),
                3 => "Café müde Grüße von web-01".to_string(),
                _ => format!("request {i} completed"),
            };
            let service = if i % 2 == 0 { "checkout" } else { "payment" };
            let mut a = attrs(&[
                ("service.name", AnyValue::str(service)),
                ("retry.count", AnyValue::int((i % 3) as i64)),
                ("active", AnyValue::bool(i % 2 == 0)),
                (
                    "http",
                    AnyValue::Map(
                        [(
                            "status".to_string(),
                            AnyValue::int(if i % 4 == 0 { 503 } else { 200 }),
                        )]
                        .into_iter()
                        .collect(),
                    ),
                ),
                ("mixed", AnyValue::str("x")),
            ]);
            if i == 7 {
                a.insert("empty_attr".into(), AnyValue::Empty);
            }
            recs_a.push(rec(
                "ds-a",
                "file-a",
                i,
                Some(ts),
                Some((sev_txt, sev_num)),
                &msg,
                a,
                i <= 2,
            ));
        }
        // Untimestamped records (kept queryable; excluded from bounded
        // windows with a count). One has severity text without a number.
        for i in 25..=27u64 {
            recs_a.push(rec(
                "ds-a",
                "file-a",
                i,
                None,
                if i == 25 { Some(("ERROR", None)) } else { None },
                "no timestamp record",
                attrs(&[("service.name", AnyValue::str("checkout"))]),
                false,
            ));
        }
        let seg_a = write_segment(&root, "logs-a.parquet", &recs_a);

        // ---- dataset ds-b: the roadmap-example corpus --------------------
        let mk_b = |n: u64, sev: (&str, Option<i32>), msg: &str, retry: Option<i64>| {
            let mut a = attrs(&[
                ("service.name", AnyValue::str("workflow")),
                ("mixed", AnyValue::int(5)),
            ]);
            if let Some(r) = retry {
                a.insert("retry.count".into(), AnyValue::int(r));
            }
            rec(
                "ds-b",
                "file-b",
                n,
                Some(BASE_TS + n as i64 * 1_000_000_000),
                Some(sev),
                msg,
                a,
                false,
            )
        };
        let recs_b = vec![
            mk_b(
                1,
                ("ERROR", Some(17)),
                "request timed out while polling",
                Some(1),
            ),
            mk_b(
                2,
                ("WARN", Some(13)),
                "request timed out while polling",
                Some(0),
            ),
            mk_b(
                3,
                ("ERROR", Some(18)),
                "request timed out while polling",
                None,
            ),
            mk_b(
                4,
                ("INFO", Some(9)),
                "request timed out while polling",
                Some(2),
            ),
            mk_b(5, ("ERROR", Some(17)), "polling fine", Some(3)),
        ];
        let seg_b = write_segment(&root, "logs-b.parquet", &recs_b);

        // ---- big dataset for cancellation checkpoints --------------------
        let mut recs_big = Vec::new();
        for i in 1..=20_000u64 {
            recs_big.push(rec(
                "ds-big",
                "file-big",
                i,
                Some(BASE_TS + i as i64),
                Some(("INFO", Some(9))),
                "bulk record",
                attrs(&[("k", AnyValue::int(i as i64))]),
                false,
            ));
        }
        let seg_big = write_segment(&root, "logs-big.parquet", &recs_big);

        // ---- FTS v2 index -------------------------------------------------
        let engine = EngineConnection::open_in_memory().expect("engine");
        let fts_path = root.join("fts_logs.db");
        let mut fts = FtsIndex::open(&fts_path).expect("fts open");
        assert_eq!(fts.version().expect("version"), 2, "new index is v2");
        index_segment_into_fts(&engine, &mut fts, "ds-a", "seg-a", &seg_a).expect("index a");
        index_segment_into_fts(&engine, &mut fts, "ds-b", "seg-b", &seg_b).expect("index b");

        // ---- field catalogs ------------------------------------------------
        let cancel = QueryCancelHandle::new(engine.interrupt_handle());
        let stats_a = compute_field_stats(
            &engine,
            std::slice::from_ref(&seg_a),
            &cancel,
            Duration::from_secs(30),
        )
        .expect("stats a");
        let stats_b = compute_field_stats(
            &engine,
            std::slice::from_ref(&seg_b),
            &cancel,
            Duration::from_secs(30),
        )
        .expect("stats b");
        let to_stored = |ds: &str, stats: &[FieldStat]| -> Vec<StoredFieldStat> {
            stats
                .iter()
                .map(|s| StoredFieldStat {
                    dataset_id: ds.to_string(),
                    display: s.display.clone(),
                    path: s.path.clone(),
                    types: s.types.clone(),
                    present_count: s.present_count,
                    distinct_est: s.distinct_est,
                    distinct_is_exact: s.distinct_is_exact,
                    examples: s.examples.clone(),
                    queryable: s.queryable,
                })
                .collect()
        };
        let ids_ab = vec!["ds-a".to_string(), "ds-b".to_string()];
        let mut rows = to_stored("ds-a", &stats_a);
        rows.extend(to_stored("ds-b", &stats_b));
        let catalog_ab = LoadedCatalog::build(ids_ab.clone(), rows, &ids_ab);
        let catalog_b = LoadedCatalog::build(
            vec!["ds-b".to_string()],
            to_stored("ds-b", &stats_b),
            &["ds-b".to_string()],
        );

        Fixture {
            files_a: vec![seg_a],
            files_b: vec![seg_b],
            files_big: vec![seg_big],
            fts_path,
            ids_ab,
            catalog_ab,
            catalog_b,
            untimestamped_a: 3,
            total_ab: 27 + 5,
        }
    })
}

fn engine() -> EngineConnection {
    EngineConnection::open_in_memory().expect("engine")
}

fn all_files(fx: &Fixture) -> Vec<PathBuf> {
    let mut f = fx.files_a.clone();
    f.extend(fx.files_b.clone());
    f
}

fn resolve(fx: &Fixture, text: &str) -> Option<ResolvedExpr> {
    let a = analyze(text, &fx.catalog_ab, &LangLimits::default());
    a.resolved
        .unwrap_or_else(|| panic!("query must resolve: {text} → {:?}", a.diagnostics))
        .expr
}

fn compiled(fx: &Fixture, text: &str, use_fts: bool) -> CompiledFilter {
    let expr = resolve(fx, text);
    let fts = use_fts.then(|| FtsIndex::open(&fx.fts_path).expect("fts"));
    let ctx = FtsContext {
        index: fts.as_ref(),
        dataset_ids: &fx.ids_ab,
    };
    compile_filter(expr.as_ref(), &ctx).expect("compile")
}

fn ids_for(fx: &Fixture, text: &str, use_fts: bool) -> BTreeSet<String> {
    let eng = engine();
    let cancel = QueryCancelHandle::new(eng.interrupt_handle());
    let filter = compiled(fx, text, use_fts);
    let window = resolve_window(&TimeStrategy::All, None);
    let page = query_page(
        &eng,
        &all_files(fx),
        &filter,
        &window,
        &PageRequest {
            cursor: None,
            backward: false,
            limit: 1000,
        },
        &cancel,
        None,
    )
    .expect("page");
    assert!(!page.has_more, "test corpus fits one page");
    page.rows.into_iter().map(|r| r.record_id).collect()
}

#[test]
fn shared_filter_page_counts_histogram_facets_agree() {
    let fx = fixture();
    let eng = engine();
    let cancel = QueryCancelHandle::new(eng.interrupt_handle());
    let text = r#""timed out""#;
    let filter = compiled(fx, text, true);
    let window = resolve_window(&TimeStrategy::All, None);
    let files = all_files(fx);

    let page = query_page(
        &eng,
        &files,
        &filter,
        &window,
        &PageRequest {
            cursor: None,
            backward: false,
            limit: 1000,
        },
        &cancel,
        None,
    )
    .expect("page");
    let counts = query_counts(&eng, &files, &filter, &window, &cancel, None).expect("counts");
    let histogram =
        query_histogram(&eng, &files, &filter, &window, 100, &cancel, None).expect("histogram");
    let facets = query_facets(
        &eng,
        &files,
        &filter,
        &window,
        &[(
            "severity".to_string(),
            FieldTarget::Canonical {
                field: logscope_query_lang::CanonicalField::Severity,
            },
        )],
        50,
        &cancel,
        None,
    )
    .expect("facets");

    assert!(!page.rows.is_empty());
    assert_eq!(page.rows.len() as i64, counts.matching);
    // Every matching record here is timestamped.
    assert_eq!(histogram.total_in_range, counts.matching);
    let facet_total: i64 =
        facets[0].values.iter().map(|v| v.count).sum::<i64>() + facets[0].missing_count;
    assert_eq!(facet_total, counts.matching);
    // Ordering is the documented total order.
    let times: Vec<Option<i64>> = page.rows.iter().map(|r| r.event_time).collect();
    let mut sorted = times.clone();
    sorted.sort_by(|a, b| match (a, b) {
        (Some(x), Some(y)) => y.cmp(x),
        (Some(_), None) => std::cmp::Ordering::Less,
        (None, Some(_)) => std::cmp::Ordering::Greater,
        (None, None) => std::cmp::Ordering::Equal,
    });
    assert_eq!(times, sorted);
}

#[test]
fn fts_and_fallback_scan_are_equivalent() {
    let fx = fixture();
    for text in [
        "cascade",
        r#""timed out""#,
        "müde",
        "web",
        "01",
        r#"NOT "timed out""#,
        r#""timed out" OR cascade"#,
        r#"café AND severity:(ERROR OR WARN OR INFO OR DEBUG)"#,
        "\"connection timed out to upstream\"",
        "grüße",
    ] {
        let with_fts = ids_for(fx, text, true);
        let fallback = ids_for(fx, text, false);
        assert_eq!(
            with_fts, fallback,
            "indexed and fallback execution disagree for {text:?}"
        );
    }
    // Sanity: the corpus actually exercises these paths.
    assert!(!ids_for(fx, "cascade", true).is_empty());
    assert!(!ids_for(fx, "müde", true).is_empty());
}

#[test]
fn keyset_pagination_has_no_duplicates_or_gaps() {
    let fx = fixture();
    let eng = engine();
    let cancel = QueryCancelHandle::new(eng.interrupt_handle());
    let filter = compiled(fx, "", true);
    let window = resolve_window(&TimeStrategy::All, None);
    let files = all_files(fx);

    // Forward walk.
    let mut seen: Vec<String> = Vec::new();
    let mut cursor: Option<String> = None;
    let mut last_page_first_cursor = None;
    loop {
        let page = query_page(
            &eng,
            &files,
            &filter,
            &window,
            &PageRequest {
                cursor: cursor.clone(),
                backward: false,
                limit: 7,
            },
            &cancel,
            None,
        )
        .expect("page");
        if page.rows.is_empty() {
            break;
        }
        seen.extend(page.rows.iter().map(|r| r.record_id.clone()));
        last_page_first_cursor = page.prev_cursor.clone();
        cursor = page.next_cursor.clone();
        if !page.has_more {
            break;
        }
    }
    let unique: BTreeSet<_> = seen.iter().cloned().collect();
    assert_eq!(seen.len(), fx.total_ab, "forward walk covers every record");
    assert_eq!(unique.len(), seen.len(), "no duplicates");

    // Untimestamped records are last.
    let full = query_page(
        &eng,
        &files,
        &filter,
        &window,
        &PageRequest {
            cursor: None,
            backward: false,
            limit: 1000,
        },
        &cancel,
        None,
    )
    .expect("full");
    let tail: Vec<_> = full
        .rows
        .iter()
        .rev()
        .take(fx.untimestamped_a)
        .map(|r| r.event_time)
        .collect();
    assert!(tail.iter().all(|t| t.is_none()));

    // Backward walk from the last page reconstructs the same order.
    let mut back: Vec<String> = Vec::new();
    let mut cursor = last_page_first_cursor;
    while let Some(c) = cursor {
        let page = query_page(
            &eng,
            &files,
            &filter,
            &window,
            &PageRequest {
                cursor: Some(c),
                backward: true,
                limit: 7,
            },
            &cancel,
            None,
        )
        .expect("page back");
        if page.rows.is_empty() {
            break;
        }
        for r in page.rows.iter().rev() {
            back.push(r.record_id.clone());
        }
        cursor = if page.has_more {
            page.prev_cursor.clone()
        } else {
            None
        };
    }
    // `back` holds every row before the final page, visited in reverse
    // display order — it must equal the forward prefix, reversed.
    let forward_order: Vec<String> = full.rows.iter().map(|r| r.record_id.clone()).collect();
    let expected_back: Vec<String> = forward_order[..back.len()].iter().rev().cloned().collect();
    assert_eq!(back, expected_back, "backward pages cover the exact prefix");

    // Invalid cursors are rejected, not executed.
    let err = query_page(
        &eng,
        &files,
        &filter,
        &window,
        &PageRequest {
            cursor: Some("not-a-cursor!!".into()),
            backward: false,
            limit: 7,
        },
        &cancel,
        None,
    )
    .unwrap_err();
    assert!(matches!(err, QueryError::InvalidParameter(_)));
}

#[test]
fn bounded_window_excludes_untimestamped_with_count() {
    let fx = fixture();
    let eng = engine();
    let cancel = QueryCancelHandle::new(eng.interrupt_handle());
    let filter = compiled(fx, "", true);
    let window = resolve_window(
        &TimeStrategy::Absolute {
            start: BASE_TS - 1,
            end: BASE_TS + 86_400_000_000_000,
        },
        None,
    );
    let files = all_files(fx);
    let counts = query_counts(&eng, &files, &filter, &window, &cancel, None).expect("counts");
    assert_eq!(counts.omitted_untimestamped, fx.untimestamped_a as i64);
    assert_eq!(counts.matching, (fx.total_ab - fx.untimestamped_a) as i64);
    let page = query_page(
        &eng,
        &files,
        &filter,
        &window,
        &PageRequest {
            cursor: None,
            backward: false,
            limit: 1000,
        },
        &cancel,
        None,
    )
    .expect("page");
    assert!(page.rows.iter().all(|r| r.event_time.is_some()));
}

#[test]
fn relative_window_resolves_against_latest_event() {
    let latest = BASE_TS + 500;
    let w = resolve_window(
        &TimeStrategy::RelativeToLatest {
            duration_nanos: 1_000,
        },
        Some(latest),
    );
    assert_eq!(w.start, Some(latest - 1_000));
    assert_eq!(w.end, Some(latest + 1));
    assert!(!w.empty_anchor);
    let none = resolve_window(
        &TimeStrategy::RelativeToLatest {
            duration_nanos: 1_000,
        },
        None,
    );
    assert!(none.empty_anchor);
}

#[test]
fn roadmap_query_selects_documented_records() {
    let fx = fixture();
    let ids = ids_for(
        fx,
        r#"service.name:workflow AND severity:(ERROR OR WARN) AND "timed out" AND NOT retry.count:0"#,
        true,
    );
    // ds-b: r1 (ERROR retry=1) and r3 (ERROR retry missing) match; r2 has
    // retry=0, r4 is INFO, r5 lacks the text.
    assert_eq!(ids.len(), 2, "exactly the two documented records: {ids:?}");
    let fallback = ids_for(
        fx,
        r#"service.name:workflow AND severity:(ERROR OR WARN) AND "timed out" AND NOT retry.count:0"#,
        false,
    );
    assert_eq!(ids, fallback);
}

#[test]
fn typed_attribute_predicates_execute() {
    let fx = fixture();
    // Nested int comparison.
    let hi = ids_for(fx, "http.status >= 500", true);
    assert!(!hi.is_empty());
    let all_a = ids_for(fx, "dataset:ds-a", true);
    assert!(hi.is_subset(&all_a));
    // Boolean.
    let active = ids_for(fx, "active:true", true);
    let inactive = ids_for(fx, "active:false", true);
    assert!(active.is_disjoint(&inactive));
    assert_eq!(active.len() + inactive.len(), 24, "only ds-a has `active`");
    // Wildcard on an attribute.
    let check = ids_for(fx, "service.name:check*", true);
    assert!(!check.is_empty());
    assert!(check.is_subset(&all_a));
    // Existence and missing.
    let has_retry = ids_for(fx, "retry.count:*", true);
    let no_retry = ids_for(fx, "NOT retry.count:*", true);
    assert_eq!(has_retry.len() + no_retry.len(), fx.total_ab);
    // The empty-value attribute counts as missing.
    let empty_attr = ids_for(fx, "empty_attr:*", true);
    assert!(empty_attr.is_empty());
    // Severity with NULL number falls back to text (one untimestamped
    // ERROR-text record exists).
    let errors = ids_for(fx, "severity:ERROR", true);
    let with_null_number = ids_for(fx, "severity:ERROR AND NOT timestamp:*", true);
    assert_eq!(with_null_number.len(), 1);
    assert!(with_null_number.is_subset(&errors));
    // Regex.
    let re = ids_for(fx, "message:/timed out|cascade/", true);
    let or = ids_for(fx, r#""timed out" OR cascade"#, true);
    assert!(
        re.is_superset(&or),
        "regex is substring-based; token OR is subset"
    );
}

#[test]
fn type_conflicts_are_reported_not_guessed() {
    let fx = fixture();
    let a = analyze("mixed:5", &fx.catalog_ab, &LangLimits::default());
    assert!(a.resolved.is_none());
    assert!(a.diagnostics.iter().any(|d| d.code == "lang/type-conflict"));
    // Narrowed to ds-b alone the field is int-typed and works.
    let b = analyze("mixed:5", &fx.catalog_b, &LangLimits::default());
    assert!(b.resolved.is_some(), "{:?}", b.diagnostics);
}

#[test]
fn facets_report_missing_and_truncation() {
    let fx = fixture();
    let eng = engine();
    let cancel = QueryCancelHandle::new(eng.interrupt_handle());
    let filter = compiled(fx, "", true);
    let window = resolve_window(&TimeStrategy::All, None);
    let files = all_files(fx);
    let facets = query_facets(
        &eng,
        &files,
        &filter,
        &window,
        &[(
            "retry.count".to_string(),
            FieldTarget::Attr {
                // Flat key containing a dot: one path segment.
                path: vec!["retry.count".into()],
            },
        )],
        2,
        &cancel,
        None,
    )
    .expect("facets");
    let f = &facets[0];
    assert!(f.truncated, "retry.count has 3+ distinct values");
    assert_eq!(f.values.len(), 2);
    // ds-a: every record has retry.count except the 3 untimestamped ones;
    // ds-b r3 lacks it too.
    assert_eq!(f.missing_count, 4);
}

#[test]
fn field_summary_reports_numeric_extent_and_distinct() {
    let fx = fixture();
    let eng = engine();
    let cancel = QueryCancelHandle::new(eng.interrupt_handle());
    let filter = compiled(fx, "dataset:ds-a", true);
    let window = resolve_window(&TimeStrategy::All, None);
    let files = all_files(fx);
    let summary = query_field_summary(
        &eng,
        &files,
        &filter,
        &window,
        "http.status",
        &FieldTarget::Attr {
            path: vec!["http".into(), "status".into()],
        },
        true,
        vec![logscope_query_lang::AttrType::Int],
        &cancel,
        None,
    )
    .expect("summary");
    assert_eq!(summary.present_count, 24);
    assert_eq!(summary.missing_count, 3);
    assert_eq!(summary.distinct_count, 2);
    assert!(summary.distinct_is_exact);
    assert_eq!(summary.min_numeric, Some(200.0));
    assert_eq!(summary.max_numeric, Some(503.0));
    assert!(!summary.high_cardinality);
}

#[test]
fn histogram_bins_are_aligned_and_zero_filled() {
    let fx = fixture();
    let eng = engine();
    let cancel = QueryCancelHandle::new(eng.interrupt_handle());
    let filter = compiled(fx, "dataset:ds-a", true);
    let window = resolve_window(&TimeStrategy::All, None);
    let h = query_histogram(&eng, &all_files(fx), &filter, &window, 50, &cancel, None)
        .expect("histogram");
    assert!(!h.empty);
    assert!(h.bins.len() <= 500);
    assert!(h.bin_width_nanos > 0);
    for (i, bin) in h.bins.iter().enumerate() {
        assert_eq!(bin.start % h.bin_width_nanos, 0, "aligned boundaries");
        if i > 0 {
            assert_eq!(
                bin.start,
                h.bins[i - 1].start + h.bin_width_nanos,
                "contiguous bins (zero-filled)"
            );
        }
    }
    assert_eq!(h.untimestamped_count, fx.untimestamped_a as i64);
    assert_eq!(
        h.total_in_range,
        (h.bins.iter().map(|b| b.count).sum::<i64>())
    );
}

#[test]
fn source_context_uses_source_order() {
    let fx = fixture();
    let eng = engine();
    let cancel = QueryCancelHandle::new(eng.interrupt_handle());
    // Find the anchor (record_number 10 in ds-a).
    let filter = compiled(fx, "dataset:ds-a", true);
    let window = resolve_window(&TimeStrategy::All, None);
    let page = query_page(
        &eng,
        &all_files(fx),
        &filter,
        &window,
        &PageRequest {
            cursor: None,
            backward: false,
            limit: 1000,
        },
        &cancel,
        None,
    )
    .expect("page");
    let anchor = page
        .rows
        .iter()
        .find(|r| r.record_number == Some(10))
        .expect("anchor");
    let origin_id: String = {
        let prov: serde_json::Value =
            serde_json::from_str(&anchor.provenance_json).expect("prov json");
        prov["origin"]["file_id"].as_str().expect("file id").into()
    };
    let ctx = query_source_context(
        &eng,
        &fx.files_a,
        "ds-a",
        &origin_id,
        &anchor.record_id,
        10,
        2,
        2,
        &cancel,
        None,
    )
    .expect("context");
    let numbers: Vec<u64> = ctx.records.iter().filter_map(|r| r.record_number).collect();
    assert_eq!(
        numbers,
        vec![8, 9, 10, 11, 12],
        "source order, not time order"
    );
    assert!(ctx.records.iter().any(|r| r.record_id == anchor.record_id));
}

#[test]
fn cancellation_and_timeout_leave_engine_usable() {
    let fx = fixture();
    let eng = engine();

    // Pre-cancelled catalog computation aborts at a checkpoint.
    let cancel = QueryCancelHandle::new(eng.interrupt_handle());
    cancel.cancel();
    let err = compute_field_stats(&eng, &fx.files_big, &cancel, Duration::from_secs(30))
        .expect_err("cancelled");
    assert!(matches!(err, QueryError::Cancelled), "got {err:?}");

    // The engine still answers immediately afterwards.
    let cancel2 = QueryCancelHandle::new(eng.interrupt_handle());
    let filter = compiled(fx, "cascade", true);
    let window = resolve_window(&TimeStrategy::All, None);
    let page = query_page(
        &eng,
        &all_files(fx),
        &filter,
        &window,
        &PageRequest {
            cursor: None,
            backward: false,
            limit: 10,
        },
        &cancel2,
        None,
    )
    .expect("page after cancel");
    assert!(!page.rows.is_empty());
}

#[test]
fn fts_overflow_falls_back_to_exact_scan() {
    let fx = fixture();
    // The big dataset has 20k identical messages; with a tiny candidate
    // bound the compiler must not truncate — it switches to the regex scan
    // and still returns every match.
    let eng = engine();
    let cancel = QueryCancelHandle::new(eng.interrupt_handle());
    let fts = FtsIndex::open(&fx.fts_path).expect("fts");
    // ds-big was never FTS-indexed (simulates index-not-ready): compile
    // with no index → regex; with index → the ids differ if FTS were used
    // blindly. The service passes index=None for unindexed datasets.
    let expr = {
        let a = analyze("bulk", &fx.catalog_ab, &LangLimits::default());
        a.resolved.expect("valid").expr
    };
    let ctx_no_index = FtsContext {
        index: None,
        dataset_ids: &["ds-big".to_string()],
    };
    let filter = compile_filter(expr.as_ref(), &ctx_no_index).expect("compile");
    assert!(filter.used_fallback_scan());
    let window = resolve_window(&TimeStrategy::All, None);
    let counts =
        query_counts(&eng, &fx.files_big, &filter, &window, &cancel, None).expect("counts");
    assert_eq!(counts.matching, 20_000);
    // With the index present but the dataset unindexed, FTS would return
    // zero candidates — proving why index-ready gating matters. The
    // compiler is only ever handed a ready index by the service layer.
    let ctx_index = FtsContext {
        index: Some(&fts),
        dataset_ids: &["ds-big".to_string()],
    };
    let filter2 = compile_filter(expr.as_ref(), &ctx_index).expect("compile");
    assert!(filter2.used_fts());
    let counts2 =
        query_counts(&eng, &fx.files_big, &filter2, &window, &cancel, None).expect("counts");
    assert_eq!(counts2.matching, 0, "unready index must never be passed in");
}
