// LogScope shell: workspace lifecycle, import, and the entry into the
// v0.2 Log Explorer.

import { useCallback, useEffect, useRef, useState } from "react";
import { listen } from "@tauri-apps/api/event";
import { open as openDialog, save as saveDialog } from "@tauri-apps/plugin-dialog";
import { api, errorText } from "./api";
import type { OverviewDto, RestoreContextDto, WorkspaceInfoDto } from "./api";
import Explorer from "./Explorer";
import CaseView from "./Case";

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
  const [importFormat, setImportFormat] = useState<
    "jsonl" | "csv" | "elasticsearch"
  >("jsonl");
  const [activeJob, setActiveJob] = useState<string | null>(null);
  const [jobLine, setJobLine] = useState<string>("");
  const [view, setView] = useState<"home" | "explorer" | "case">("home");
  const [pendingRestore, setPendingRestore] =
    useState<RestoreContextDto | null>(null);
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
      setView("home");
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

  if (view === "explorer" && workspace && overview) {
    return (
      <main className="main-wide">
        {error && <div className="error">{error}</div>}
        <Explorer
          overview={overview}
          onBack={() => {
            setView("home");
            void refreshOverview();
          }}
          onOpenImport={() => setView("home")}
          restore={pendingRestore}
          onRestoreConsumed={() => setPendingRestore(null)}
        />
      </main>
    );
  }

  if (view === "case" && workspace) {
    return (
      <main className="main-wide">
        {error && <div className="error">{error}</div>}
        <CaseView
          onBack={() => {
            setView("home");
            void refreshOverview();
          }}
          onJumpToExplorer={(ctx) => {
            setPendingRestore(ctx);
            setView("explorer");
          }}
        />
      </main>
    );
  }

  return (
    <main>
      <h1>LogScope</h1>
      <p className="subtitle">offline log investigation</p>
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
              <button
                onClick={() => setView("explorer")}
                disabled={
                  !overview?.datasets.some(
                    (d) => d.signal === "logs" && d.status === "published",
                  ) || !!activeJob
                }
              >
                Explore logs
              </button>
              <button onClick={() => setView("case")} disabled={!!activeJob}>
                Investigations
              </button>
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
                aria-label="Import profile"
                onChange={(e) =>
                  setImportFormat(
                    e.target.value as "jsonl" | "csv" | "elasticsearch",
                  )
                }
              >
                <option value="jsonl">JSON lines (generic)</option>
                <option value="csv">CSV (with headers)</option>
                <option value="elasticsearch">
                  Elasticsearch export (JSONL, ECS)
                </option>
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

        </>
      )}
    </main>
  );
}
