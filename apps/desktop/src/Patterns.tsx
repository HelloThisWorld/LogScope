// Pattern Explorer (v0.4 WP2): definitions, runs, paged pattern
// summaries, and bounded deterministic drill-down. All semantics live
// Rust-side — this view never computes a pattern, never re-sorts
// results, and shows run/staleness/truncation states as text, not
// color alone.

import { useCallback, useEffect, useState } from "react";
import { listen } from "@tauri-apps/api/event";
import { api, errorText } from "./api";
import type {
  AnalysisDefinitionDto,
  AnalysisFinishedDto,
  AnalysisRunDto,
  PatternSummaryDto,
} from "./api";
import type { LogRowV2Dto } from "./bindings/LogRowV2Dto";

const MASK_RULES = [
  "quoted",
  "url",
  "path",
  "timestamp",
  "uuid",
  "trace_span_hex",
  "ip",
  "hex0x",
  "duration",
  "byte_size",
  "number",
  "bare_hex",
] as const;

const PAGE = 50;

function fmtTime(nanos: number | null): string {
  if (nanos === null) return "—";
  return new Date(Number(nanos / 1_000_000)).toISOString();
}


export default function Patterns(props: { onBack: () => void }) {
  const [error, setError] = useState("");
  const [defs, setDefs] = useState<AnalysisDefinitionDto[]>([]);
  const [selectedDef, setSelectedDef] = useState<string>("");
  const [runs, setRuns] = useState<AnalysisRunDto[]>([]);
  const [selectedRun, setSelectedRun] = useState<string>("");
  const [staleReason, setStaleReason] = useState<string | null>(null);
  const [runningJob, setRunningJob] = useState<string | null>(null);
  const [page, setPage] = useState(0);
  const [patterns, setPatterns] = useState<PatternSummaryDto[]>([]);
  const [detail, setDetail] = useState<PatternSummaryDto | null>(null);
  const [records, setRecords] = useState<LogRowV2Dto[] | null>(null);

  // Creation form.
  const [showCreate, setShowCreate] = useState(false);
  const [newKind, setNewKind] = useState<"message_pattern" | "stack_fingerprint">(
    "message_pattern",
  );
  const [newName, setNewName] = useState("");
  const [newQuery, setNewQuery] = useState("");
  const [newStackField, setNewStackField] = useState("");
  const [masks, setMasks] = useState<Record<string, boolean>>(
    Object.fromEntries(MASK_RULES.map((r) => [r, true])),
  );

  const refreshDefs = useCallback(async () => {
    try {
      const list = await api.listAnalysisDefinitions();
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
    setPatterns([]);
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

  const loadPatterns = useCallback(
    async (runId: string, pageNo: number) => {
      try {
        setPatterns(await api.listPatterns(runId, pageNo * PAGE, PAGE));
        setPage(pageNo);
        setDetail(null);
        setRecords(null);
      } catch (e) {
        setError(errorText(e));
      }
    },
    [],
  );

  const selectRun = async (run: AnalysisRunDto) => {
    setSelectedRun(run.run_id);
    setStaleReason(null);
    setPatterns([]);
    setDetail(null);
    setRecords(null);
    if (run.state === "completed" || run.state === "stale") {
      try {
        setStaleReason(await api.checkAnalysisRun(run.run_id));
      } catch {
        // staleness display is advisory; listing below still works
      }
      await loadPatterns(run.run_id, 0);
    }
  };

  const doCreate = async () => {
    setError("");
    try {
      await api.createPatternDefinition({
        kind: newKind,
        name: newName,
        description: null,
        dataset_ids: [],
        query_text: newQuery,
        stack_field: newKind === "stack_fingerprint" ? newStackField : null,
        masking_profile_json: JSON.stringify(masks),
        config_json: "{}",
        limits_json: "{}",
      });
      setShowCreate(false);
      setNewName("");
      await refreshDefs();
    } catch (e) {
      setError(errorText(e));
    }
  };

  const doRun = async () => {
    setError("");
    try {
      const started = await api.startPatternAnalysis(selectedDef);
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

  const doDrill = async (p: PatternSummaryDto) => {
    setError("");
    setRecords(null);
    try {
      setRecords(await api.patternRecords(selectedRun, p.pattern_id, 200));
    } catch (e) {
      setError(errorText(e));
    }
  };

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
        <td>{counts.accepted ?? "—"}</td>
        <td className="dim">
          {r.state === "failed" || r.state === "cancelled"
            ? (failed?.message ?? failed?.code ?? "")
            : (r.invalidation_reason ?? "")}
        </td>
      </tr>
    );
  };

  const selDef = defs.find((d) => d.definition_id === selectedDef);

  return (
    <div>
      <div className="row">
        <button onClick={props.onBack}>← Back</button>
        <h2>Patterns</h2>
        {runningJob && (
          <>
            <span className="status">analysis running…</span>
            <button onClick={() => void doCancel()}>Cancel run</button>
          </>
        )}
      </div>
      {error && <div className="error">{error}</div>}

      <section>
        <h3>Analysis definitions</h3>
        {defs.length === 0 && !showCreate && (
          <p className="dim">
            No analysis definitions yet — create one to extract message
            templates or stack fingerprints.
          </p>
        )}
        <div className="row">
          <select
            aria-label="analysis definition"
            value={selectedDef}
            onChange={(e) => setSelectedDef(e.target.value)}
          >
            {defs.map((d) => (
              <option key={d.definition_id} value={d.definition_id}>
                {d.name} ({d.kind}, rev {d.revision.toString()})
              </option>
            ))}
          </select>
          <button onClick={() => void doRun()} disabled={!selectedDef || !!runningJob}>
            Run analysis
          </button>
          <button onClick={() => setShowCreate(!showCreate)}>
            {showCreate ? "Close form" : "New definition…"}
          </button>
        </div>
        {selDef && (
          <p className="dim">
            scope: {selDef.dataset_ids.length ? selDef.dataset_ids.join(", ") : "all log datasets"}
            {" · query: "}
            {selDef.query_text || "(all records)"}
            {" · algorithm: "}
            {selDef.algorithm_id} v{selDef.algorithm_version.toString()}
          </p>
        )}
        {showCreate && (
          <div className="panel">
            <div className="row">
              <label>
                kind{" "}
                <select
                  value={newKind}
                  onChange={(e) =>
                    setNewKind(e.target.value as typeof newKind)
                  }
                >
                  <option value="message_pattern">message templates</option>
                  <option value="stack_fingerprint">stack fingerprints</option>
                </select>
              </label>
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
              {newKind === "stack_fingerprint" && (
                <input
                  placeholder="attribute holding the stack text"
                  value={newStackField}
                  onChange={(e) => setNewStackField(e.target.value)}
                />
              )}
            </div>
            <div className="row wrap">
              <span className="dim">mask rules (analysis identity only — not redaction):</span>
              {MASK_RULES.map((r) => (
                <label key={r}>
                  <input
                    type="checkbox"
                    checked={masks[r]}
                    onChange={(e) =>
                      setMasks({ ...masks, [r]: e.target.checked })
                    }
                  />
                  {r}
                </label>
              ))}
            </div>
            <button onClick={() => void doCreate()} disabled={!newName.trim()}>
              Create definition
            </button>
          </div>
        )}
      </section>

      <section>
        <h3>Runs</h3>
        {runs.length === 0 ? (
          <p className="dim">No runs for this definition yet.</p>
        ) : (
          <table>
            <thead>
              <tr>
                <th>State</th>
                <th>Started</th>
                <th>Accepted</th>
                <th>Note</th>
              </tr>
            </thead>
            <tbody>{runs.map(runRow)}</tbody>
          </table>
        )}
        {staleReason && (
          <div className="error">
            Run is stale: {staleReason}. Results stay readable; drill-down is
            refused — re-run the analysis for current data.
          </div>
        )}
      </section>

      {selectedRun && patterns.length === 0 && (
        <p className="dim">
          {runs.find((r) => r.run_id === selectedRun)?.state === "completed"
            ? "The run completed with zero patterns in scope."
            : "Select a completed run to see its patterns."}
        </p>
      )}

      {patterns.length > 0 && (
        <section>
          <h3>
            Patterns{" "}
            <span className="dim">
              (page {page + 1}, ordered by count — the stored order)
            </span>
          </h3>
          <div className="row">
            <button disabled={page === 0} onClick={() => void loadPatterns(selectedRun, page - 1)}>
              ← Prev
            </button>
            <button
              disabled={patterns.length < PAGE}
              onClick={() => void loadPatterns(selectedRun, page + 1)}
            >
              Next →
            </button>
          </div>
          <table>
            <thead>
              <tr>
                <th>Count</th>
                <th>Template</th>
                <th>First seen</th>
                <th>Last seen</th>
                <th>Flags</th>
              </tr>
            </thead>
            <tbody>
              {patterns.map((p) => (
                <tr
                  key={p.pattern_id}
                  className={detail?.pattern_id === p.pattern_id ? "selected" : ""}
                  onClick={() => {
                    setDetail(p);
                    setRecords(null);
                  }}
                >
                  <td>{p.count.toString()}</td>
                  <td className="mono" title={p.template}>
                    {p.template.length > 120
                      ? `${p.template.slice(0, 120)}…`
                      : p.template}
                  </td>
                  <td className="dim">{fmtTime(p.first_seen)}</td>
                  <td className="dim">{fmtTime(p.last_seen)}</td>
                  <td className="dim">
                    {[
                      p.untimestamped > 0 ? `${p.untimestamped} undated` : "",
                      p.buckets_truncated ? "buckets truncated" : "",
                      p.services_truncated ? "resources truncated" : "",
                      p.parse_quality && p.parse_quality !== "parsed"
                        ? `parse ${p.parse_quality}`
                        : "",
                    ]
                      .filter(Boolean)
                      .join(" · ")}
                  </td>
                </tr>
              ))}
            </tbody>
          </table>
        </section>
      )}

      {detail && (
        <section className="panel">
          <h3>Pattern detail</h3>
          <p className="mono">{detail.template}</p>
          {detail.exception_type && (
            <p>
              exception: <span className="mono">{detail.exception_type}</span>
            </p>
          )}
          <p className="dim">
            id: <span className="mono">{detail.pattern_id}</span>{" "}
            <button
              onClick={() => void navigator.clipboard.writeText(detail.pattern_id)}
            >
              Copy ID
            </button>
            <button
              onClick={() => void navigator.clipboard.writeText(detail.template)}
            >
              Copy template
            </button>
          </p>
          <p className="dim">
            count {detail.count.toString()} ({detail.untimestamped.toString()}{" "}
            undated) · peak bucket {fmtTime(detail.peak_bucket_start)} ×
            {detail.peak_bucket_count.toString()}
          </p>
          <p className="dim">
            resources:{" "}
            {(JSON.parse(detail.services_json) as {
              resource_id: string;
              count: number;
            }[])
              .map((s) => `${s.resource_id} (${s.count})`)
              .join(", ") || "—"}
            {detail.services_truncated ? " · truncated" : ""}
          </p>
          <p className="dim">
            examples (representative, not the complete contributing set):{" "}
            {(JSON.parse(detail.examples_json) as {
              role: string;
              record_id: string;
            }[])
              .map((x) => `${x.role}: ${x.record_id.slice(0, 16)}…`)
              .join(" · ")}
          </p>
          <button onClick={() => void doDrill(detail)} disabled={!!staleReason}>
            Show contributing records (bounded)
          </button>
          {records && (
            <div>
              <p className="dim">
                {records.length} record(s){records.length === 200 ? " — limit reached" : ""}
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
      )}
    </div>
  );
}
