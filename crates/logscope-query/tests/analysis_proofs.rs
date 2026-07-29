//! v0.0 analysis proofs: span graph reconstruction without fabrication,
//! representative metric rollup, and query execution budgets.

use std::path::PathBuf;
use std::time::Duration;

use logscope_model::*;
use logscope_query::*;
use logscope_store::{MetricSegmentWriter, SpanSegmentWriter};

fn prov(dataset: &str) -> IngestProvenance {
    IngestProvenance {
        dataset_id: dataset.into(),
        logical_source_id: "src-1".into(),
        origin: PhysicalOrigin::File {
            file_id: "file-1".into(),
            archive_entry: None,
        },
        locator: RecordLocator {
            record_number: Some(1),
            ..Default::default()
        },
        parser_id: "test".into(),
        parser_version: "0".into(),
        profile_id: None,
        profile_version: None,
        normalizer_version: "0".into(),
        protocol: SourceProtocol::FileImport,
        content_type: None,
        ingest_time: UnixNanos(0),
        raw_hash: "00".into(),
        original_timestamp_precision: None,
        flags: vec![],
    }
}

#[allow(clippy::too_many_arguments)]
fn span(
    trace: &str,
    id: &str,
    parent: Option<&str>,
    name: &str,
    start: i64,
    end: Option<i64>,
    links: Vec<SpanLink>,
    flags: Option<u32>,
) -> SpanRecord {
    SpanRecord {
        record_id: String::new(),
        trace_id: TraceId::from_hex(trace).unwrap(),
        span_id: SpanId::from_hex(id).unwrap(),
        parent_span_id: parent.map(|p| SpanId::from_hex(p).unwrap()),
        trace_state: None,
        flags,
        name: name.into(),
        kind: SpanKind::Server,
        start_time: UnixNanos(start),
        end_time: end.map(UnixNanos),
        status: SpanStatus {
            code: StatusCode::Unset,
            message: None,
        },
        resource_id: "res-1".into(),
        scope_id: "scp-1".into(),
        attributes: AttrMap::new(),
        events: vec![],
        links,
        dropped_attributes_count: 0,
        dropped_events_count: 0,
        dropped_links_count: 0,
        provenance: prov("ds-spans"),
    }
    .seal()
}

const TRACE: &str = "0af7651916cd43dd8448eb211c80319c";
const OTHER_TRACE: &str = "1af7651916cd43dd8448eb211c80319d";

#[test]
fn span_graph_preserves_problems_without_fabrication() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("spans-a.parquet");
    let mut w = SpanSegmentWriter::create(&path).unwrap();

    let link_to_other = SpanLink {
        trace_id: TraceId::from_hex(OTHER_TRACE).unwrap(),
        span_id: SpanId::from_hex("cccccccccccccccc").ok(),
        trace_state: None,
        attributes: AttrMap::new(),
        dropped_attributes_count: 0,
        flags: None,
    };
    let link_in_trace = SpanLink {
        trace_id: TraceId::from_hex(TRACE).unwrap(),
        span_id: SpanId::from_hex("bbbbbbbbbbbbbbbb").ok(),
        trace_state: None,
        attributes: AttrMap::new(),
        dropped_attributes_count: 0,
        flags: None,
    };

    let spans = vec![
        // root (sampled)
        span(
            TRACE,
            "aaaaaaaaaaaaaaaa",
            None,
            "GET /root",
            1_000,
            Some(9_000),
            vec![],
            Some(1),
        ),
        // normal child
        span(
            TRACE,
            "bbbbbbbbbbbbbbbb",
            Some("aaaaaaaaaaaaaaaa"),
            "child",
            2_000,
            Some(3_000),
            vec![],
            Some(1),
        ),
        // orphan: parent never arrives
        span(
            TRACE,
            "dddddddddddddddd",
            Some("9999999999999999"),
            "orphan",
            2_500,
            Some(2_600),
            vec![],
            None,
        ),
        // incomplete: no end time
        span(
            TRACE,
            "eeeeeeeeeeeeeeee",
            Some("aaaaaaaaaaaaaaaa"),
            "incomplete",
            4_000,
            None,
            vec![],
            Some(0),
        ),
        // clock skew: child starts before parent
        span(
            TRACE,
            "ffffffffffffffff",
            Some("bbbbbbbbbbbbbbbb"),
            "skewed",
            1_500,
            Some(1_800),
            vec![],
            None,
        ),
        // linked producer span with in-trace and cross-trace links
        span(
            TRACE,
            "abababababababab",
            Some("aaaaaaaaaaaaaaaa"),
            "producer",
            5_000,
            Some(5_500),
            vec![link_in_trace, link_to_other],
            None,
        ),
        // duplicate span id (same id as child, different content)
        span(
            TRACE,
            "bbbbbbbbbbbbbbbb",
            Some("aaaaaaaaaaaaaaaa"),
            "child-duplicate",
            2_100,
            Some(2_900),
            vec![],
            None,
        ),
        // unrelated trace must not appear
        span(
            OTHER_TRACE,
            "1212121212121212",
            None,
            "other",
            1,
            Some(2),
            vec![],
            None,
        ),
    ];
    w.write_batch(&spans).unwrap();
    let stats = w.finish().unwrap();
    assert_eq!(stats.rows, 8);

    let engine = EngineConnection::open_in_memory().unwrap();
    let graph = reconstruct_trace(&engine, &[path], TRACE).unwrap();

    assert_eq!(graph.integrity.span_count, 7, "other trace excluded");
    assert_eq!(graph.roots, vec!["aaaaaaaaaaaaaaaa".to_string()]);
    assert!(!graph.integrity.missing_root);
    assert_eq!(graph.integrity.orphan_count, 1);
    assert_eq!(graph.integrity.incomplete_count, 1);
    assert!(graph.integrity.clock_skew_count >= 1);
    assert_eq!(
        graph.integrity.duplicate_span_ids,
        vec!["bbbbbbbbbbbbbbbb".to_string()]
    );

    // Orphans are reported as unresolved references, never as nodes.
    assert_eq!(graph.unresolved_parents.len(), 1);
    assert_eq!(
        graph.unresolved_parents[0].parent_span_id,
        "9999999999999999"
    );
    assert!(
        graph.nodes.iter().all(|n| n.span_id != "9999999999999999"),
        "no synthetic spans"
    );

    // Both duplicate spans survive.
    assert_eq!(
        graph
            .nodes
            .iter()
            .filter(|n| n.span_id == "bbbbbbbbbbbbbbbb")
            .count(),
        2
    );

    // Links are first-class edges with resolution info.
    let links: Vec<_> = graph
        .link_edges
        .iter()
        .filter(|l| l.from_span_id == "abababababababab")
        .collect();
    assert_eq!(links.len(), 2);
    assert!(links
        .iter()
        .any(|l| l.to_trace_id == TRACE && l.resolved_in_trace));
    assert!(links
        .iter()
        .any(|l| l.to_trace_id == OTHER_TRACE && !l.resolved_in_trace));

    // Sampled bit survives.
    let root = graph
        .nodes
        .iter()
        .find(|n| n.span_id == "aaaaaaaaaaaaaaaa")
        .unwrap();
    assert_eq!(root.sampled, Some(true));

    // Annotations mark the problem spans.
    let orphan = graph
        .nodes
        .iter()
        .find(|n| n.span_id == "dddddddddddddddd")
        .unwrap();
    assert!(orphan.annotations.contains(&"orphan_parent".to_string()));
    let incomplete = graph
        .nodes
        .iter()
        .find(|n| n.span_id == "eeeeeeeeeeeeeeee")
        .unwrap();
    assert!(incomplete.annotations.contains(&"missing_end".to_string()));
    assert_eq!(incomplete.duration_nanos, None, "no invented durations");
}

#[test]
fn metric_rollup_aggregates_gauge_and_delta_sum() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("metrics-a.parquet");
    let mut w = MetricSegmentWriter::create(&path).unwrap();

    let point = |t: i64, v: NumberValue| NumberPoint {
        common: PointCommon {
            attributes: AttrMap::new(),
            start_time: Some(UnixNanos(0)),
            time: UnixNanos(t),
            flags: 0,
            exemplars: vec![],
            quality: vec![],
        },
        value: v,
    };

    let sec = 1_000_000_000i64;
    let records = vec![
        MetricRecord {
            record_id: String::new(),
            name: "queue.depth".into(),
            description: None,
            unit: Some("{item}".into()),
            data: MetricData::Gauge {
                points: vec![
                    point(sec, NumberValue::Int(10)),
                    point(30 * sec, NumberValue::Int(20)),
                    point(70 * sec, NumberValue::Int(50)),
                ],
            },
            metadata: AttrMap::new(),
            resource_id: "res-1".into(),
            scope_id: "scp-1".into(),
            provenance: prov("ds-metrics"),
        }
        .seal(),
        MetricRecord {
            record_id: String::new(),
            name: "requests.count".into(),
            description: None,
            unit: Some("{request}".into()),
            data: MetricData::Sum {
                temporality: Temporality::Delta,
                is_monotonic: true,
                points: vec![
                    point(10 * sec, NumberValue::Int(4)),
                    point(50 * sec, NumberValue::Int(6)),
                    point(80 * sec, NumberValue::Int(5)),
                ],
            },
            metadata: AttrMap::new(),
            resource_id: "res-1".into(),
            scope_id: "scp-1".into(),
            provenance: prov("ds-metrics"),
        }
        .seal(),
        // Cumulative sums are stored but excluded from this rollup.
        MetricRecord {
            record_id: String::new(),
            name: "requests.count".into(),
            description: None,
            unit: None,
            data: MetricData::Sum {
                temporality: Temporality::Cumulative,
                is_monotonic: true,
                points: vec![point(20 * sec, NumberValue::Int(1000))],
            },
            metadata: AttrMap::new(),
            resource_id: "res-1".into(),
            scope_id: "scp-1".into(),
            provenance: prov("ds-metrics"),
        }
        .seal(),
    ];
    w.write_batch(&records).unwrap();
    let stats = w.finish().unwrap();
    assert_eq!(stats.rows, 7);

    let engine = EngineConnection::open_in_memory().unwrap();
    let minute = 60 * sec;

    let gauge = rollup_gauge_or_delta_sum(
        &engine,
        std::slice::from_ref(&path),
        "queue.depth",
        minute,
        None,
        None,
    )
    .unwrap();
    assert_eq!(gauge.len(), 2);
    assert_eq!(gauge[0].bucket_start, 0);
    assert_eq!(gauge[0].points, 2);
    assert_eq!(gauge[0].min, Some(10.0));
    assert_eq!(gauge[0].max, Some(20.0));
    assert_eq!(gauge[0].avg, Some(15.0));
    assert_eq!(gauge[1].bucket_start, minute);
    assert_eq!(gauge[1].points, 1);
    assert_eq!(gauge[1].sum, Some(50.0));

    let sums =
        rollup_gauge_or_delta_sum(&engine, &[path], "requests.count", minute, None, None).unwrap();
    // Only the delta series participates: bucket0 = 4+6 = 10, bucket1 = 5.
    assert_eq!(sums.len(), 2);
    assert_eq!(sums[0].sum, Some(10.0));
    assert_eq!(sums[1].sum, Some(5.0));
    let total_points: u64 = sums.iter().map(|r| r.points).sum();
    assert_eq!(
        total_points, 3,
        "cumulative point must not leak into rollup"
    );
}

#[test]
fn query_budget_times_out_and_connection_survives() {
    let engine = EngineConnection::open_in_memory().unwrap();
    let cancel = QueryCancelHandle::new(engine.interrupt_handle());
    let result: Result<i64, QueryError> = run_bounded(&cancel, Duration::from_millis(200), || {
        Ok(engine.raw().query_row(
            "SELECT count(*) FROM range(300000) a, range(300000) b",
            [],
            |r| r.get(0),
        )?)
    });
    assert!(matches!(result, Err(QueryError::Timeout)), "{result:?}");
    // Connection still usable.
    let ok: i64 = engine
        .raw()
        .query_row("SELECT 7", [], |r| r.get(0))
        .unwrap();
    assert_eq!(ok, 7);
}

#[test]
fn external_cancellation_maps_to_cancelled() {
    let engine = EngineConnection::open_in_memory().unwrap();
    let cancel = QueryCancelHandle::new(engine.interrupt_handle());
    let canceller = {
        let cancel = cancel.clone();
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(100));
            cancel.cancel();
        })
    };
    let result: Result<i64, QueryError> = run_bounded(&cancel, Duration::from_secs(60), || {
        Ok(engine.raw().query_row(
            "SELECT count(*) FROM range(300000) a, range(300000) b",
            [],
            |r| r.get(0),
        )?)
    });
    canceller.join().unwrap();
    assert!(matches!(result, Err(QueryError::Cancelled)), "{result:?}");
}

#[test]
fn empty_segment_list_yields_empty_page() {
    let engine = EngineConnection::open_in_memory().unwrap();
    let cancel = QueryCancelHandle::new(engine.interrupt_handle());
    let page = query_log_page(
        &engine,
        &Vec::<PathBuf>::new(),
        &LogQueryRequest {
            dataset_ids: vec!["ds".into()],
            limit: 10,
            ..Default::default()
        },
        None,
        &cancel,
        None,
    )
    .unwrap();
    assert!(page.rows.is_empty());
    assert!(!page.has_more);
}
