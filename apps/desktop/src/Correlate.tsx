// Correlate view (v0.4 WP4-UX): grouped relationships, behavioural
// signals, and probable neighborhoods.
//
// Every confidence class, evidence strength, and limitation string comes
// from the `corr-rules` / `sig-rules` engines. This view never decides
// that something is a relationship, never promotes a class, and never
// paraphrases a limitation — it displays what the run recorded, and
// where the engine called something an investigative lead it is labelled
// as one here too.

import { useCallback, useEffect, useState } from "react";
import { listen } from "@tauri-apps/api/event";
import { api, errorText } from "./api";
import type {
  AnalysisDefinitionDto,
  AnalysisFinishedDto,
  AnalysisRunDto,
  CorrelationEdgeDto,
  CorrelationGroupDto,
  CorrelationSignalDto,
  ProbableNeighborhoodDto,
} from "./api";
import type { LogRowV2Dto } from "./bindings/LogRowV2Dto";

// `span_id` is deliberately absent: a span ID is unique only within its
// trace, so it cannot select a group. The engine refuses it by name.
const KEYS = [
  ["trace_id", "trace ID (exact)"],
  ["trace_span", "trace + span pair (exact)"],
  ["request_id", "request ID"],
  ["transaction_id", "transaction ID"],
  ["message_id", "message ID"],
  ["entity_id", "entity ID"],
  ["attribute", "attribute (bounded)"],
] as const;

type KeyName = (typeof KEYS)[number][0];

/** Keys whose confidence is `exact`; normalization is refused on these. */
const EXACT_KEYS: readonly string[] = ["trace_id", "trace_span"];

const SIGNALS = [
  ["retry", "retry"],
  ["operational_duplicate", "operational duplicate"],
  ["clock_skew", "clock skew"],
  ["gap", "gap"],
] as const;

const COMPATIBLE_FIELDS = [
  "resource",
  "dataset",
  "source",
  "operation",
  "outcome",
  "event_name",
  "event_type",
  "severity",
] as const;

const PAGE = 50;
const DRILL_LIMIT = 200;
const EDGE_LIMIT = 200;
const SIGNAL_LIMIT = 200;
const NEIGHBOR_LIMIT = 50;
const NS_PER_MS = 1_000_000;
const NS_PER_SEC = 1_000_000_000;

/** Nanoseconds are outside the exact-integer range of a JS number, so
 *  every conversion goes through BigInt and display math stays in
 *  milliseconds. */
function nanosToText(nanos: bigint | null): string {
  if (nanos === null) return "—";
  const ms = Number(nanos / BigInt(NS_PER_MS));
  return new Date(ms).toISOString().replace("T", " ").replace("Z", "");
}

/** A measured delta, rendered without implying direction or cause. */
function fmtDelta(nanos: bigint): string {
  const negative = nanos < 0n;
  const abs = negative ? -nanos : nanos;
  const ns = Number(abs);
  const text =
    ns < 1_000
      ? `${ns} ns`
      : ns < 1_000_000
        ? `${(ns / 1_000).toFixed(1)} µs`
        : ns < NS_PER_SEC
          ? `${(ns / 1_000_000).toFixed(1)} ms`
          : ns < 60 * NS_PER_SEC
            ? `${(ns / NS_PER_SEC).toFixed(2)} s`
            : `${(ns / (60 * NS_PER_SEC)).toFixed(1)} min`;
  return negative ? `−${text}` : text;
}

/** Durations, unlike absolute event times, stay inside the exact-integer
 *  range of a JS number: 2^53 ns is about 104 days, and no tolerance or
 *  threshold here approaches that. Absolute nanosecond timestamps
 *  arriving from Rust are still bigint and must stay that way. */
function secondsToNanos(seconds: number): number {
  return Math.round(seconds * 1000) * NS_PER_MS;
}

function parseList(json: string): string[] {
  try {
    const v: unknown = JSON.parse(json);
    return Array.isArray(v) ? v.map(String) : [];
  } catch {
    return [];
  }
}

export default function Correlate(props: { onBack: () => void }) {
  const [error, setError] = useState("");
  const [defs, setDefs] = useState<AnalysisDefinitionDto[]>([]);
  const [selectedDef, setSelectedDef] = useState<string>("");
  const [runs, setRuns] = useState<AnalysisRunDto[]>([]);
  const [selectedRun, setSelectedRun] = useState<string>("");
  const [staleReason, setStaleReason] = useState<string | null>(null);
  const [runningJob, setRunningJob] = useState<string | null>(null);
  const [page, setPage] = useState(0);
  const [groups, setGroups] = useState<CorrelationGroupDto[]>([]);
  const [detail, setDetail] = useState<CorrelationGroupDto | null>(null);
  const [edges, setEdges] = useState<CorrelationEdgeDto[] | null>(null);
  const [signals, setSignals] = useState<CorrelationSignalDto[] | null>(null);
  const [records, setRecords] = useState<LogRowV2Dto[] | null>(null);
  const [detailTab, setDetailTab] = useState<
    "signals" | "sequence" | "records"
  >("signals");
  const [viewFilter, setViewFilter] = useState<string>("");

  // Neighborhood drill-down.
  const [anchor, setAnchor] = useState<string>("");
  const [hood, setHood] = useState<ProbableNeighborhoodDto | null>(null);
  const [hoodFields, setHoodFields] = useState<string[]>(["operation"]);
  const [hoodSeconds, setHoodSeconds] = useState("2");

  // Creation form.
  const [showCreate, setShowCreate] = useState(false);
  const [newName, setNewName] = useState("");
  const [newQuery, setNewQuery] = useState("");
  const [newKey, setNewKey] = useState<KeyName>("request_id");
  const [newAttribute, setNewAttribute] = useState("");
  const [trim, setTrim] = useState(false);
  const [caseFold, setCaseFold] = useState(false);
  const [stripPrefix, setStripPrefix] = useState("");
  const [pickedSignals, setPickedSignals] = useState<string[]>(
    SIGNALS.map(([id]) => id),
  );
  const [attemptAttribute, setAttemptAttribute] = useState("");
  const [skewToleranceMs, setSkewToleranceMs] = useState("1");
  const [gapSeconds, setGapSeconds] = useState("300");

  const isExactKey = EXACT_KEYS.includes(newKey);

  const refreshDefs = useCallback(async () => {
    try {
      const list = (await api.listAnalysisDefinitions()).filter(
        (d) => d.kind === "correlation",
      );
      setDefs(list);
      if (list.length && !list.some((d) => d.definition_id === selectedDef)) {
        setSelectedDef(list[0].definition_id);
      }
    } catch (e) {
      setError(errorText(e));
    }
  }, [selectedDef]);

  const refreshRuns = useCallback(async () => {
    if (!selectedDef) {
      setRuns([]);
      return;
    }
    try {
      setRuns(await api.listAnalysisRuns(selectedDef));
    } catch (e) {
      setError(errorText(e));
    }
  }, [selectedDef]);

  useEffect(() => {
    void refreshDefs();
  }, [refreshDefs]);

  useEffect(() => {
    void refreshRuns();
    setSelectedRun("");
    setGroups([]);
    setDetail(null);
  }, [refreshRuns]);

  useEffect(() => {
    const un = listen<AnalysisFinishedDto>("analysis-finished", (ev) => {
      setRunningJob((current) =>
        current === ev.payload.job_id ? null : current,
      );
      if (ev.payload.error) setError(errorText(ev.payload.error));
      void refreshRuns();
    });
    return () => {
      void un.then((f) => f());
    };
  }, [refreshRuns]);

  const loadGroups = useCallback(async (runId: string, pageNo: number) => {
    try {
      setGroups(await api.listCorrelationGroups(runId, pageNo * PAGE, PAGE));
      setPage(pageNo);
      setDetail(null);
      setEdges(null);
      setSignals(null);
      setRecords(null);
      setHood(null);
    } catch (e) {
      setError(errorText(e));
    }
  }, []);

  const selectRun = async (run: AnalysisRunDto) => {
    setSelectedRun(run.run_id);
    setStaleReason(null);
    setGroups([]);
    setDetail(null);
    setHood(null);
    if (run.state === "completed" || run.state === "stale") {
      try {
        setStaleReason(await api.checkAnalysisRun(run.run_id));
      } catch {
        // Staleness display is advisory; the listing below still works.
      }
      await loadGroups(run.run_id, 0);
    }
  };

  const openGroup = async (group: CorrelationGroupDto) => {
    setDetail(group);
    setEdges(null);
    setSignals(null);
    setRecords(null);
    setHood(null);
    setDetailTab("signals");
    try {
      setSignals(
        await api.listCorrelationSignals(
          selectedRun,
          group.group_id,
          SIGNAL_LIMIT,
        ),
      );
    } catch (e) {
      // A run from before signals existed is refused by name rather than
      // returning an empty list, so surface the reason instead of a
      // silently empty tab.
      setError(errorText(e));
      setSignals([]);
    }
  };

  const loadSequence = async () => {
    if (!detail) return;
    setDetailTab("sequence");
    if (edges) return;
    try {
      setEdges(
        await api.listCorrelationEdges(selectedRun, detail.group_id, EDGE_LIMIT),
      );
    } catch (e) {
      setError(errorText(e));
    }
  };

  const loadRecords = async () => {
    if (!detail) return;
    setDetailTab("records");
    if (records) return;
    try {
      setRecords(
        await api.correlationRecords(selectedRun, detail.group_id, DRILL_LIMIT),
      );
    } catch (e) {
      setError(errorText(e));
    }
  };

  const runNeighborhood = async (recordId: string) => {
    const seconds = Number(hoodSeconds);
    if (!Number.isFinite(seconds) || seconds <= 0) {
      setError("the neighborhood tolerance must be a positive number of seconds");
      return;
    }
    setError("");
    setAnchor(recordId);
    try {
      setHood(
        await api.probableNeighborhood(
          selectedRun,
          recordId,
          hoodFields,
          secondsToNanos(seconds),
          NEIGHBOR_LIMIT,
        ),
      );
    } catch (e) {
      setHood(null);
      setError(errorText(e));
    }
  };

  const createDefinition = async () => {
    if (!newName.trim()) {
      setError("a definition needs a name");
      return;
    }
    const skewMs = Number(skewToleranceMs);
    const gapSec = Number(gapSeconds);
    if (!Number.isFinite(skewMs) || skewMs < 0) {
      setError("the clock-skew tolerance must be zero or more milliseconds");
      return;
    }
    if (!Number.isFinite(gapSec) || gapSec <= 0) {
      setError(
        "the gap threshold must be positive: every pair of records is at least zero apart, so a non-positive threshold reports every pair",
      );
      return;
    }
    setError("");
    try {
      const def = await api.createCorrelationDefinition({
        name: newName.trim(),
        description: null,
        // Empty means every published log dataset, matching Compare.
        dataset_ids: [],
        query_text: newQuery,
        key: newKey,
        attribute: newKey === "attribute" ? newAttribute.trim() : null,
        // The engine refuses normalization on canonical identifiers; the
        // form disables the controls so the refusal is visible up front.
        trim: isExactKey ? false : trim,
        case_fold: isExactKey ? false : caseFold,
        strip_prefix: isExactKey || !stripPrefix.trim() ? null : stripPrefix.trim(),
        signals: pickedSignals,
        attempt_attribute: attemptAttribute.trim() || null,
        clock_skew_tolerance_nanos: Math.round(skewMs * NS_PER_MS),
        gap_threshold_nanos: secondsToNanos(gapSec),
      });
      setShowCreate(false);
      setSelectedDef(def.definition_id);
      await refreshDefs();
    } catch (e) {
      setError(errorText(e));
    }
  };

  const startRun = async () => {
    if (!selectedDef) return;
    try {
      const started = await api.startCorrelationAnalysis(selectedDef);
      setRunningJob(started.job_id);
      await refreshRuns();
    } catch (e) {
      setError(errorText(e));
    }
  };

  const shown = viewFilter
    ? groups.filter((g) => g.confidence === viewFilter)
    : groups;

  return (
    <div className="explorer">
      <div className="explorer-header">
        <button className="link" onClick={props.onBack}>
          ← back
        </button>
        <h2 style={{ margin: 0 }}>Correlate</h2>
        <span className="spacer" />
        <button onClick={() => setShowCreate((v) => !v)}>
          {showCreate ? "cancel" : "new definition"}
        </button>
      </div>

      {error && <div className="error">{error}</div>}

      {showCreate && (
        <section className="case-form">
          <div className="row">
            <label>
              name
              <input
                value={newName}
                onChange={(e) => setNewName(e.target.value)}
                placeholder="correlate checkout requests"
              />
            </label>
            <label>
              key
              <select
                value={newKey}
                onChange={(e) => setNewKey(e.target.value as KeyName)}
              >
                {KEYS.map(([id, label]) => (
                  <option key={id} value={id}>
                    {label}
                  </option>
                ))}
              </select>
            </label>
            {newKey === "attribute" && (
              <label>
                attribute
                <input
                  value={newAttribute}
                  onChange={(e) => setNewAttribute(e.target.value)}
                  placeholder="session.id"
                />
              </label>
            )}
          </div>

          <p className="dim">
            There is no <code>span_id</code> key: a span ID is unique only
            within its trace, so it cannot select a group. Use the trace + span
            pair to group the records of one span.
          </p>

          <div className="row">
            <span className="dim">query (optional)</span>
            <input
              className="mono"
              style={{ flex: 1, minWidth: 240 }}
              value={newQuery}
              onChange={(e) => setNewQuery(e.target.value)}
              placeholder='severity="ERROR"'
            />
          </div>

          <fieldset className="case-form">
            <legend className="dim">normalization</legend>
            {isExactKey ? (
              <p className="dim">
                Not available for canonical telemetry identifiers. They are
                validated and normalized at ingest, and altering them here would
                make an exact relationship mean something else.
              </p>
            ) : (
              <div className="row">
                <label className="dim">
                  <input
                    type="checkbox"
                    checked={trim}
                    onChange={(e) => setTrim(e.target.checked)}
                  />{" "}
                  trim
                </label>
                <label className="dim">
                  <input
                    type="checkbox"
                    checked={caseFold}
                    onChange={(e) => setCaseFold(e.target.checked)}
                  />{" "}
                  case fold
                </label>
                <label>
                  strip prefix
                  <input
                    value={stripPrefix}
                    onChange={(e) => setStripPrefix(e.target.value)}
                    placeholder="req-"
                  />
                </label>
              </div>
            )}
          </fieldset>

          <fieldset className="case-form">
            <legend className="dim">signals</legend>
            <div className="row">
              {SIGNALS.map(([id, label]) => (
                <label key={id} className="dim">
                  <input
                    type="checkbox"
                    checked={pickedSignals.includes(id)}
                    onChange={(e) =>
                      setPickedSignals((prev) =>
                        e.target.checked
                          ? [...prev, id]
                          : prev.filter((s) => s !== id),
                      )
                    }
                  />{" "}
                  {label}
                </label>
              ))}
            </div>
            <div className="row">
              <label>
                attempt attribute
                <input
                  value={attemptAttribute}
                  onChange={(e) => setAttemptAttribute(e.target.value)}
                  placeholder="attempt"
                />
              </label>
              <label>
                clock-skew tolerance (ms)
                <input
                  value={skewToleranceMs}
                  onChange={(e) => setSkewToleranceMs(e.target.value)}
                />
              </label>
              <label>
                gap threshold (s)
                <input
                  value={gapSeconds}
                  onChange={(e) => setGapSeconds(e.target.value)}
                />
              </label>
            </div>
            <p className="dim">
              Without an attempt attribute a retry can never be reported as
              documented — only the source counting its own attempts makes a
              retry an observation rather than a reading.
            </p>
          </fieldset>

          <div className="row">
            <button onClick={() => void createDefinition()}>create</button>
          </div>
        </section>
      )}

      <div className="row">
        <label>
          definition
          <select
            value={selectedDef}
            onChange={(e) => setSelectedDef(e.target.value)}
          >
            {defs.length === 0 && <option value="">(none yet)</option>}
            {defs.map((d) => (
              <option key={d.definition_id} value={d.definition_id}>
                {d.name}
              </option>
            ))}
          </select>
        </label>
        <button
          onClick={() => void startRun()}
          disabled={!selectedDef || runningJob !== null}
        >
          {runningJob ? "running…" : "run"}
        </button>
      </div>

      {runs.length > 0 && (
        <table>
          <thead>
            <tr>
              <th>run</th>
              <th>state</th>
              <th className="num">groups</th>
              <th className="num">signals</th>
              <th>started</th>
            </tr>
          </thead>
          <tbody>
            {runs.map((r) => {
              const counts = (() => {
                try {
                  return JSON.parse(r.counts_json ?? "{}") as Record<
                    string,
                    unknown
                  >;
                } catch {
                  return {};
                }
              })();
              return (
                <tr
                  key={r.run_id}
                  className={r.run_id === selectedRun ? "selected" : ""}
                  onClick={() => void selectRun(r)}
                >
                  <td className="mono">{r.run_id.slice(0, 12)}</td>
                  <td>
                    <span className={`status-chip status-${r.state}`}>
                      {r.state}
                    </span>
                  </td>
                  <td className="num">{String(counts.groups ?? "—")}</td>
                  <td className="num">{String(counts.signals ?? "—")}</td>
                  <td className="dim">{r.started_at ?? "—"}</td>
                </tr>
              );
            })}
          </tbody>
        </table>
      )}

      {staleReason && (
        <p className="error">
          This run is stale: {staleReason}. Listings still work, but drill-down
          is refused — the answer would silently change.
        </p>
      )}

      {selectedRun && (
        <div className="row">
          <span className="dim">confidence</span>
          <select
            value={viewFilter}
            onChange={(e) => setViewFilter(e.target.value)}
          >
            <option value="">all (this page)</option>
            <option value="exact">exact</option>
            <option value="correlated">correlated</option>
            <option value="probable">probable</option>
          </select>
          <span className="spacer" />
          <button
            onClick={() => void loadGroups(selectedRun, page - 1)}
            disabled={page === 0}
          >
            ← prev
          </button>
          <span className="dim">page {page + 1}</span>
          <button
            onClick={() => void loadGroups(selectedRun, page + 1)}
            disabled={groups.length < PAGE}
          >
            next →
          </button>
        </div>
      )}

      <div className="explorer-main">
        {selectedRun && (
          <div className="event-table" style={{ minHeight: 240 }}>
            <div className="table-header" role="row">
              <span className="col col-fix">key</span>
              <span className="col col-fix">confidence</span>
              <span className="col col-fix">events</span>
              <span className="col col-grow">first → last</span>
            </div>
            {shown.length === 0 && (
              <p className="dim table-empty">
                {groups.length === 0
                  ? "no groups in this run"
                  : "no groups of that confidence on this page"}
              </p>
            )}
            {shown.map((g) => (
              <div
                key={g.group_id}
                role="row"
                className={
                  "table-row" +
                  (detail?.group_id === g.group_id ? " selected" : "")
                }
                style={{ position: "relative", height: "auto" }}
                onClick={() => void openGroup(g)}
              >
                <span className="col col-fix mono">{g.key_value}</span>
                <span className="col col-fix">
                  <span className={`kind-chip conf-${g.confidence}`}>
                    {g.confidence}
                  </span>
                </span>
                <span className="col col-fix">
                  {g.event_count}
                  {g.undated_count > 0 && (
                    <span className="dim"> +{g.undated_count} undated</span>
                  )}
                  {g.truncated_count > 0 && (
                    <span className="dim"> +{g.truncated_count} over limit</span>
                  )}
                </span>
                <span className="col col-grow dim">
                  {nanosToText(g.first_event_time)} →{" "}
                  {nanosToText(g.last_event_time)}
                </span>
              </div>
            ))}
          </div>
        )}

        {detail && (
          <aside className="detail-panel">
            <h4 className="mono" style={{ margin: "0 0 4px" }}>
              {detail.key_value}
            </h4>
            <p className="dim">{detail.reason}</p>
            <p className="dim">
              rule {detail.rule_id} v{String(detail.rule_version)} ·{" "}
              {detail.event_count} sequenced
              {detail.undated_count > 0 &&
                ` · ${detail.undated_count} undated (counted, never ordered)`}
              {detail.truncated_count > 0 &&
                ` · ${detail.truncated_count} beyond the group limit`}
            </p>

            <nav className="side-tabs" role="tablist">
              <button
                className={detailTab === "signals" ? "active" : ""}
                onClick={() => setDetailTab("signals")}
              >
                signals
              </button>
              <button
                className={detailTab === "sequence" ? "active" : ""}
                onClick={() => void loadSequence()}
              >
                sequence
              </button>
              <button
                className={detailTab === "records" ? "active" : ""}
                onClick={() => void loadRecords()}
              >
                records
              </button>
            </nav>

            {detailTab === "signals" && (
              <SignalList signals={signals} />
            )}

            {detailTab === "sequence" && (
              <>
                <p className="dim">
                  Consecutive records only. Adjacency in time is not causation,
                  and every gap is reported as measured.
                </p>
                {edges === null && <p className="dim">loading…</p>}
                {edges?.length === 0 && (
                  <p className="dim">no ordered pairs in this group</p>
                )}
                <table className="kv">
                  <tbody>
                    {edges?.map((e) => (
                      <tr key={e.edge_id}>
                        <td className="k mono">
                          {e.from_record_id.slice(0, 10)} →{" "}
                          {e.to_record_id.slice(0, 10)}
                        </td>
                        <td className="v">{fmtDelta(e.delta_nanos)}</td>
                      </tr>
                    ))}
                  </tbody>
                </table>
              </>
            )}

            {detailTab === "records" && (
              <>
                {records === null && <p className="dim">loading…</p>}
                {records?.length === 0 && <p className="dim">no records</p>}
                <table className="kv">
                  <tbody>
                    {records?.map((r) => (
                      <tr key={r.record_id}>
                        <td className="k">
                          <button
                            className="link"
                            title="probable neighborhood around this record"
                            onClick={() => void runNeighborhood(r.record_id)}
                          >
                            anchor
                          </button>
                        </td>
                        <td className="v">
                          <div className="dim mono">
                            {r.event_time_text ?? "—"} · {r.severity ?? "—"}
                          </div>
                          <div className="pre-wrap">{r.message}</div>
                        </td>
                      </tr>
                    ))}
                  </tbody>
                </table>
              </>
            )}
          </aside>
        )}
      </div>

      {detail && detailTab === "records" && (
        <section>
          <div className="row">
            <strong>Probable neighborhood</strong>
            <span className="dim">tolerance (s)</span>
            <input
              style={{ width: 70 }}
              value={hoodSeconds}
              onChange={(e) => setHoodSeconds(e.target.value)}
            />
            <span className="dim">must share</span>
            {COMPATIBLE_FIELDS.map((f) => (
              <label key={f} className="dim">
                <input
                  type="checkbox"
                  checked={hoodFields.includes(f)}
                  onChange={(e) =>
                    setHoodFields((prev) =>
                      e.target.checked
                        ? [...prev, f]
                        : prev.filter((x) => x !== f),
                    )
                  }
                />{" "}
                {f}
              </label>
            ))}
            <button
              onClick={() => void runNeighborhood(anchor)}
              disabled={!anchor}
            >
              re-run
            </button>
          </div>
          {!anchor && (
            <p className="dim">
              Pick an anchor from the records tab. A neighborhood is defined by
              distance in time, so an undated record cannot anchor one.
            </p>
          )}
          {hood && <Neighborhood hood={hood} />}
        </section>
      )}
    </div>
  );
}

/** Signals, grouped so a documented observation is never displayed the
 *  same way as an investigative lead. */
function SignalList(props: { signals: CorrelationSignalDto[] | null }) {
  const { signals } = props;
  if (signals === null) return <p className="dim">loading…</p>;
  if (signals.length === 0)
    return (
      <p className="dim">
        no signals in this group under the configured rules
      </p>
    );
  return (
    <ul className="case-cards">
      {signals.map((s) => {
        const matched = parseList(s.matched_json);
        const missing = parseList(s.missing_json);
        return (
          <li key={s.signal_id} className="case-card">
            <div className="row">
              <span className={`kind-chip sig-${s.kind}`}>
                {s.kind.replace(/_/g, " ")}
              </span>
              <span className={`status-chip strength-${s.strength}`}>
                {s.strength}
              </span>
              {s.investigative_lead && (
                <span className="badge badge-warn">investigative lead</span>
              )}
              <span className="spacer" />
              <span className="dim mono">{fmtDelta(s.delta_nanos)}</span>
            </div>
            <div className="dim">
              matched: {matched.length ? matched.join(", ") : "nothing"}
              {missing.length > 0 && <> · absent: {missing.join(", ")}</>}
              {s.tolerance_nanos !== null && (
                <> · tolerance {fmtDelta(s.tolerance_nanos)}</>
              )}
            </div>
            <div className="pre-wrap">{s.reason}</div>
          </li>
        );
      })}
    </ul>
  );
}

/** Everything gate 21 requires a probable relationship to expose is a
 *  field of the DTO, so this renders them rather than deriving them. */
function Neighborhood(props: { hood: ProbableNeighborhoodDto }) {
  const { hood } = props;
  return (
    <>
      <div className="row">
        <span className={`kind-chip conf-${hood.confidence}`}>
          {hood.confidence}
        </span>
        <span className="badge badge-warn">investigative lead</span>
        <span className="dim">
          rule {hood.rule_id} v{String(hood.rule_version)}
        </span>
        <span className="dim">anchor time {hood.anchor_time_quality}</span>
        <span className="dim">
          {hood.admitted} admitted
          {hood.truncated > 0 && `, ${hood.truncated} dropped by the limit`} ·{" "}
          {hood.scanned} scanned
        </span>
      </div>
      <p className="dim">constraint: {hood.constraints}</p>
      <p className="dim">{hood.reason}</p>
      {hood.neighbors.length === 0 ? (
        <p className="dim">no records met the rule</p>
      ) : (
        <table>
          <thead>
            <tr>
              <th>record</th>
              <th className="num">distance</th>
              <th>shared</th>
              <th>timestamp quality</th>
            </tr>
          </thead>
          <tbody>
            {hood.neighbors.map((n) => (
              <tr key={n.record_id}>
                <td className="mono">{n.record_id.slice(0, 16)}</td>
                <td className="num">{fmtDelta(n.delta_nanos)}</td>
                <td className="dim">
                  {n.matched_fields.length
                    ? n.matched_fields.join(", ")
                    : "time proximity alone"}
                </td>
                <td className="dim">{n.time_quality}</td>
              </tr>
            ))}
          </tbody>
        </table>
      )}
    </>
  );
}
