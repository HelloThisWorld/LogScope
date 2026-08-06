// Compare view (v0.4 WP3-UX): explicit baseline-versus-suspect window
// comparison. Every classification, rate, and threshold decision comes
// from the `cmp-rules` engine — this view never classifies, never
// re-sorts stored results, and never turns a bounded page into a claim
// about the whole distribution.

import { useCallback, useEffect, useState } from "react";
import { listen } from "@tauri-apps/api/event";
import { api, errorText } from "./api";
import type {
  AnalysisDefinitionDto,
  AnalysisFinishedDto,
  AnalysisRunDto,
  ComparisonResultDto,
} from "./api";
import type { LogRowV2Dto } from "./bindings/LogRowV2Dto";

const DIMENSIONS = [
  ["message_pattern", "message templates"],
  ["stack_fingerprint", "stack fingerprints"],
  ["severity", "severity"],
  ["resource", "resource / service"],
  ["operation", "operation"],
  ["outcome", "outcome"],
  ["event_name", "event name"],
  ["attribute", "attribute (bounded)"],
] as const;

type Dimension = (typeof DIMENSIONS)[number][0];

const CLASSIFICATIONS = [
  "new",
  "disappeared",
  "increased",
  "decreased",
  "unchanged",
  "insufficient_data",
] as const;

const PAGE = 50;
const DRILL_LIMIT = 200;
const NS_PER_MS = 1_000_000;
const WIDE_WINDOW_MS = 7 * 24 * 3600 * 1000;

// UTC nanos exceed the exact-integer range of a JS number, so window
// math stays in milliseconds and the nanosecond bound is built in
// BigInt space — a typed 10:00:00 is stored as exactly 10:00:00.
function msToNanos(ms: number): bigint {
  return BigInt(ms) * BigInt(NS_PER_MS);
}

/** "yyyy-MM-ddTHH:mm:ss" (treated as UTC) → epoch milliseconds. */
function textToMillis(text: string): number | null {
  const t = text.trim();
  if (!t) return null;
  const ms = Date.parse(t.endsWith("Z") ? t : `${t}Z`);
  return Number.isNaN(ms) ? null : ms;
}

function millisToText(ms: number): string {
  return new Date(ms).toISOString().slice(0, 19);
}

/** Nanos read back from stored JSON: exact to the millisecond after
 *  rounding, which is all this display needs. */
function nanosToText(nanos: number): string {
  return millisToText(Math.round(nanos / NS_PER_MS));
}

function fmtDuration(ms: number): string {
  const seconds = Math.round(ms / 1000);
  if (seconds % 86400 === 0) return `${seconds / 86400} d`;
  if (seconds % 3600 === 0) return `${seconds / 3600} h`;
  if (seconds % 60 === 0) return `${seconds / 60} min`;
  return `${seconds} s`;
}

/** Basis points as stored (a decimal string or "undefined") → text. */
function fmtRateChange(bp: string): string {
  if (bp === "undefined") return "undefined (zero baseline)";
  const sign = bp.startsWith("-") ? "" : "+";
  const digits = bp.startsWith("-") ? bp.slice(1) : bp;
  const padded = digits.padStart(3, "0");
  const whole = padded.slice(0, -2);
  const frac = padded.slice(-2);
  return `${sign}${bp.startsWith("-") ? "-" : ""}${whole}.${frac} %`;
}

type Calculation = {
  baseline_duration_nanos: number;
  suspect_duration_nanos: number;
  rule_id: string;
  rule_version: number;
};

export default function Compare(props: { onBack: () => void }) {
  const [error, setError] = useState("");
  const [defs, setDefs] = useState<AnalysisDefinitionDto[]>([]);
  const [selectedDef, setSelectedDef] = useState<string>("");
  const [runs, setRuns] = useState<AnalysisRunDto[]>([]);
  const [selectedRun, setSelectedRun] = useState<string>("");
  const [staleReason, setStaleReason] = useState<string | null>(null);
  const [runningJob, setRunningJob] = useState<string | null>(null);
  const [page, setPage] = useState(0);
  const [results, setResults] = useState<ComparisonResultDto[]>([]);
  const [detail, setDetail] = useState<ComparisonResultDto | null>(null);
  const [records, setRecords] = useState<LogRowV2Dto[] | null>(null);
  const [recordSide, setRecordSide] = useState<"baseline" | "suspect">(
    "baseline",
  );
  const [viewFilter, setViewFilter] = useState<string>("");

  // Creation form.
  const [showCreate, setShowCreate] = useState(false);
  const [newName, setNewName] = useState("");
  const [newQuery, setNewQuery] = useState("");
  const [dimension, setDimension] = useState<Dimension>("message_pattern");
  const [attribute, setAttribute] = useState("");
  const [stackField, setStackField] = useState("");
  const [baselineStart, setBaselineStart] = useState("");
  const [baselineEnd, setBaselineEnd] = useState("");
  const [suspectStart, setSuspectStart] = useState("");
  const [suspectEnd, setSuspectEnd] = useState("");
  const [topK, setTopK] = useState("100");
  const [minCount, setMinCount] = useState("5");
  const [relThresholdBp, setRelThresholdBp] = useState("5000");
  const [absThreshold, setAbsThreshold] = useState("10");

  const refreshDefs = useCallback(async () => {
    try {
      const list = (await api.listAnalysisDefinitions()).filter(
        (d) => d.kind === "comparison",
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
    setResults([]);
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

  const loadResults = useCallback(async (runId: string, pageNo: number) => {
    try {
      setResults(await api.listComparisonResults(runId, pageNo * PAGE, PAGE));
      setPage(pageNo);
      setDetail(null);
      setRecords(null);
    } catch (e) {
      setError(errorText(e));
    }
  }, []);

  const selectRun = async (run: AnalysisRunDto) => {
    setSelectedRun(run.run_id);
    setStaleReason(null);
    setResults([]);
    setDetail(null);
    setRecords(null);
    if (run.state === "completed" || run.state === "stale") {
      try {
        setStaleReason(await api.checkAnalysisRun(run.run_id));
      } catch {
        // staleness display is advisory; listing below still works
      }
      await loadResults(run.run_id, 0);
    }
  };

  const windows = {
    bs: textToMillis(baselineStart),
    be: textToMillis(baselineEnd),
    ss: textToMillis(suspectStart),
    se: textToMillis(suspectEnd),
  };
  const allBounds =
    windows.bs !== null &&
    windows.be !== null &&
    windows.ss !== null &&
    windows.se !== null;

  // Local validation mirrors the engine's refusals so the reason is
  // visible before a definition exists; the engine refuses again anyway.
  const localProblem = (() => {
    if (!allBounds) return "all four window bounds are required";
    const { bs, be, ss, se } = windows as Record<string, number>;
    if (be <= bs) return "the baseline window must end after it starts";
    if (se <= ss) return "the suspect window must end after it starts";
    if (bs < se && ss < be)
      return "the windows overlap; comparison windows must be disjoint";
    return "";
  })();

  const durations = allBounds
    ? {
        baseline: (windows.be as number) - (windows.bs as number),
        suspect: (windows.se as number) - (windows.ss as number),
      }
    : null;

  /** Fills the baseline with the equal-length interval immediately
   *  before the suspect window — a preset that resolves to concrete
   *  visible bounds, never a hidden relative rule. */
  const presetPrecedingWindow = () => {
    const ss = textToMillis(suspectStart);
    const se = textToMillis(suspectEnd);
    if (ss === null || se === null || se <= ss) {
      setError("set a valid suspect window first");
      return;
    }
    setError("");
    setBaselineStart(millisToText(ss - (se - ss)));
    setBaselineEnd(millisToText(ss));
  };

  const swapWindows = () => {
    setBaselineStart(suspectStart);
    setBaselineEnd(suspectEnd);
    setSuspectStart(baselineStart);
    setSuspectEnd(baselineEnd);
    setNewName((n) => (n ? `${n} (swapped)` : ""));
    setShowCreate(true);
  };

  const doCreate = async () => {
    setError("");
    if (localProblem) {
      setError(localProblem);
      return;
    }
    const thresholds: Record<string, number> = {};
    const putInt = (key: string, text: string) => {
      const n = Number(text);
      if (Number.isFinite(n) && Number.isInteger(n) && n >= 0)
        thresholds[key] = n;
    };
    putInt("min_count", minCount);
    putInt("min_new_count", minCount);
    putInt("min_gone_count", minCount);
    putInt("rel_threshold_bp", relThresholdBp);
    putInt("abs_threshold", absThreshold);
    try {
      const created = await api.createComparisonDefinition({
        name: newName,
        description: null,
        dataset_ids: [],
        query_text: newQuery,
        dimension,
        attribute: dimension === "attribute" ? attribute : null,
        stack_field: dimension === "stack_fingerprint" ? stackField : null,
        baseline_start: msToNanos(windows.bs as number),
        baseline_end: msToNanos(windows.be as number),
        suspect_start: msToNanos(windows.ss as number),
        suspect_end: msToNanos(windows.se as number),
        top_k: Number(topK) || null,
        thresholds_json: JSON.stringify(thresholds),
        masking_profile_json: "{}",
      });
      setShowCreate(false);
      setNewName("");
      await refreshDefs();
      setSelectedDef(created.definition_id);
    } catch (e) {
      setError(errorText(e));
    }
  };

  const doRun = async () => {
    setError("");
    try {
      const started = await api.startComparisonAnalysis(selectedDef);
      setRunningJob(started.job_id);
      await refreshRuns();
    } catch (e) {
      setError(errorText(e));
    }
  };

  const doCancel = async () => {
    if (runningJob) {
      try {
        await api.cancelJob(runningJob);
      } catch (e) {
        setError(errorText(e));
      }
    }
  };

  const doDrill = async (r: ComparisonResultDto, side: "baseline" | "suspect") => {
    setError("");
    setRecords(null);
    setRecordSide(side);
    try {
      setRecords(
        await api.comparisonRecords(selectedRun, r.key, side, DRILL_LIMIT),
      );
    } catch (e) {
      setError(errorText(e));
    }
  };

  const selDef = defs.find((d) => d.definition_id === selectedDef);
  const selDefConfig: Record<string, unknown> = selDef
    ? JSON.parse(selDef.config_json || "{}")
    : {};
  const selRun = runs.find((r) => r.run_id === selectedRun);
  const runCounts: Record<string, number | boolean> = selRun
    ? JSON.parse(selRun.counts_json || "{}")
    : {};
  const runManifest: Record<string, unknown> = selRun?.manifest_json
    ? JSON.parse(selRun.manifest_json)
    : {};
  const remainder = (runManifest.remainder ?? null) as {
    keys: number;
    baseline_count: number;
    suspect_count: number;
  } | null;

  const shown = viewFilter
    ? results.filter((r) => r.classification === viewFilter)
    : results;
  const pageMaxCount = shown.reduce(
    (m, r) => Math.max(m, r.baseline_count, r.suspect_count),
    0,
  );

  const runRow = (r: AnalysisRunDto) => {
    const counts = JSON.parse(r.counts_json || "{}");
    const failed = r.error_json ? JSON.parse(r.error_json) : null;
    return (
      <tr
        key={r.run_id}
        className={r.run_id === selectedRun ? "selected" : ""}
        onClick={() => void selectRun(r)}
      >
        <td>{r.state}</td>
        <td className="dim">{r.started_at.slice(0, 19)}</td>
        <td>
          {counts.baseline_accepted ?? "—"} / {counts.suspect_accepted ?? "—"}
        </td>
        <td className="dim">
          {r.state === "failed" || r.state === "cancelled"
            ? (failed?.message ?? failed?.code ?? "")
            : (r.invalidation_reason ?? "")}
        </td>
      </tr>
    );
  };

  return (
    <div>
      <div className="row">
        <button onClick={props.onBack}>← Back</button>
        <h2>Compare</h2>
        {runningJob && (
          <>
            <span className="status">comparison running…</span>
            <button onClick={() => void doCancel()}>Cancel run</button>
          </>
        )}
      </div>
      {error && <div className="error">{error}</div>}

      <section>
        <h3>Comparison definitions</h3>
        {defs.length === 0 && !showCreate && (
          <p className="dim">
            No comparisons yet — define an explicit baseline window and an
            explicit suspect window to compare.
          </p>
        )}
        <div className="row">
          <select
            aria-label="comparison definition"
            value={selectedDef}
            onChange={(e) => setSelectedDef(e.target.value)}
          >
            {defs.map((d) => (
              <option key={d.definition_id} value={d.definition_id}>
                {d.name} (rev {d.revision.toString()})
              </option>
            ))}
          </select>
          <button
            onClick={() => void doRun()}
            disabled={!selectedDef || !!runningJob}
          >
            Run comparison
          </button>
          <button onClick={() => setShowCreate(!showCreate)}>
            {showCreate ? "Close form" : "New comparison…"}
          </button>
        </div>
        {selDef && (
          <p className="dim">
            dimension: {String(selDefConfig.dimension ?? "—")}
            {" · baseline: "}
            {nanosToText(Number(selDefConfig.baseline_start))} →{" "}
            {nanosToText(Number(selDefConfig.baseline_end))}
            {" · suspect: "}
            {nanosToText(Number(selDefConfig.suspect_start))} →{" "}
            {nanosToText(Number(selDefConfig.suspect_end))}
            {" · rule: "}
            {selDef.algorithm_id} v{selDef.algorithm_version.toString()}
            {" · query: "}
            {selDef.query_text || "(all records)"}
          </p>
        )}
        {showCreate && (
          <div className="panel">
            <div className="row">
              <input
                placeholder="name"
                value={newName}
                onChange={(e) => setNewName(e.target.value)}
              />
              <input
                placeholder="query (optional, e.g. severity:ERROR)"
                value={newQuery}
                onChange={(e) => setNewQuery(e.target.value)}
              />
              <label>
                dimension{" "}
                <select
                  value={dimension}
                  onChange={(e) => setDimension(e.target.value as Dimension)}
                >
                  {DIMENSIONS.map(([value, label]) => (
                    <option key={value} value={value}>
                      {label}
                    </option>
                  ))}
                </select>
              </label>
              {dimension === "attribute" && (
                <input
                  placeholder="attribute name"
                  value={attribute}
                  onChange={(e) => setAttribute(e.target.value)}
                />
              )}
              {dimension === "stack_fingerprint" && (
                <input
                  placeholder="attribute holding the stack text"
                  value={stackField}
                  onChange={(e) => setStackField(e.target.value)}
                />
              )}
            </div>
            <div className="row wrap">
              <label>
                baseline start (UTC){" "}
                <input
                  placeholder="2024-06-01T10:00:00"
                  value={baselineStart}
                  onChange={(e) => setBaselineStart(e.target.value)}
                />
              </label>
              <label>
                end{" "}
                <input
                  placeholder="2024-06-01T11:00:00"
                  value={baselineEnd}
                  onChange={(e) => setBaselineEnd(e.target.value)}
                />
              </label>
              <label>
                suspect start (UTC){" "}
                <input
                  placeholder="2024-06-01T11:00:00"
                  value={suspectStart}
                  onChange={(e) => setSuspectStart(e.target.value)}
                />
              </label>
              <label>
                end{" "}
                <input
                  placeholder="2024-06-01T12:00:00"
                  value={suspectEnd}
                  onChange={(e) => setSuspectEnd(e.target.value)}
                />
              </label>
              <button onClick={presetPrecedingWindow}>
                Baseline = preceding equal window
              </button>
              <button onClick={swapWindows}>Swap windows</button>
            </div>
            <div className="row wrap">
              <label>
                top-K{" "}
                <input
                  size={5}
                  value={topK}
                  onChange={(e) => setTopK(e.target.value)}
                />
              </label>
              <label>
                min count{" "}
                <input
                  size={4}
                  value={minCount}
                  onChange={(e) => setMinCount(e.target.value)}
                />
              </label>
              <label>
                relative threshold (basis points, 5000 = 50 %){" "}
                <input
                  size={6}
                  value={relThresholdBp}
                  onChange={(e) => setRelThresholdBp(e.target.value)}
                />
              </label>
              <label>
                absolute count threshold{" "}
                <input
                  size={5}
                  value={absThreshold}
                  onChange={(e) => setAbsThreshold(e.target.value)}
                />
              </label>
            </div>
            {durations && !localProblem && (
              <p className="dim">
                baseline {fmtDuration(durations.baseline)} · suspect{" "}
                {fmtDuration(durations.suspect)}
                {durations.baseline !== durations.suspect
                  ? " — different lengths: increase/decrease is decided on rates, not raw counts"
                  : ""}
                {durations.baseline > WIDE_WINDOW_MS ||
                durations.suspect > WIDE_WINDOW_MS
                  ? " · wide windows scan more records; the run is bounded and cancellable"
                  : ""}
                {" · at most "}
                {topK} keys are stored; the remainder is counted in the run
                manifest, never dropped
              </p>
            )}
            {localProblem && <p className="error">{localProblem}</p>}
            <button
              onClick={() => void doCreate()}
              disabled={!newName.trim() || !!localProblem}
            >
              Create comparison
            </button>
          </div>
        )}
      </section>

      <section>
        <h3>Runs</h3>
        {runs.length === 0 ? (
          <p className="dim">No runs for this comparison yet.</p>
        ) : (
          <table>
            <thead>
              <tr>
                <th>State</th>
                <th>Started</th>
                <th>Accepted (baseline / suspect)</th>
                <th>Note</th>
              </tr>
            </thead>
            <tbody>{runs.map(runRow)}</tbody>
          </table>
        )}
        {selRun && selRun.state === "completed" && (
          <p className="dim">
            excluded — missing field: {String(runCounts.excluded_missing_field ?? 0)}
            {" · unparseable stacks: "}
            {String(runCounts.stack_malformed ?? 0)}
            {" · without a timestamp (outside both windows): "}
            {String(runCounts.untimestamped_excluded ?? 0)}
            {runCounts.keys_truncated
              ? ` · key limit reached: ${String(runCounts.excluded_over_key_limit ?? 0)} occurrences not keyed`
              : ""}
            {remainder && remainder.keys > 0
              ? ` · beyond top-K: ${remainder.keys} keys (${remainder.baseline_count} baseline / ${remainder.suspect_count} suspect occurrences)`
              : ""}
          </p>
        )}
        {staleReason && (
          <div className="error">
            Run is stale: {staleReason}. Results stay readable; drill-down is
            refused — re-run the comparison for current data.
          </div>
        )}
      </section>

      {selectedRun && results.length === 0 && (
        <p className="dim">
          {selRun?.state === "completed"
            ? "The run completed with zero comparable keys in scope — every record was excluded (see the counts above)."
            : "Select a completed run to see its results."}
        </p>
      )}

      {results.length > 0 && (
        <section>
          <h3>
            Results{" "}
            <span className="dim">
              (page {page + 1}, stored order: combined count desc, then key)
            </span>
          </h3>
          <div className="row wrap">
            <button
              disabled={page === 0}
              onClick={() => void loadResults(selectedRun, page - 1)}
            >
              ← Prev
            </button>
            <button
              disabled={results.length < PAGE}
              onClick={() => void loadResults(selectedRun, page + 1)}
            >
              Next →
            </button>
            <label>
              view filter (this page only){" "}
              <select
                value={viewFilter}
                onChange={(e) => setViewFilter(e.target.value)}
              >
                <option value="">all classifications</option>
                {CLASSIFICATIONS.map((c) => (
                  <option key={c} value={c}>
                    {c}
                  </option>
                ))}
              </select>
            </label>
            {viewFilter && (
              <span className="dim">
                showing {shown.length} of {results.length} rows on this page —
                a display filter, not a re-run
              </span>
            )}
          </div>
          <table>
            <thead>
              <tr>
                <th>Key</th>
                <th>Classification</th>
                <th>Baseline</th>
                <th>Suspect</th>
                <th>Counts (bounded to this page)</th>
                <th>Rate change</th>
              </tr>
            </thead>
            <tbody>
              {shown.map((r) => (
                <tr
                  key={r.result_id}
                  className={detail?.result_id === r.result_id ? "selected" : ""}
                  onClick={() => {
                    setDetail(r);
                    setRecords(null);
                  }}
                >
                  <td className="mono" title={r.key}>
                    {r.key.length > 100 ? `${r.key.slice(0, 100)}…` : r.key}
                  </td>
                  <td>{r.classification}</td>
                  <td>{r.baseline_count}</td>
                  <td>{r.suspect_count}</td>
                  <td>
                    <div className="dim" style={{ lineHeight: 1.1 }}>
                      <div
                        style={{
                          background: "currentColor",
                          height: 6,
                          width: pageMaxCount
                            ? `${(r.baseline_count / pageMaxCount) * 100}%`
                            : 0,
                          opacity: 0.45,
                        }}
                        title={`baseline ${r.baseline_count}`}
                      />
                      <div
                        style={{
                          background: "currentColor",
                          height: 6,
                          marginTop: 2,
                          width: pageMaxCount
                            ? `${(r.suspect_count / pageMaxCount) * 100}%`
                            : 0,
                        }}
                        title={`suspect ${r.suspect_count}`}
                      />
                    </div>
                  </td>
                  <td>{fmtRateChange(r.rate_change_bp)}</td>
                </tr>
              ))}
            </tbody>
          </table>
        </section>
      )}

      {detail && (
        <ResultDetail
          result={detail}
          staleReason={staleReason}
          records={records}
          recordSide={recordSide}
          onDrill={(side) => void doDrill(detail, side)}
        />
      )}
    </div>
  );
}

function ResultDetail(props: {
  result: ComparisonResultDto;
  staleReason: string | null;
  records: LogRowV2Dto[] | null;
  recordSide: "baseline" | "suspect";
  onDrill: (side: "baseline" | "suspect") => void;
}) {
  const { result, records } = props;
  const calc = JSON.parse(result.calculation_json || "{}") as Calculation;
  const durB = Number(calc.baseline_duration_nanos ?? 0);
  const durS = Number(calc.suspect_duration_nanos ?? 0);
  const durBms = Math.round(durB / NS_PER_MS);
  const durSms = Math.round(durS / NS_PER_MS);
  return (
    <section className="panel">
      <h3>Result detail</h3>
      <p className="mono">{result.key}</p>
      <p className="dim">
        id: <span className="mono">{result.result_id}</span>{" "}
        <button
          onClick={() => void navigator.clipboard.writeText(result.result_id)}
        >
          Copy ID
        </button>
        <button onClick={() => void navigator.clipboard.writeText(result.key)}>
          Copy key
        </button>
      </p>
      <table>
        <tbody>
          <tr>
            <td>classification</td>
            <td>
              {result.classification} (rule {result.rule_id} v
              {result.rule_version})
            </td>
          </tr>
          <tr>
            <td>baseline</td>
            <td>
              {result.baseline_count} occurrences in {fmtDuration(durBms)}
            </td>
          </tr>
          <tr>
            <td>suspect</td>
            <td>
              {result.suspect_count} occurrences in {fmtDuration(durSms)}
            </td>
          </tr>
          <tr>
            <td>count change</td>
            <td>
              {result.count_change > 0 ? "+" : ""}
              {result.count_change} (suspect − baseline)
            </td>
          </tr>
          <tr>
            <td>rate change</td>
            <td>
              {fmtRateChange(result.rate_change_bp)}
              {result.rate_change_bp !== "undefined" &&
                ` — ${result.rate_change_bp} basis points`}
            </td>
          </tr>
          <tr>
            <td>formula</td>
            <td className="mono">
              rates compared as suspect_count·baseline_duration vs
              baseline_count·suspect_duration ({result.suspect_count}·{durB} vs{" "}
              {result.baseline_count}·{durS}); change = (difference ×
              10000) ÷ (baseline_count·suspect_duration), integer division
            </td>
          </tr>
        </tbody>
      </table>
      <p className="dim">
        Integer arithmetic only: no rate is ever materialized as a float, and a
        zero baseline reports <span className="mono">undefined</span> rather
        than dividing.
      </p>
      <div className="row">
        <button
          onClick={() => props.onDrill("baseline")}
          disabled={!!props.staleReason || result.baseline_count === 0}
        >
          Baseline records (bounded)
        </button>
        <button
          onClick={() => props.onDrill("suspect")}
          disabled={!!props.staleReason || result.suspect_count === 0}
        >
          Suspect records (bounded)
        </button>
      </div>
      {records && (
        <div>
          <p className="dim">
            {props.recordSide}: {records.length} record(s)
            {records.length === DRILL_LIMIT ? " — limit reached" : ""}
          </p>
          <table>
            <thead>
              <tr>
                <th>Time</th>
                <th>Severity</th>
                <th>Message</th>
              </tr>
            </thead>
            <tbody>
              {records.map((r) => (
                <tr key={r.record_id}>
                  <td className="dim">{r.event_time_text ?? "—"}</td>
                  <td>{r.severity_text ?? ""}</td>
                  <td className="mono">{r.message}</td>
                </tr>
              ))}
            </tbody>
          </table>
        </div>
      )}
    </section>
  );
}
