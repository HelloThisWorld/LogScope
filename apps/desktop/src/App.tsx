// LogScope v0.0 proof shell: create/open a workspace, run the proof
// import/query path, watch job progress, cancel, close, reopen.
// This is intentionally not the v0.2 Log Explorer.

import { useCallback, useEffect, useRef, useState } from "react";
import { listen } from "@tauri-apps/api/event";
import { open as openDialog, save as saveDialog } from "@tauri-apps/plugin-dialog";
import { api, errorText } from "./api";
import type { LogPageDto, OverviewDto, WorkspaceInfoDto } from "./api";

type JobEventPayload = {
  event: "started" | "progress" | "finished";
  job_id: string;
  progress?: {
    stage: string;
    current_item?: string;
    records_accepted: number;
    records_rejected: number;
    records_unparsed: number;
    records_duplicate: number;
    bytes_processed: number;
  };
  status?: string;
  error?: { code: string; message: string };
};

export default function App() {
  const [workspace, setWorkspace] = useState<WorkspaceInfoDto | null>(null);
  const [overview, setOverview] = useState<OverviewDto | null>(null);
  const [recent, setRecent] = useState<string[]>([]);
  const [status, setStatus] = useState<string>("");
  const [error, setError] = useState<string>("");

  const [wsPath, setWsPath] = useState("");
  const [wsName, setWsName] = useState("Investigation");

  const [importPaths, setImportPaths] = useState<string[]>([]);
  const [importName, setImportName] = useState("imported logs");
  const [importFormat, setImportFormat] = useState<"jsonl" | "csv">("jsonl");
  const [activeJob, setActiveJob] = useState<string | null>(null);
  const [jobLine, setJobLine] = useState<string>("");

  const [searchText, setSearchText] = useState("");
  const [minSeverity, setMinSeverity] = useState<string>("");
  const [offset, setOffset] = useState(0);
  const [page, setPage] = useState<LogPageDto | null>(null);
  const importFinishedRef = useRef<() => void>(() => {});

  const refreshOverview = useCallback(async () => {
    try {
      setOverview(await api.overview());
      setError("");
    } catch (e) {
      setOverview(null);
      if (!String(errorText(e)).includes("workspace/importing")) {
        setError(errorText(e));
      }
    }
  }, []);

  useEffect(() => {
    api.recentWorkspaces().then(setRecent).catch(() => {});
    const unlistenJob = listen<JobEventPayload>("job-event", (e) => {
      const p = e.payload;
      if (p.event === "progress" && p.progress) {
        const pr = p.progress;
        setJobLine(
          `${pr.stage}: accepted ${pr.records_accepted}, rejected ${pr.records_rejected}, ` +
            `unparsed ${pr.records_unparsed}, duplicates ${pr.records_duplicate}, ` +
            `${(pr.bytes_processed / 1048576).toFixed(1)} MiB read`,
        );
      } else if (p.event === "finished") {
        setJobLine(
          p.status === "completed"
            ? "import completed"
            : `import ${p.status}${p.error ? `: [${p.error.code}] ${p.error.message}` : ""}`,
        );
      }
    });
    const unlistenDone = listen<string>("import-finished", () => {
      setActiveJob(null);
      importFinishedRef.current();
    });
    return () => {
      unlistenJob.then((f) => f());
      unlistenDone.then((f) => f());
    };
  }, []);

  importFinishedRef.current = () => {
    void refreshOverview();
  };

  const guard = async (work: () => Promise<void>) => {
    try {
      setError("");
      await work();
    } catch (e) {
      setError(errorText(e));
    }
  };

  const doCreate = () =>
    guard(async () => {
      const picked =
        wsPath ||
        (await saveDialog({ title: "Choose a new workspace folder" })) ||
        "";
      if (!picked) return;
      const info = await api.createWorkspace(picked, wsName);
      setWorkspace(info);
      setStatus(`created workspace ${info.name} (${info.workspace_id})`);
      await refreshOverview();
    });

  const doOpen = (path?: string) =>
    guard(async () => {
      const picked =
        path ||
        wsPath ||
        ((await openDialog({ directory: true, title: "Open workspace folder" })) as
          | string
          | null) ||
        "";
      if (!picked) return;
      const info = await api.openWorkspace(picked);
      setWorkspace(info);
      const rec = info.recovery
        ? ` (recovered: ${info.recovery.interrupted_jobs.length} interrupted jobs, ` +
          `${info.recovery.discarded_staging_dirs.length} staging dirs discarded)`
        : "";
      setStatus(`opened workspace ${info.name}${rec}`);
      await refreshOverview();
    });

  const doClose = () =>
    guard(async () => {
      await api.closeWorkspace();
      setWorkspace(null);
      setOverview(null);
      setPage(null);
      setStatus("workspace closed");
    });

  const pickImportFiles = () =>
    guard(async () => {
      const picked = (await openDialog({
        multiple: true,
        title: "Select log files",
        filters: [
          { name: "Logs", extensions: ["jsonl", "ndjson", "json", "csv", "log", "gz"] },
        ],
      })) as string[] | string | null;
      if (!picked) return;
      setImportPaths(Array.isArray(picked) ? picked : [picked]);
    });

  const doImport = () =>
    guard(async () => {
      const jobId = await api.startImport({
        paths: importPaths,
        dataset_name: importName,
        format: importFormat,
      });
      setActiveJob(jobId);
      setJobLine("import started");
    });

  const doCancel = () =>
    guard(async () => {
      if (activeJob) await api.cancelJob(activeJob);
    });

  const runQuery = (nextOffset = 0) =>
    guard(async () => {
      const result = await api.queryLogs({
        dataset_ids: [],
        time_start: null,
        time_end: null,
        min_severity: minSeverity ? Number(minSeverity) : null,
        contains_text: searchText || null,
        limit: 50,
        offset: nextOffset,
      });
      setOffset(nextOffset);
      setPage(result);
    });

  return (
    <main>
      <h1>LogScope</h1>
      <p className="subtitle">v0.0 offline architecture proof — not the Log Explorer</p>
      {error && <div className="error">{error}</div>}
      {status && <div className="status">{status}</div>}

      {!workspace ? (
        <section>
          <h2>Workspace</h2>
          <div className="row">
            <input
              placeholder="workspace folder path"
              value={wsPath}
              onChange={(e) => setWsPath(e.target.value)}
              size={60}
            />
            <input
              placeholder="name"
              value={wsName}
              onChange={(e) => setWsName(e.target.value)}
            />
            <button onClick={doCreate}>Create</button>
            <button onClick={() => doOpen()}>Open…</button>
          </div>
          {recent.length > 0 && (
            <div>
              <h3>Recent</h3>
              <ul>
                {recent.map((r) => (
                  <li key={r}>
                    <button className="link" onClick={() => doOpen(r)}>
                      {r}
                    </button>
                  </li>
                ))}
              </ul>
            </div>
          )}
        </section>
      ) : (
        <>
          <section>
            <h2>
              {workspace.name}{" "}
              <span className="dim">({workspace.root})</span>
            </h2>
            <div className="row">
              <span className="dim">
                schema v{workspace.schema_version.toString()} · signals:{" "}
                {workspace.available_signals.join(", ") || "none yet"}
              </span>
              <button onClick={doClose}>Close workspace</button>
              <button onClick={refreshOverview}>Refresh</button>
            </div>
            {overview && (
              <table>
                <thead>
                  <tr>
                    <th>Dataset</th>
                    <th>Signal</th>
                    <th>Status</th>
                    <th>Rows</th>
                    <th>Segments</th>
                    <th>Size</th>
                  </tr>
                </thead>
                <tbody>
                  {overview.datasets.map((d) => (
                    <tr key={d.dataset_id}>
                      <td>{d.name}</td>
                      <td>{d.signal}</td>
                      <td>{d.status}</td>
                      <td className="num">{d.row_count.toLocaleString()}</td>
                      <td className="num">{d.segment_count.toString()}</td>
                      <td className="num">
                        {(Number(d.byte_size) / 1048576).toFixed(1)} MiB
                      </td>
                    </tr>
                  ))}
                </tbody>
              </table>
            )}
          </section>

          <section>
            <h2>Import (proof path)</h2>
            <div className="row">
              <button onClick={pickImportFiles}>Select files…</button>
              <span className="dim">
                {importPaths.length
                  ? `${importPaths.length} file(s) selected`
                  : "no files selected"}
              </span>
              <input
                value={importName}
                onChange={(e) => setImportName(e.target.value)}
                placeholder="dataset name"
              />
              <select
                value={importFormat}
                onChange={(e) => setImportFormat(e.target.value as "jsonl" | "csv")}
              >
                <option value="jsonl">JSON lines</option>
                <option value="csv">CSV (with headers)</option>
              </select>
              <button onClick={doImport} disabled={!importPaths.length || !!activeJob}>
                Start import
              </button>
              <button onClick={doCancel} disabled={!activeJob}>
                Cancel
              </button>
            </div>
            {jobLine && <div className="jobline">{jobLine}</div>}
          </section>

          <section>
            <h2>Query (proof path)</h2>
            <div className="row">
              <input
                placeholder="full-text search"
                value={searchText}
                onChange={(e) => setSearchText(e.target.value)}
              />
              <select
                value={minSeverity}
                onChange={(e) => setMinSeverity(e.target.value)}
              >
                <option value="">any severity</option>
                <option value="9">INFO and above</option>
                <option value="13">WARN and above</option>
                <option value="17">ERROR and above</option>
              </select>
              <button onClick={() => runQuery(0)}>Run query</button>
            </div>
            {page && (
              <>
                <div className="row dim">
                  {page.rows.length} rows (limit {page.limit}, offset {offset})
                  <button disabled={offset === 0} onClick={() => runQuery(Math.max(0, offset - page.limit))}>
                    Prev
                  </button>
                  <button disabled={!page.has_more} onClick={() => runQuery(offset + page.limit)}>
                    Next
                  </button>
                </div>
                <table>
                  <thead>
                    <tr>
                      <th>Time</th>
                      <th>Severity</th>
                      <th>Message</th>
                      <th>Locator</th>
                    </tr>
                  </thead>
                  <tbody>
                    {page.rows.map((r) => (
                      <tr key={r.record_id}>
                        <td className="mono">{r.event_time_text ?? "—"}</td>
                        <td>{r.severity_text ?? "—"}</td>
                        <td>{r.display_message}</td>
                        <td className="mono dim">
                          #{r.record_number?.toString() ?? "?"} L
                          {r.line_start?.toString() ?? "?"}
                        </td>
                      </tr>
                    ))}
                  </tbody>
                </table>
              </>
            )}
          </section>
        </>
      )}
    </main>
  );
}
