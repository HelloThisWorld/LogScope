// Log Explorer (v0.2): query editor with authoritative diagnostics,
// histogram with brush, virtualized event table, facets, detail/context
// panel, saved views, and bounded export. All semantics come from the
// Rust services; this file only presents them. Log content is rendered
// exclusively through React text nodes (never HTML).

import {
  useCallback,
  useEffect,
  useMemo,
  useRef,
  useState,
} from "react";
import { listen } from "@tauri-apps/api/event";
import { save as saveDialog } from "@tauri-apps/plugin-dialog";
import { api, errorText } from "./api";
import type {
  ColumnSetDto,
  DiagnosticDto,
  ExportStatusDto,
  FacetDto,
  FieldCatalogDto,
  HighlightDto,
  HistogramDto,
  LogRowV2Dto,
  OverviewDto,
  QueryPageV2Dto,
  RecentSearchDto,
  RecordDetailDto,
  RestoreContextDto,
  SavedSearchDto,
  SourceContextDto,
  TimeStrategyDto,
} from "./api";
import PinDialog from "./PinDialog";
import type { PinScope, PinTarget } from "./PinDialog";

const ROW_HEIGHT = 26;
const PAGE_LIMIT = 200;
const MAX_LOADED_ROWS = 10_000;
const MAX_CELL_CHARS = 400;
const DEFAULT_COLUMNS = ["timestamp", "severity", "message"];
const RELATIVE_PRESETS: Array<[string, number]> = [
  ["15 minutes", 15 * 60e9],
  ["1 hour", 3600e9],
  ["24 hours", 24 * 3600e9],
  ["7 days", 7 * 24 * 3600e9],
  ["30 days", 30 * 24 * 3600e9],
];

/** i64 DTO fields are bigint; UI math happens in number space. */
function nb(n: number): bigint {
  return BigInt(Math.round(n));
}

function clampText(s: string, max = MAX_CELL_CHARS): string {
  return s.length > max ? s.slice(0, max) + " …" : s;
}

function fmtNanos(n: number | null | undefined): string {
  if (n === null || n === undefined) return "—";
  const d = new Date(Number(BigInt(n) / 1000000n));
  return d.toISOString().replace("T", " ").replace("Z", "");
}

function fmtCount(n: number | bigint): string {
  return Number(n).toLocaleString("en-US");
}

/** Tagged canonical attribute JSON → flat [path, typeTag, display] rows. */
function flattenAttrs(
  json: string,
  limit = 200,
): Array<[string, string, string]> {
  const out: Array<[string, string, string]> = [];
  let parsed: unknown;
  try {
    parsed = JSON.parse(json);
  } catch {
    return out;
  }
  const walk = (prefix: string, node: unknown) => {
    if (out.length >= limit || typeof node !== "object" || node === null) return;
    for (const [key, tagged] of Object.entries(node as Record<string, unknown>)) {
      if (out.length >= limit) return;
      const t = tagged as { t?: string; v?: unknown };
      const path = prefix ? `${prefix}.${key}` : key;
      if (!t || typeof t.t !== "string") continue;
      if (t.t === "map") {
        out.push([path, "object", ""]);
        walk(path, t.v);
      } else if (t.t === "array") {
        out.push([path, "array", clampText(JSON.stringify(t.v ?? []))]);
      } else if (t.t === "empty") {
        out.push([path, "empty", ""]);
      } else {
        out.push([path, t.t, clampText(String(t.v ?? ""))]);
      }
    }
  };
  walk("", parsed);
  return out;
}

function severityClass(sev: string | null): string {
  switch (sev) {
    case "ERROR":
    case "FATAL":
      return "sev-error";
    case "WARN":
      return "sev-warn";
    case "DEBUG":
    case "TRACE":
      return "sev-debug";
    default:
      return "sev-info";
  }
}

async function copyText(text: string) {
  try {
    await navigator.clipboard.writeText(text);
  } catch {
    // Clipboard access can be denied; the button simply does nothing then.
  }
}

/** Query text rendered with lexical highlights (text nodes only). */
function HighlightedQuery({
  text,
  highlights,
  diagnostics,
}: {
  text: string;
  highlights: HighlightDto[];
  diagnostics: DiagnosticDto[];
}) {
  const parts = useMemo(() => {
    const marks = highlights
      .map((h) => ({ ...h.span, cls: `hl-${h.kind}` }))
      .sort((a, b) => a.start_utf16 - b.start_utf16);
    const errSpans = diagnostics
      .filter((d) => d.severity === "error")
      .map((d) => d.span);
    const inErr = (s: number, e: number) =>
      errSpans.some((x) => s < x.end_utf16 && e > x.start_utf16);
    const out: Array<{ text: string; cls: string }> = [];
    let pos = 0;
    for (const m of marks) {
      if (m.start_utf16 > pos) {
        out.push({ text: text.slice(pos, m.start_utf16), cls: "" });
      }
      const seg = text.slice(m.start_utf16, m.end_utf16);
      out.push({
        text: seg,
        cls: m.cls + (inErr(m.start_utf16, m.end_utf16) ? " hl-error" : ""),
      });
      pos = m.end_utf16;
    }
    if (pos < text.length) out.push({ text: text.slice(pos), cls: "" });
    return out;
  }, [text, highlights, diagnostics]);
  return (
    <pre className="editor-highlight" aria-hidden="true">
      {parts.map((p, i) => (
        <span key={i} className={p.cls || undefined}>
          {p.text}
        </span>
      ))}
      {"\n"}
    </pre>
  );
}

function Histogram({
  data,
  onBrush,
  onReset,
  brushed,
}: {
  data: HistogramDto | null;
  onBrush: (startNanos: number, endNanos: number) => void;
  onReset: () => void;
  brushed: boolean;
}) {
  const ref = useRef<HTMLDivElement>(null);
  const [drag, setDrag] = useState<[number, number] | null>(null);
  if (!data || data.empty || data.bins.length === 0) {
    return (
      <div className="histogram histogram-empty" role="img" aria-label="No timestamped data for histogram">
        {data && Number(data.untimestamped_count) > 0
          ? `no timestamped records in range · ${fmtCount(data.untimestamped_count)} without timestamps`
          : "no histogram data"}
      </div>
    );
  }
  const bins = data.bins;
  const max = Math.max(...bins.map((b) => Number(b.count)), 1);
  const W = 100 / bins.length;
  const toNanos = (frac: number) =>
    Number(data.start) + Math.round(frac * (Number(data.end) - Number(data.start)));
  const frac = (clientX: number) => {
    const rect = ref.current?.getBoundingClientRect();
    if (!rect) return 0;
    return Math.min(1, Math.max(0, (clientX - rect.left) / rect.width));
  };
  return (
    <div className="histogram-wrap">
      <div
        ref={ref}
        className="histogram"
        role="img"
        aria-label={`Histogram: ${fmtCount(data.total_in_range)} records between ${fmtNanos(
          Number(data.start),
        )} and ${fmtNanos(Number(data.end))} UTC`}
        onMouseDown={(e) => {
          const f = frac(e.clientX);
          setDrag([f, f]);
        }}
        onMouseMove={(e) => {
          if (drag) setDrag([drag[0], frac(e.clientX)]);
        }}
        onMouseUp={() => {
          if (drag) {
            const [a, b] = [Math.min(...drag), Math.max(...drag)];
            setDrag(null);
            if (b - a > 0.005) onBrush(toNanos(a), toNanos(b));
          }
        }}
        onMouseLeave={() => setDrag(null)}
      >
        {bins.map((b, i) => (
          <div
            key={i}
            className="histogram-bar"
            style={{
              left: `${i * W}%`,
              width: `${W}%`,
              height: `${(Number(b.count) / max) * 100}%`,
            }}
            title={`${fmtNanos(Number(b.start))} UTC · ${fmtCount(b.count)}`}
          />
        ))}
        {drag && (
          <div
            className="histogram-brush"
            style={{
              left: `${Math.min(...drag) * 100}%`,
              width: `${Math.abs(drag[1] - drag[0]) * 100}%`,
            }}
          />
        )}
      </div>
      <div className="histogram-meta dim">
        <span>
          {fmtNanos(Number(data.start))} → {fmtNanos(Number(data.end))} UTC ·{" "}
          {fmtCount(data.total_in_range)} in range
          {Number(data.untimestamped_count) > 0 &&
            ` · ${fmtCount(data.untimestamped_count)} without timestamps (not drawn)`}
        </span>
        {brushed && (
          <button className="link" onClick={onReset} aria-label="Reset time range">
            reset range
          </button>
        )}
      </div>
    </div>
  );
}

export default function Explorer({
  overview,
  onBack,
  onOpenImport,
  restore,
  onRestoreConsumed,
}: {
  overview: OverviewDto;
  onBack: () => void;
  onOpenImport: () => void;
  /** Evidence jump-back: restore this captured context verbatim. */
  restore?: RestoreContextDto | null;
  onRestoreConsumed?: () => void;
}) {
  const logDatasets = overview.datasets.filter(
    (d) => d.signal === "logs" && d.status === "published",
  );
  const [selection, setSelection] = useState<string[]>([]);
  const [queryText, setQueryText] = useState("");
  const [diagnostics, setDiagnostics] = useState<DiagnosticDto[]>([]);
  const [highlights, setHighlights] = useState<HighlightDto[]>([]);
  const [catalog, setCatalog] = useState<FieldCatalogDto | null>(null);
  const [strategy, setStrategy] = useState<TimeStrategyDto>({
    kind: "all",
    start: null,
    end: null,
    duration_nanos: null,
  });
  const [savedStrategy, setSavedStrategy] = useState<TimeStrategyDto | null>(null);

  const [rows, setRows] = useState<LogRowV2Dto[]>([]);
  const [page, setPage] = useState<QueryPageV2Dto | null>(null);
  const [histogram, setHistogram] = useState<HistogramDto | null>(null);
  const [facets, setFacets] = useState<FacetDto[]>([]);
  const [facetFields, setFacetFields] = useState<string[]>(["severity", "dataset"]);
  const [phase, setPhase] = useState<
    "idle" | "running" | "done" | "cancelled" | "failed" | "timed-out"
  >("idle");
  const [error, setError] = useState("");
  const [loadingMore, setLoadingMore] = useState(false);
  const [columns, setColumns] = useState<string[]>(DEFAULT_COLUMNS);
  const [columnSetsList, setColumnSetsList] = useState<ColumnSetDto[]>([]);
  const [savedList, setSavedList] = useState<SavedSearchDto[]>([]);
  const [recentList, setRecentList] = useState<RecentSearchDto[]>([]);
  const [panel, setPanel] = useState<"fields" | "saved" | "recent" | "columns">(
    "fields",
  );
  const [selected, setSelected] = useState<LogRowV2Dto | null>(null);
  const [detail, setDetail] = useState<RecordDetailDto | null>(null);
  const [context, setContext] = useState<SourceContextDto | null>(null);
  const [detailTab, setDetailTab] = useState<
    "fields" | "attributes" | "resource" | "provenance" | "context"
  >("fields");
  const [exportOpen, setExportOpen] = useState(false);
  const [exportRunning, setExportRunning] = useState<ExportStatusDto | null>(null);
  const [saveName, setSaveName] = useState("");
  const [scrollTop, setScrollTop] = useState(0);
  const [pinTarget, setPinTarget] = useState<PinTarget | null>(null);
  const [pinMessage, setPinMessage] = useState("");
  const [checked, setChecked] = useState<Set<string>>(new Set());

  const seqRef = useRef(0);
  const activeRequest = useRef<string | null>(null);
  const tableRef = useRef<HTMLDivElement>(null);
  const editorRef = useRef<HTMLTextAreaElement>(null);
  const runRef = useRef<
    | ((opts?: {
        strategyOverride?: TimeStrategyDto;
        recordRecent?: boolean;
      }) => Promise<void>)
    | null
  >(null);

  const effectiveSelection = selection.length
    ? selection
    : logDatasets.map((d) => d.dataset_id);

  const refreshMeta = useCallback(async () => {
    try {
      setCatalog(await api.fieldCatalog(selection));
      setSavedList(await api.savedSearches());
      setRecentList(await api.recentSearches());
      setColumnSetsList(await api.columnSets());
    } catch (e) {
      setError(errorText(e));
    }
  }, [selection]);

  useEffect(() => {
    void refreshMeta();
  }, [refreshMeta]);

  // Default column set on first load.
  useEffect(() => {
    const def = columnSetsList.find((c) => c.is_default);
    if (def && def.columns.length) setColumns(def.columns);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [columnSetsList.length]);

  // Debounced authoritative validation (no client-side grammar).
  useEffect(() => {
    const t = setTimeout(async () => {
      try {
        const a = await api.validateQuery(selection, queryText);
        setDiagnostics(a.diagnostics);
        setHighlights(a.highlights);
      } catch (e) {
        setError(errorText(e));
      }
    }, 150);
    return () => clearTimeout(t);
  }, [queryText, selection]);

  const hasErrors = diagnostics.some((d) => d.severity === "error");

  const run = useCallback(
    async (opts?: { strategyOverride?: TimeStrategyDto; recordRecent?: boolean }) => {
      const strat = opts?.strategyOverride ?? strategy;
      if (activeRequest.current) {
        void api.cancelQuery(activeRequest.current);
      }
      const seq = ++seqRef.current;
      const requestId = `q-${seq}-${Date.now()}`;
      activeRequest.current = requestId;
      setPhase("running");
      setError("");
      try {
        const [pageResult, histResult, facetResult] = await Promise.allSettled([
          api.runQuery({
            request_id: requestId,
            dataset_ids: selection,
            query_text: queryText,
            time_strategy: strat,
            cursor: null,
            backward: false,
            limit: PAGE_LIMIT,
            record_recent: opts?.recordRecent ?? true,
          }),
          api.runHistogram({
            request_id: `${requestId}-h`,
            dataset_ids: selection,
            query_text: queryText,
            time_strategy: strat,
            max_bins: 120,
          }),
          api.runFacets({
            request_id: `${requestId}-f`,
            dataset_ids: selection,
            query_text: queryText,
            time_strategy: strat,
            fields: facetFields,
            top_k: 8,
          }),
        ]);
        if (seq !== seqRef.current) return; // stale response: drop
        if (pageResult.status === "fulfilled") {
          setPage(pageResult.value);
          setRows(pageResult.value.rows);
          setPhase("done");
          tableRef.current?.scrollTo({ top: 0 });
          setScrollTop(0);
        } else {
          const msg = errorText(pageResult.reason);
          setPhase(
            msg.includes("query/cancelled")
              ? "cancelled"
              : msg.includes("query/timeout")
                ? "timed-out"
                : "failed",
          );
          setError(msg);
        }
        setHistogram(histResult.status === "fulfilled" ? histResult.value : null);
        setFacets(facetResult.status === "fulfilled" ? facetResult.value : []);
        void api.recentSearches().then(setRecentList).catch(() => {});
      } finally {
        if (activeRequest.current === requestId) activeRequest.current = null;
      }
    },
    [queryText, selection, strategy, facetFields],
  );

  const cancelRun = useCallback(() => {
    if (activeRequest.current) {
      void api.cancelQuery(activeRequest.current);
      void api.cancelQuery(`${activeRequest.current}-h`);
      void api.cancelQuery(`${activeRequest.current}-f`);
    }
  }, []);

  runRef.current = run;

  /** The exact scope a pin is made from — what is on screen, verbatim. */
  const currentScope = (): PinScope => ({
    queryText,
    datasetIds: selection,
    timeStrategy: strategy,
  });

  // Evidence jump-back: apply the captured context exactly as stored —
  // exact query, exact datasets, concrete resolved bounds when present —
  // then run. Never broadened: a relative strategy is restored as the
  // absolute window it resolved to at pin time.
  useEffect(() => {
    if (!restore) return;
    const r = restore;
    onRestoreConsumed?.();
    setChecked(new Set());
    setSelection(r.dataset_ids);
    setQueryText(r.query_text ?? "");
    let strat: TimeStrategyDto =
      r.time_strategy ?? { kind: "all", start: null, end: null, duration_nanos: null };
    const freeze = (s: number, e: number): TimeStrategyDto => ({
      kind: "absolute",
      start: nb(s),
      end: nb(e),
      duration_nanos: null,
    });
    if (r.interval_start != null && r.interval_end != null) {
      strat = freeze(r.interval_start, r.interval_end);
    } else if (
      strat.kind === "relative_to_latest" &&
      r.resolved_start != null &&
      r.resolved_end != null
    ) {
      strat = freeze(r.resolved_start, r.resolved_end);
    }
    setStrategy(strat);
    setPinMessage(`restored from evidence (${r.kind})`);
    if (r.kind === "event" && r.record_id && r.dataset_id) {
      const datasetId = r.dataset_id;
      const recordId = r.record_id;
      void (async () => {
        try {
          const d = await api.getRecord(datasetId, recordId);
          setSelected(d.row);
          setDetail(d);
          setDetailTab("fields");
        } catch (e) {
          setError(errorText(e));
        }
      })();
    } else if (r.query_text != null) {
      // Let React commit the restored state, then execute it.
      setTimeout(() => {
        void runRef.current?.({ strategyOverride: strat, recordRecent: false });
      }, 0);
    }
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [restore]);

  const loadMore = useCallback(async () => {
    if (!page?.has_more || !page.next_cursor || loadingMore) return;
    if (rows.length >= MAX_LOADED_ROWS) return;
    setLoadingMore(true);
    try {
      const next = await api.runQuery({
        request_id: `q-more-${Date.now()}`,
        dataset_ids: selection,
        query_text: queryText,
        time_strategy: strategy,
        cursor: page.next_cursor,
        backward: false,
        limit: PAGE_LIMIT,
        record_recent: false,
      });
      setRows((r) => [...r, ...next.rows].slice(0, MAX_LOADED_ROWS));
      setPage((p) =>
        p ? { ...p, next_cursor: next.next_cursor, has_more: next.has_more } : p,
      );
    } catch (e) {
      setError(errorText(e));
    } finally {
      setLoadingMore(false);
    }
  }, [page, rows.length, loadingMore, selection, queryText, strategy]);

  // Virtualization window.
  const viewH = 480;
  const first = Math.max(0, Math.floor(scrollTop / ROW_HEIGHT) - 10);
  const visible = Math.ceil(viewH / ROW_HEIGHT) + 20;
  const slice = rows.slice(first, first + visible);
  useEffect(() => {
    if (rows.length && scrollTop + viewH > (rows.length - 50) * ROW_HEIGHT) {
      void loadMore();
    }
  }, [scrollTop, rows.length, loadMore]);

  const openDetail = useCallback(async (row: LogRowV2Dto) => {
    setSelected(row);
    setDetail(null);
    setContext(null);
    try {
      setDetail(await api.getRecord(row.dataset_id, row.record_id));
      setContext(
        await api.sourceContext({
          dataset_id: row.dataset_id,
          record_id: row.record_id,
          before: 5,
          after: 5,
        }),
      );
    } catch (e) {
      setError(errorText(e));
    }
  }, []);

  const addPredicate = useCallback(
    async (field: string, value: string, negate = false) => {
      const pred = await api.buildPredicate(field, value, negate);
      setQueryText((t) => (t.trim() ? `${t.trim()} AND ${pred}` : pred));
      editorRef.current?.focus();
    },
    [],
  );

  const cellValue = (row: LogRowV2Dto, col: string): string => {
    switch (col) {
      case "timestamp":
        return row.event_time_text ?? "—";
      case "severity":
        return row.severity ?? "—";
      case "message":
        return row.message;
      case "dataset":
        return (
          logDatasets.find((d) => d.dataset_id === row.dataset_id)?.name ??
          row.dataset_id
        );
      case "trace_id":
        return row.trace_id ?? "";
      case "record_id":
        return row.record_id;
      default: {
        const found = flattenAttrs(row.attributes_json, 500).find(
          ([p]) => p === col,
        );
        return found ? found[2] : "";
      }
    }
  };

  const onKeyDownTable = (e: React.KeyboardEvent) => {
    if (!rows.length) return;
    const idx = selected
      ? rows.findIndex((r) => r.record_id === selected.record_id)
      : -1;
    if (e.key === "ArrowDown") {
      e.preventDefault();
      void openDetail(rows[Math.min(rows.length - 1, idx + 1)]);
    } else if (e.key === "ArrowUp") {
      e.preventDefault();
      void openDetail(rows[Math.max(0, idx - 1)]);
    }
  };

  const doExport = async (format: "csv" | "jsonl") => {
    const dest = await saveDialog({
      title: `Export ${format.toUpperCase()}`,
      defaultPath: `logscope-export.${format}`,
    });
    if (!dest) return;
    try {
      const status = await api.startExport({
        dataset_ids: selection,
        query_text: queryText,
        time_strategy: strategy,
        format,
        destination: dest,
        row_limit: null,
        byte_limit: null,
        csv_columns: format === "csv" ? columns : [],
      });
      setExportRunning(status);
    } catch (e) {
      setError(errorText(e));
    }
  };

  useEffect(() => {
    const un = listen<string>("export-finished", async () => {
      setExportRunning((cur) => {
        if (cur) {
          void api
            .exportStatus(cur.export_id)
            .then((s) => setExportRunning(s))
            .catch(() => {});
        }
        return cur;
      });
    });
    return () => {
      un.then((f) => f());
    };
  }, []);

  if (logDatasets.length === 0) {
    return (
      <section className="explorer-empty">
        <h2>Explore logs</h2>
        <p>This workspace has no imported log data yet.</p>
        <button onClick={onOpenImport}>Import log files…</button>
        <button className="link" onClick={onBack}>
          back to workspace
        </button>
      </section>
    );
  }

  const ftsPending = catalog && !catalog.fts_ready;

  return (
    <div className="explorer">
      <header className="explorer-header">
        <button onClick={onBack} aria-label="Back to workspace overview">
          ← {overview.workspace.name}
        </button>
        <label>
          Datasets{" "}
          <select
            multiple
            aria-label="Dataset selection"
            value={effectiveSelection}
            onChange={(e) => {
              const picked = Array.from(e.target.selectedOptions).map(
                (o) => o.value,
              );
              setSelection(
                picked.length === logDatasets.length ? [] : picked,
              );
            }}
            size={Math.min(3, logDatasets.length)}
          >
            {logDatasets.map((d) => (
              <option key={d.dataset_id} value={d.dataset_id}>
                {d.name} ({fmtCount(d.row_count)})
              </option>
            ))}
          </select>
        </label>
        <span className="dim">
          {effectiveSelection.length}/{logDatasets.length} datasets · logs only
        </span>
        {ftsPending && (
          <span className="badge badge-warn" title="Text search uses the exact fallback scan until indexes finish">
            indexes building — exact scan in use
            <button
              className="link"
              onClick={() => api.rebuildIndexes().then(refreshMeta).catch((e) => setError(errorText(e)))}
            >
              rebuild now
            </button>
          </span>
        )}
        <span className="spacer" />
        <button
          onClick={() =>
            setPinTarget({
              kind: "query",
              scope: currentScope(),
              savedSearchId: null,
            })
          }
          disabled={phase !== "done"}
          title="Pin the current query, dataset selection and time window as evidence"
        >
          Pin query
        </button>
        {checked.size > 0 && (
          <button
            onClick={() =>
              setPinTarget({
                kind: "selection",
                recordIds: rows
                  .filter((r) => checked.has(r.record_id))
                  .map((r) => r.record_id),
                scope: currentScope(),
              })
            }
            title="Pin the checked rows, in table order, as one selection"
          >
            Pin selection ({checked.size})
          </button>
        )}
        <button onClick={() => setExportOpen(true)} disabled={phase !== "done"}>
          Export…
        </button>
      </header>

      <div className="query-row">
        <div className="editor-wrap">
          <HighlightedQuery
            text={queryText}
            highlights={highlights}
            diagnostics={diagnostics}
          />
          <textarea
            ref={editorRef}
            className="editor"
            aria-label="Query editor"
            spellCheck={false}
            rows={2}
            placeholder={'severity:(ERROR OR WARN) AND "timed out" — Ctrl+Enter runs'}
            value={queryText}
            onChange={(e) => setQueryText(e.target.value)}
            onKeyDown={(e) => {
              if ((e.ctrlKey || e.metaKey) && e.key === "Enter") {
                e.preventDefault();
                if (!hasErrors) void run();
              } else if (e.key === "Escape") {
                cancelRun();
              }
            }}
          />
        </div>
        <div className="query-actions">
          <button
            onClick={() => void run()}
            disabled={hasErrors || phase === "running"}
            aria-label="Run query (Ctrl+Enter)"
          >
            Run
          </button>
          <button
            onClick={cancelRun}
            disabled={phase !== "running"}
            aria-label="Cancel running query (Escape)"
          >
            Cancel
          </button>
          <button
            className="link"
            onClick={() => setQueryText("")}
            aria-label="Clear query text (keeps datasets and time range)"
          >
            clear
          </button>
          <button
            className="link"
            onClick={() => void copyText(queryText)}
            aria-label="Copy query text"
          >
            copy
          </button>
        </div>
        <div className="time-controls">
          <select
            aria-label="Time range strategy"
            value={strategy.kind}
            onChange={(e) => {
              const kind = e.target.value;
              if (kind === "all") {
                setStrategy({ kind: "all", start: null, end: null, duration_nanos: null });
              } else if (kind === "relative_to_latest") {
                setStrategy({
                  kind,
                  start: null,
                  end: null,
                  duration_nanos: nb(3600e9),
                });
              } else {
                const now = Date.now();
                setStrategy({
                  kind: "absolute",
                  start: nb((now - 86400e3) * 1e6),
                  end: nb(now * 1e6),
                  duration_nanos: null,
                });
              }
            }}
          >
            <option value="all">All data</option>
            <option value="relative_to_latest">Relative to newest event</option>
            <option value="absolute">Absolute range (UTC)</option>
          </select>
          {strategy.kind === "relative_to_latest" && (
            <select
              aria-label="Relative window duration"
              value={String(strategy.duration_nanos ?? 3600e9)}
              onChange={(e) =>
                setStrategy({
                  ...strategy,
                  duration_nanos: nb(Number(e.target.value)),
                })
              }
            >
              {RELATIVE_PRESETS.map(([label, nanos]) => (
                <option key={label} value={String(nanos)}>
                  last {label}
                </option>
              ))}
            </select>
          )}
          {strategy.kind === "absolute" && (
            <>
              <input
                type="datetime-local"
                aria-label="Range start (UTC)"
                value={new Date(Number(strategy.start ?? 0) / 1e6)
                  .toISOString()
                  .slice(0, 16)}
                onChange={(e) =>
                  setStrategy({
                    ...strategy,
                    start: nb(Date.parse(e.target.value + "Z") * 1e6),
                  })
                }
              />
              <input
                type="datetime-local"
                aria-label="Range end (UTC, exclusive)"
                value={new Date(Number(strategy.end ?? 0) / 1e6)
                  .toISOString()
                  .slice(0, 16)}
                onChange={(e) =>
                  setStrategy({
                    ...strategy,
                    end: nb(Date.parse(e.target.value + "Z") * 1e6),
                  })
                }
              />
            </>
          )}
        </div>
      </div>

      {diagnostics.length > 0 && (
        <ul className="diagnostics" aria-live="polite">
          {diagnostics.slice(0, 4).map((d, i) => (
            <li key={i} className={d.severity === "error" ? "diag-error" : "diag-warn"}>
              <code>{d.code}</code> {d.message}
              {d.hint ? <span className="dim"> — {d.hint}</span> : null}
            </li>
          ))}
        </ul>
      )}

      <Histogram
        data={histogram}
        brushed={strategy.kind === "absolute"}
        onBrush={(start, end) => {
          if (!savedStrategy) setSavedStrategy(strategy);
          const next: TimeStrategyDto = {
            kind: "absolute",
            start: nb(start),
            end: nb(end),
            duration_nanos: null,
          };
          setStrategy(next);
          void run({ strategyOverride: next, recordRecent: false });
        }}
        onReset={() => {
          const back = savedStrategy ?? {
            kind: "all",
            start: null,
            end: null,
            duration_nanos: null,
          };
          setSavedStrategy(null);
          setStrategy(back);
          void run({ strategyOverride: back, recordRecent: false });
        }}
      />

      <div className="status-line" role="status">
        {strategy.kind === "absolute" &&
          strategy.start != null &&
          strategy.end != null &&
          histogram &&
          !histogram.empty && (
            <button
              className="link"
              onClick={() =>
                setPinTarget({
                  kind: "interval",
                  scope: currentScope(),
                  start: Number(strategy.start),
                  end: Number(strategy.end),
                  bucketWidthNanos: Number(histogram.bin_width_nanos),
                  displayTimezone: "UTC",
                  neighborBuckets: histogram.bins
                    .slice(0, 60)
                    .map((b) => [Number(b.start), Number(b.count)]),
                })
              }
              title="Pin the brushed half-open interval with its visible histogram context"
            >
              📌 pin interval
            </button>
          )}
        {pinMessage && <span className="dim">{pinMessage}</span>}
        {phase === "running" && <span className="badge">running…</span>}
        {phase === "done" && page && (
          <>
            <span>
              <strong>{fmtCount(page.matching)}</strong> matching
              {Number(page.omitted_untimestamped) > 0 && (
                <span className="badge badge-warn" title="Records without a valid timestamp are excluded from a bounded time range">
                  {fmtCount(page.omitted_untimestamped)} without timestamps omitted
                </span>
              )}
            </span>
            <span className="dim">{Number(page.elapsed_ms)} ms</span>
            <span className="dim">
              {page.used_fallback_text_scan
                ? "text: exact scan"
                : page.used_fts
                  ? "text: indexed"
                  : ""}
            </span>
            {page.resolved_window.start_text && (
              <span className="dim">
                window {page.resolved_window.start_text} → {page.resolved_window.end_text} (UTC, end exclusive)
              </span>
            )}
            {rows.length >= MAX_LOADED_ROWS && (
              <span className="badge badge-warn">
                first {fmtCount(MAX_LOADED_ROWS)} rows loaded — narrow the query for more
              </span>
            )}
          </>
        )}
        {phase === "cancelled" && <span className="badge">cancelled</span>}
        {phase === "timed-out" && <span className="badge badge-warn">timed out</span>}
        {phase === "failed" && <span className="badge badge-error">failed</span>}
        {error && <span className="error-inline">{clampText(error, 300)}</span>}
      </div>

      <div className="explorer-main">
        <aside className="side-panel">
          <nav className="side-tabs" role="tablist">
            {(["fields", "saved", "recent", "columns"] as const).map((p) => (
              <button
                key={p}
                role="tab"
                aria-selected={panel === p}
                className={panel === p ? "active" : ""}
                onClick={() => setPanel(p)}
              >
                {p}
              </button>
            ))}
          </nav>
          {panel === "fields" && (
            <div className="field-panel">
              {!catalog?.complete && (
                <p className="dim">field catalog still building — list may be partial</p>
              )}
              <p className="dim">facets ({facetFields.length}/8):</p>
              {facets.map((f) => (
                <div key={f.display} className="facet">
                  <div className="facet-head">
                    <strong>{f.display}</strong>
                    <button
                      className="link"
                      aria-label={`Remove facet ${f.display}`}
                      onClick={() =>
                        setFacetFields((ff) => ff.filter((x) => x !== f.display))
                      }
                    >
                      ×
                    </button>
                  </div>
                  {f.error ? (
                    <p className="dim">{f.error}</p>
                  ) : (
                    <>
                      {f.values.map((v) => (
                        <div key={v.value} className="facet-value">
                          <button
                            className="link"
                            title={`Filter ${f.display} = ${v.value}`}
                            onClick={() => {
                              void addPredicate(f.display, v.value).then(() => run());
                            }}
                          >
                            {clampText(v.value, 40)}
                          </button>
                          <button
                            className="link dim"
                            aria-label={`Exclude ${v.value}`}
                            onClick={() => {
                              void addPredicate(f.display, v.value, true).then(() =>
                                run(),
                              );
                            }}
                          >
                            −
                          </button>
                          <button
                            className="link dim"
                            aria-label={`Pin group ${f.display} = ${v.value} as evidence`}
                            title="Pin this group as evidence"
                            onClick={() =>
                              setPinTarget({
                                kind: "group",
                                scope: currentScope(),
                                field: f.display,
                                valueJson: JSON.stringify(v.value),
                                label: v.value,
                              })
                            }
                          >
                            📌
                          </button>
                          <span className="num dim">{fmtCount(v.count)}</span>
                        </div>
                      ))}
                      {Number(f.missing_count) > 0 && (
                        <div className="facet-value dim">
                          <span>(missing)</span>
                          <button
                            className="link dim"
                            aria-label={`Pin missing-value group of ${f.display} as evidence`}
                            title="Pin the missing-value group as evidence"
                            onClick={() =>
                              setPinTarget({
                                kind: "group",
                                scope: currentScope(),
                                field: f.display,
                                valueJson: "null",
                                label: "(missing)",
                              })
                            }
                          >
                            📌
                          </button>
                          <span className="num">{fmtCount(f.missing_count)}</span>
                        </div>
                      )}
                      {f.truncated && <p className="dim">more values exist…</p>}
                    </>
                  )}
                </div>
              ))}
              <details>
                <summary>All fields</summary>
                {(catalog?.fields ?? [])
                  .filter((f) => f.queryable)
                  .map((f) => (
                    <div key={f.display} className="field-row">
                      <button
                        className="link"
                        title={
                          f.origin === "attribute"
                            ? `${f.types.join("/")} · ${fmtCount(f.present_count)} present · ~${fmtCount(f.distinct_est)} distinct${f.distinct_is_exact ? "" : " (approx)"}`
                            : "canonical field"
                        }
                        onClick={() =>
                          setQueryText((t) => (t ? `${t} ${f.display}:` : `${f.display}:`))
                        }
                      >
                        {f.display}
                      </button>
                      {f.facetable && facetFields.length < 8 && !facetFields.includes(f.display) && (
                        <button
                          className="link dim"
                          aria-label={`Add facet ${f.display}`}
                          onClick={() => setFacetFields((ff) => [...ff, f.display])}
                        >
                          + facet
                        </button>
                      )}
                    </div>
                  ))}
              </details>
            </div>
          )}
          {panel === "saved" && (
            <div>
              <div className="row">
                <input
                  placeholder="save current as…"
                  aria-label="Saved search name"
                  value={saveName}
                  onChange={(e) => setSaveName(e.target.value)}
                />
                <button
                  disabled={!saveName.trim() || hasErrors}
                  onClick={async () => {
                    try {
                      await api.saveSearch({
                        savedSearchId: null,
                        name: saveName,
                        queryText,
                        datasetIds: selection,
                        timeStrategy: strategy,
                      });
                      setSaveName("");
                      setSavedList(await api.savedSearches());
                    } catch (e) {
                      setError(errorText(e));
                    }
                  }}
                >
                  Save
                </button>
              </div>
              {savedList.map((s) => (
                <div key={s.saved_search_id} className="saved-row">
                  <button
                    className="link"
                    title={s.query_text}
                    onClick={() => {
                      setQueryText(s.query_text);
                      setSelection(s.all_datasets ? [] : s.dataset_ids);
                      setStrategy(s.time_strategy);
                    }}
                  >
                    {s.name}
                  </button>
                  <button
                    className="link dim"
                    aria-label={`Pin saved search ${s.name} as evidence`}
                    title="Pin this saved search as evidence (captured, never substituted)"
                    onClick={() =>
                      setPinTarget({
                        kind: "query",
                        scope: {
                          queryText: s.query_text,
                          datasetIds: s.all_datasets ? [] : s.dataset_ids,
                          timeStrategy: s.time_strategy,
                        },
                        savedSearchId: s.saved_search_id,
                      })
                    }
                  >
                    📌
                  </button>
                  <button
                    className="link dim"
                    aria-label={`Delete saved search ${s.name}`}
                    onClick={async () => {
                      await api.deleteSavedSearch(s.saved_search_id);
                      setSavedList(await api.savedSearches());
                    }}
                  >
                    ×
                  </button>
                </div>
              ))}
              {savedList.length === 0 && <p className="dim">no saved searches</p>}
            </div>
          )}
          {panel === "recent" && (
            <div>
              {recentList.map((r) => (
                <div key={r.recent_id} className="saved-row">
                  <button
                    className="link"
                    title={`run ${r.run_count}× · last ${r.last_run_at}`}
                    onClick={() => {
                      setQueryText(r.query_text);
                      setSelection(r.all_datasets ? [] : r.dataset_ids);
                      setStrategy(r.time_strategy);
                    }}
                  >
                    {clampText(r.query_text || "(match all)", 48)}
                  </button>
                  <button
                    className="link dim"
                    aria-label="Delete recent search"
                    onClick={async () => {
                      await api.deleteRecentSearch(Number(r.recent_id));
                      setRecentList(await api.recentSearches());
                    }}
                  >
                    ×
                  </button>
                </div>
              ))}
              {recentList.length > 0 ? (
                <button
                  className="link dim"
                  onClick={async () => {
                    await api.clearRecentSearches();
                    setRecentList([]);
                  }}
                >
                  clear all (local only)
                </button>
              ) : (
                <p className="dim">no recent searches</p>
              )}
            </div>
          )}
          {panel === "columns" && (
            <div>
              {["timestamp", "severity", "message", "dataset", "trace_id", "record_id"]
                .concat(
                  (catalog?.fields ?? [])
                    .filter((f) => f.origin === "attribute" && f.queryable)
                    .map((f) => f.display),
                )
                .map((c) => (
                  <label key={c} className="col-row">
                    <input
                      type="checkbox"
                      checked={columns.includes(c)}
                      onChange={(e) =>
                        setColumns((cols) =>
                          e.target.checked
                            ? [...cols, c]
                            : cols.filter((x) => x !== c),
                        )
                      }
                    />{" "}
                    {c}
                  </label>
                ))}
              <div className="row">
                <input
                  placeholder="save columns as…"
                  aria-label="Column set name"
                  value={saveName}
                  onChange={(e) => setSaveName(e.target.value)}
                />
                <button
                  disabled={!saveName.trim() || columns.length === 0}
                  onClick={async () => {
                    try {
                      await api.saveColumnSet({
                        columnSetId: null,
                        name: saveName,
                        columns,
                        isDefault: true,
                      });
                      setSaveName("");
                      setColumnSetsList(await api.columnSets());
                    } catch (e) {
                      setError(errorText(e));
                    }
                  }}
                >
                  Save as default
                </button>
              </div>
              {columnSetsList.map((cs) => (
                <div key={cs.column_set_id} className="saved-row">
                  <button className="link" onClick={() => setColumns(cs.columns)}>
                    {cs.name}
                    {cs.is_default ? " (default)" : ""}
                  </button>
                  <button
                    className="link dim"
                    aria-label={`Delete column set ${cs.name}`}
                    onClick={async () => {
                      await api.deleteColumnSet(cs.column_set_id);
                      setColumnSetsList(await api.columnSets());
                    }}
                  >
                    ×
                  </button>
                </div>
              ))}
            </div>
          )}
        </aside>

        <div
          className="event-table"
          ref={tableRef}
          role="grid"
          aria-label="Log events"
          aria-rowcount={rows.length}
          tabIndex={0}
          onKeyDown={onKeyDownTable}
          onScroll={(e) => setScrollTop((e.target as HTMLDivElement).scrollTop)}
          style={{ height: viewH }}
        >
          <div className="table-header" role="row">
            <span className="col col-check" role="columnheader" aria-label="Selection">
              {checked.size > 0 && (
                <button
                  className="link dim"
                  aria-label="Clear selection"
                  onClick={() => setChecked(new Set())}
                >
                  ×
                </button>
              )}
            </span>
            {columns.map((c) => (
              <span key={c} className={`col col-${c === "message" ? "grow" : "fix"}`} role="columnheader">
                {c}
              </span>
            ))}
          </div>
          {rows.length === 0 && phase === "done" && (
            <p className="dim table-empty">no matching records</p>
          )}
          {rows.length === 0 && phase === "idle" && (
            <p className="dim table-empty">
              press Run (or Ctrl+Enter) to execute the query
            </p>
          )}
          <div style={{ height: rows.length * ROW_HEIGHT, position: "relative" }}>
            {slice.map((row, i) => (
              <div
                key={row.record_id + row.dataset_id}
                role="row"
                aria-selected={selected?.record_id === row.record_id}
                className={
                  "table-row " +
                  severityClass(row.severity) +
                  (selected?.record_id === row.record_id ? " selected" : "")
                }
                style={{ top: (first + i) * ROW_HEIGHT, height: ROW_HEIGHT }}
                onClick={() => void openDetail(row)}
              >
                <span
                  className="col col-check"
                  onClick={(e) => e.stopPropagation()}
                >
                  <input
                    type="checkbox"
                    aria-label={`Select event ${row.record_id}`}
                    checked={checked.has(row.record_id)}
                    onChange={(e) => {
                      setChecked((prev) => {
                        const next = new Set(prev);
                        if (e.target.checked) next.add(row.record_id);
                        else next.delete(row.record_id);
                        return next;
                      });
                    }}
                  />
                </span>
                {columns.map((c) => (
                  <span key={c} className={`col col-${c === "message" ? "grow" : "fix"}`}>
                    {clampText(cellValue(row, c), c === "message" ? 300 : 64)}
                  </span>
                ))}
              </div>
            ))}
          </div>
          {loadingMore && <p className="dim table-empty">loading more…</p>}
        </div>

        <aside className="detail-panel">
          {!selected && <p className="dim">select an event to inspect it</p>}
          {selected && (
            <>
              <nav className="side-tabs" role="tablist">
                {(["fields", "attributes", "resource", "provenance", "context"] as const).map(
                  (t) => (
                    <button
                      key={t}
                      role="tab"
                      aria-selected={detailTab === t}
                      className={detailTab === t ? "active" : ""}
                      onClick={() => setDetailTab(t)}
                    >
                      {t}
                    </button>
                  ),
                )}
              </nav>
              <div className="detail-actions">
                <button
                  className="link"
                  title="Pin this event as evidence"
                  onClick={() =>
                    setPinTarget({
                      kind: "event",
                      datasetId: selected.dataset_id,
                      recordId: selected.record_id,
                      displayFields: columns,
                    })
                  }
                >
                  📌 pin event
                </button>
                <button
                  className="link"
                  onClick={() => {
                    if (!detail) return;
                    const parse = (s: string | null) => {
                      try {
                        return s ? JSON.parse(s) : null;
                      } catch {
                        return s;
                      }
                    };
                    const text = JSON.stringify(
                      {
                        ...detail.row,
                        attributes: parse(detail.row.attributes_json),
                        provenance: parse(detail.provenance_json),
                      },
                      (_k, v) => (typeof v === "bigint" ? v.toString() : v),
                      2,
                    ).slice(0, 512 * 1024);
                    void copyText(text);
                  }}
                >
                  copy event JSON (bounded)
                </button>
              </div>
              {detailTab === "fields" && detail && (
                <table className="kv">
                  <tbody>
                    {(
                      [
                        ["timestamp", detail.row.event_time_text ?? "(missing)"],
                        ["severity", detail.row.severity ?? "—"],
                        ["severity.text", detail.row.severity_text ?? "—"],
                        [
                          "severity.number",
                          detail.row.severity_number?.toString() ?? "—",
                        ],
                        ["message", detail.row.message],
                        ["event.name", detail.event_name ?? "—"],
                        ["trace_id", detail.row.trace_id ?? "—"],
                        ["span_id", detail.row.span_id ?? "—"],
                        ["dataset", detail.row.dataset_id],
                        ["record_id", detail.row.record_id],
                      ] as Array<[string, string]>
                    ).map(([k, v]) => (
                      <tr key={k}>
                        <td className="k">
                          <button className="link" onClick={() => void copyText(k)} title="Copy field name">
                            {k}
                          </button>
                        </td>
                        <td className="v">
                          <ExpandableText text={v} />
                          <button className="link dim" onClick={() => void copyText(v)} title="Copy value">
                            ⧉
                          </button>
                        </td>
                      </tr>
                    ))}
                    {detail.timestamp_quality.length > 0 && (
                      <tr>
                        <td className="k">timestamp quality</td>
                        <td className="v">
                          {detail.timestamp_quality.map((q) => (
                            <span key={q} className="badge badge-warn">
                              {q}
                            </span>
                          ))}
                          {detail.original_timestamp_text && (
                            <span className="dim">
                              {" "}
                              original: {clampText(detail.original_timestamp_text, 64)}
                            </span>
                          )}
                        </td>
                      </tr>
                    )}
                    {detail.body_json && (
                      <tr>
                        <td className="k">body (typed)</td>
                        <td className="v">
                          <ExpandableText text={detail.body_json} mono />
                        </td>
                      </tr>
                    )}
                  </tbody>
                </table>
              )}
              {detailTab === "attributes" && detail && (
                <table className="kv">
                  <tbody>
                    {flattenAttrs(detail.row.attributes_json).map(([path, ty, val]) => (
                      <tr key={path}>
                        <td className="k">
                          <span className="dim">{ty}</span> {path}
                        </td>
                        <td className="v">
                          <ExpandableText text={val} />
                          {ty !== "object" && ty !== "array" && ty !== "empty" && (
                            <>
                              <button
                                className="link dim"
                                title="Add as filter"
                                onClick={() => void addPredicate(path, val)}
                              >
                                +
                              </button>
                              <button
                                className="link dim"
                                title="Exclude value"
                                onClick={() => void addPredicate(path, val, true)}
                              >
                                −
                              </button>
                              <button
                                className="link dim"
                                title="Copy value"
                                onClick={() => void copyText(val)}
                              >
                                ⧉
                              </button>
                            </>
                          )}
                        </td>
                      </tr>
                    ))}
                  </tbody>
                </table>
              )}
              {detailTab === "resource" && detail && (
                <div>
                  <h4>Resource</h4>
                  <ExpandableText text={pretty(detail.resource_json)} mono />
                  <h4>Instrumentation scope</h4>
                  <ExpandableText text={pretty(detail.scope_json)} mono />
                </div>
              )}
              {detailTab === "provenance" && detail && (
                <div>
                  <p className="dim">
                    profile {detail.profile_id ?? "—"} v{detail.profile_version ?? "—"} ·
                    parser {detail.parser_id ?? "—"} v{detail.parser_version ?? "—"} ·
                    normalizer v{detail.normalizer_version ?? "—"}
                  </p>
                  <ExpandableText text={pretty(detail.provenance_json)} mono />
                  <button
                    className="link"
                    title="Provenance can contain sensitive paths; copying is explicit"
                    onClick={() => void copyText(detail.provenance_json)}
                  >
                    copy source locator JSON
                  </button>
                </div>
              )}
              {detailTab === "context" && (
                <div>
                  {!context && <p className="dim">loading context…</p>}
                  {context && (
                    <>
                      <p className="dim">
                        source order ±5 around record · file:{" "}
                        {context.source_path ? clampText(context.source_path, 80) : "?"} ·{" "}
                        <span
                          className={
                            context.source_status === "available"
                              ? "badge"
                              : "badge badge-warn"
                          }
                        >
                          {context.source_status === "available"
                            ? "raw source verified"
                            : context.source_status === "changed"
                              ? "source file changed — canonical copies shown"
                              : context.source_status === "missing"
                                ? "source file unavailable — canonical copies shown"
                                : "raw bytes not addressable — canonical copies shown"}
                        </span>
                      </p>
                      {context.raw_excerpt && (
                        <>
                          <h4>Raw record bytes</h4>
                          <ExpandableText text={context.raw_excerpt} mono />
                          <button
                            className="link"
                            onClick={() => context.raw_excerpt && void copyText(context.raw_excerpt)}
                          >
                            copy raw excerpt
                          </button>
                        </>
                      )}
                      <table className="kv context-list">
                        <tbody>
                          {context.records.map((r) => (
                            <tr
                              key={r.record_id}
                              className={
                                r.record_id === context.anchor_record_id
                                  ? "context-anchor"
                                  : ""
                              }
                            >
                              <td className="k num">#{r.record_number?.toString() ?? "?"}</td>
                              <td className="v">
                                <span className={severityClass(r.severity)}>
                                  {r.severity ?? "—"}
                                </span>{" "}
                                {clampText(r.message, 200)}
                              </td>
                            </tr>
                          ))}
                        </tbody>
                      </table>
                    </>
                  )}
                </div>
              )}
            </>
          )}
        </aside>
      </div>

      {pinTarget && (
        <PinDialog
          target={pinTarget}
          onClose={() => setPinTarget(null)}
          onPinned={(msg) => {
            setPinMessage(msg);
            setChecked(new Set());
          }}
        />
      )}

      {exportOpen && (
        <div className="modal" role="dialog" aria-label="Export results">
          <div className="modal-body">
            <h3>Export current result</h3>
            <p className="dim">
              Exports the exact current filter, dataset selection, and time range in
              the table's order. Default limits: 1,000,000 rows / 1 GiB — a bounded
              export is marked truncated, never silently complete.
            </p>
            {!exportRunning && (
              <div className="row">
                <button onClick={() => void doExport("jsonl")}>JSONL (lossless)</button>
                <button onClick={() => void doExport("csv")}>
                  CSV ({columns.length} visible columns)
                </button>
                <button className="link" onClick={() => setExportOpen(false)}>
                  close
                </button>
              </div>
            )}
            {exportRunning && (
              <div>
                <p>
                  status: <strong>{exportRunning.status}</strong>
                  {exportRunning.truncated && (
                    <span className="badge badge-warn">TRUNCATED at limit</span>
                  )}
                </p>
                <p className="dim">
                  {fmtCount(exportRunning.rows_written)} rows ·{" "}
                  {(Number(exportRunning.bytes_written) / 1048576).toFixed(1)} MiB →{" "}
                  {exportRunning.destination}
                </p>
                {exportRunning.error && (
                  <p className="error">[{exportRunning.error.code}] {exportRunning.error.message}</p>
                )}
                <div className="row">
                  {exportRunning.status === "running" && (
                    <button onClick={() => void api.cancelJob(exportRunning.job_id)}>
                      Cancel export
                    </button>
                  )}
                  <button
                    className="link"
                    onClick={() => {
                      setExportRunning(null);
                      setExportOpen(false);
                    }}
                  >
                    close
                  </button>
                </div>
              </div>
            )}
          </div>
        </div>
      )}
    </div>
  );
}

function pretty(json: string | null): string {
  if (!json) return "—";
  try {
    return JSON.stringify(JSON.parse(json), null, 2);
  } catch {
    return json;
  }
}

/** Bounded value display with explicit expansion (hostile-input safe: text
 * nodes only, hard cap even when expanded). */
function ExpandableText({ text, mono }: { text: string; mono?: boolean }) {
  const [open, setOpen] = useState(false);
  const limit = open ? 16_384 : 240;
  const clipped = text.length > limit;
  return (
    <span className={mono ? "mono pre-wrap" : "pre-wrap"}>
      {text.slice(0, limit)}
      {clipped && !open && (
        <button className="link dim" onClick={() => setOpen(true)}>
          … expand ({fmtCount(text.length)} chars)
        </button>
      )}
      {clipped && open && <span className="dim"> … (display capped at 16 KiB)</span>}
      {open && !clipped && (
        <button className="link dim" onClick={() => setOpen(false)}>
          collapse
        </button>
      )}
    </span>
  );
}
