// v0.3 evidence panel: flat + grouped listing, distinct integrity states
// (never collapsed into a boolean), batched verification with progress
// and cancellation over the shared jobs channel, supersession/history
// display, and jump-back that restores the captured Explorer context
// verbatim.

import { useEffect, useMemo, useState } from "react";
import { listen } from "@tauri-apps/api/event";
import { api, errorText } from "./api";
import type {
  EvidenceDto,
  InvestigationBundleDto,
  HistoryDto,
  RestoreContextDto,
  VerifyFinishedDto,
} from "./api";
import type { SaveState } from "./Case";

/** Distinct rendering for every resolver state (vocab.rs ResolverState). */
const RESOLVER_STATES: Record<string, { icon: string; label: string }> = {
  unverified: { icon: "○", label: "unverified" },
  verified: { icon: "✓", label: "verified" },
  broken: { icon: "✖", label: "broken" },
  unsupported_reference_version: {
    icon: "⛔",
    label: "written by a newer build",
  },
  dataset_revision_unavailable: {
    icon: "⚠",
    label: "dataset revision unavailable",
  },
  source_missing: { icon: "⚠", label: "source file missing" },
  source_changed: { icon: "⚠", label: "source file changed" },
  canonical_available_source_unavailable: {
    icon: "⚠",
    label: "canonical ok, source unavailable",
  },
  partially_resolved: { icon: "◐", label: "partially resolved" },
  query_drift: { icon: "Δ", label: "query drift" },
};

function StateBadge({ state }: { state: string }) {
  const s = RESOLVER_STATES[state] ?? { icon: "?", label: state };
  return (
    <span className={`resolver-state rs-${state}`} title={state}>
      <span aria-hidden="true">{s.icon}</span> {s.label}
    </span>
  );
}

type VerifyProgress = {
  running: boolean;
  jobId: string | null;
  line: string;
  summary: string;
};

export default function EvidencePanel({
  bundle,
  saves,
  tracked,
  reload,
  onJumpToExplorer,
}: {
  bundle: InvestigationBundleDto;
  saves: Record<string, SaveState>;
  tracked: (
    id: string,
    work: () => Promise<void>,
    after?: () => Promise<void>,
  ) => Promise<void>;
  reload: () => Promise<void>;
  onJumpToExplorer: (ctx: RestoreContextDto) => void;
}) {
  const [grouped, setGrouped] = useState(true);
  const [showArchived, setShowArchived] = useState(false);
  const [newGroupName, setNewGroupName] = useState("");
  const [verify, setVerify] = useState<VerifyProgress>({
    running: false,
    jobId: null,
    line: "",
    summary: "",
  });
  const [editing, setEditing] = useState<
    Record<string, { title: string; annotation: string; relevance: string }>
  >({});
  const [historyFor, setHistoryFor] = useState<string | null>(null);
  const [history, setHistory] = useState<HistoryDto[]>([]);
  const [snapshotFor, setSnapshotFor] = useState<string | null>(null);

  const invId = bundle.investigation.investigation_id;

  useEffect(() => {
    const unProgress = listen<{
      event: string;
      job_id: string;
      progress?: { stage: string; current_item?: string };
    }>("job-event", (e) => {
      const p = e.payload;
      setVerify((v) =>
        v.running && p.job_id === v.jobId && p.event === "progress" && p.progress
          ? {
              ...v,
              line: `${p.progress.stage}${
                p.progress.current_item ? `: ${p.progress.current_item}` : ""
              }`,
            }
          : v,
      );
    });
    const unDone = listen<VerifyFinishedDto>("verify-finished", (e) => {
      const p = e.payload;
      setVerify((v) => {
        if (p.job_id !== v.jobId) return v;
        const summary = p.report
          ? `${p.report.cancelled ? "cancelled — " : ""}${p.report.updated} of ${
              p.report.total
            } verified in ${p.report.duration_ms} ms (${Object.entries(
              p.report.states,
            )
              .map(([k, n]) => `${k}: ${n}`)
              .join(", ")}); ${p.report.dataset_lookups} dataset lookups`
          : `verification failed: ${p.error ? `[${p.error.code}] ${p.error.message}` : "unknown"}`;
        return { running: false, jobId: null, line: "", summary };
      });
      void reload();
    });
    return () => {
      unProgress.then((f) => f());
      unDone.then((f) => f());
    };
  }, [reload]);

  const startVerify = async (only: string[] | null) => {
    try {
      const started = await api.startVerifyEvidence(invId, only);
      setVerify({
        running: true,
        jobId: started.job_id,
        line: `verifying ${started.total} evidence item(s)…`,
        summary: "",
      });
    } catch (e) {
      setVerify({
        running: false,
        jobId: null,
        line: "",
        summary: errorText(e),
      });
    }
  };

  const cancelVerify = async () => {
    if (verify.jobId) {
      try {
        await api.cancelJob(verify.jobId);
      } catch {
        // The job may have finished in the meantime; the terminal event
        // will report the truth either way.
      }
    }
  };

  const visible = useMemo(
    () => bundle.evidence.filter((ev) => showArchived || !ev.archived),
    [bundle.evidence, showArchived],
  );

  const sections: [string, string, EvidenceDto[]][] = useMemo(() => {
    if (!grouped) return [["all", "All evidence", visible]];
    const byGroup = new Map<string, EvidenceDto[]>();
    for (const ev of visible) {
      const key = ev.group_id ?? "";
      byGroup.set(key, [...(byGroup.get(key) ?? []), ev]);
    }
    const out: [string, string, EvidenceDto[]][] = bundle.groups.map((g) => [
      g.group_id,
      g.name,
      byGroup.get(g.group_id) ?? [],
    ]);
    out.push(["", "Ungrouped", byGroup.get("") ?? []]);
    return out;
  }, [grouped, visible, bundle.groups]);

  const jump = async (evidenceId: string) => {
    try {
      onJumpToExplorer(await api.evidenceRestoreContext(evidenceId));
    } catch (e) {
      setVerify((v) => ({ ...v, summary: errorText(e) }));
    }
  };

  const showHistory = async (evidenceId: string) => {
    if (historyFor === evidenceId) {
      setHistoryFor(null);
      return;
    }
    setHistory(await api.evidenceHistory(evidenceId));
    setHistoryFor(evidenceId);
  };

  return (
    <>
      <h4>Evidence</h4>
      <div className="row">
        <button
          onClick={() => void startVerify(null)}
          disabled={verify.running || bundle.evidence.length === 0}
        >
          Verify all
        </button>
        {verify.running && (
          <>
            <span className="dim" role="status">
              {verify.line}
            </span>
            <button onClick={() => void cancelVerify()}>Cancel</button>
          </>
        )}
        {!verify.running && verify.summary && (
          <span className="dim" role="status">
            {verify.summary}
          </span>
        )}
        <span className="spacer" />
        <label className="dim">
          <input
            type="checkbox"
            checked={grouped}
            onChange={(e) => setGrouped(e.target.checked)}
          />
          grouped
        </label>
        <label className="dim">
          <input
            type="checkbox"
            checked={showArchived}
            onChange={(e) => setShowArchived(e.target.checked)}
          />
          show archived
        </label>
      </div>

      <div className="row">
        <input
          value={newGroupName}
          onChange={(e) => setNewGroupName(e.target.value)}
          placeholder="new group name"
          aria-label="New evidence group name"
        />
        <button
          disabled={!newGroupName.trim()}
          onClick={() =>
            void tracked(
              "new-group",
              async () => {
                await api.createEvidenceGroup(invId, newGroupName.trim());
                setNewGroupName("");
              },
              reload,
            )
          }
        >
          Add group
        </button>
      </div>

      {sections.map(([groupId, name, list]) => (
        <div key={groupId || "__ungrouped"} className="evidence-group">
          {grouped && (
            <div className="row">
              <h5>{name}</h5>
              {groupId && (
                <>
                  <button
                    aria-label={`Rename group ${name}`}
                    onClick={() => {
                      const g = bundle.groups.find(
                        (x) => x.group_id === groupId,
                      );
                      if (!g) return;
                      const next = window.prompt("Group name", g.name);
                      if (next && next.trim() && next !== g.name) {
                        void tracked(
                          groupId,
                          async () => {
                            await api.renameEvidenceGroup(
                              groupId,
                              g.revision,
                              next.trim(),
                            );
                          },
                          reload,
                        );
                      }
                    }}
                  >
                    rename
                  </button>
                  <button
                    aria-label={`Delete group ${name}`}
                    onClick={() =>
                      void tracked(
                        groupId,
                        async () => {
                          await api.deleteEvidenceGroup(groupId);
                        },
                        reload,
                      )
                    }
                  >
                    delete
                  </button>
                </>
              )}
            </div>
          )}
          <ul className="case-cards">
            {list.map((ev) => {
              const ed = editing[ev.evidence_id];
              return (
                <li
                  key={ev.evidence_id}
                  className={`case-card${ev.archived ? " archived" : ""}`}
                >
                  <div className="row">
                    <span className={`kind-chip kind-${ev.kind}`}>
                      {ev.kind}
                    </span>
                    <StateBadge state={ev.resolver_state} />
                    {ev.supersedes_evidence_id && (
                      <span
                        className="dim"
                        title={`supersedes ${ev.supersedes_evidence_id}`}
                      >
                        supersedes {ev.supersedes_evidence_id.slice(0, 12)}…
                      </span>
                    )}
                    <SaveBadgeInline state={saves[ev.evidence_id]} />
                    <span className="spacer" />
                    {ev.last_verified_at && (
                      <span className="dim">
                        verified {ev.last_verified_at}
                      </span>
                    )}
                  </div>

                  {ed ? (
                    <>
                      <input
                        value={ed.title}
                        aria-label="Evidence title"
                        onChange={(e) =>
                          setEditing((s) => ({
                            ...s,
                            [ev.evidence_id]: { ...ed, title: e.target.value },
                          }))
                        }
                      />
                      <textarea
                        value={ed.annotation}
                        rows={2}
                        placeholder="annotation"
                        aria-label="Evidence annotation"
                        onChange={(e) =>
                          setEditing((s) => ({
                            ...s,
                            [ev.evidence_id]: {
                              ...ed,
                              annotation: e.target.value,
                            },
                          }))
                        }
                      />
                      <textarea
                        value={ed.relevance}
                        rows={2}
                        placeholder="why this matters"
                        aria-label="Evidence relevance"
                        onChange={(e) =>
                          setEditing((s) => ({
                            ...s,
                            [ev.evidence_id]: {
                              ...ed,
                              relevance: e.target.value,
                            },
                          }))
                        }
                      />
                      <div className="row">
                        <button
                          onClick={() =>
                            void tracked(
                              ev.evidence_id,
                              async () => {
                                await api.updateEvidenceAnnotation(
                                  ev.evidence_id,
                                  ev.revision,
                                  ed.title.trim(),
                                  ed.annotation.trim() || null,
                                  ed.relevance.trim() || null,
                                );
                                setEditing((s) => {
                                  const { [ev.evidence_id]: _, ...rest } = s;
                                  return rest;
                                });
                              },
                              reload,
                            )
                          }
                        >
                          Save
                        </button>
                        <button
                          onClick={() =>
                            setEditing((s) => {
                              const { [ev.evidence_id]: _, ...rest } = s;
                              return rest;
                            })
                          }
                        >
                          Cancel
                        </button>
                      </div>
                    </>
                  ) : (
                    <>
                      <div>
                        <strong>{ev.title}</strong>
                        {ev.annotation && (
                          <div className="dim">{ev.annotation}</div>
                        )}
                        {ev.relevance && (
                          <div className="dim">why: {ev.relevance}</div>
                        )}
                      </div>
                      <div className="row">
                        {ev.kind !== "item_ref" && (
                          <button
                            onClick={() => void jump(ev.evidence_id)}
                            aria-label="Open in Explorer exactly as captured"
                          >
                            Open in Explorer
                          </button>
                        )}
                        <button
                          onClick={() =>
                            void startVerify([ev.evidence_id])
                          }
                          disabled={verify.running}
                        >
                          verify
                        </button>
                        <button
                          onClick={() =>
                            setEditing((s) => ({
                              ...s,
                              [ev.evidence_id]: {
                                title: ev.title,
                                annotation: ev.annotation ?? "",
                                relevance: ev.relevance ?? "",
                              },
                            }))
                          }
                        >
                          edit
                        </button>
                        <label className="dim">
                          group
                          <select
                            value={ev.group_id ?? ""}
                            aria-label="Evidence group"
                            onChange={(e) =>
                              void tracked(
                                ev.evidence_id,
                                async () => {
                                  await api.setEvidenceGroup(
                                    ev.evidence_id,
                                    ev.revision,
                                    e.target.value || null,
                                  );
                                },
                                reload,
                              )
                            }
                          >
                            <option value="">(none)</option>
                            {bundle.groups.map((g) => (
                              <option key={g.group_id} value={g.group_id}>
                                {g.name}
                              </option>
                            ))}
                          </select>
                        </label>
                        <label className="dim">
                          link
                          <select
                            value=""
                            aria-label="Link to hypothesis"
                            onChange={(e) => {
                              const hid = e.target.value;
                              const h = bundle.hypotheses.find(
                                (x) => x.hypothesis_id === hid,
                              );
                              if (!h) return;
                              const linked = h.linked_evidence_ids.includes(
                                ev.evidence_id,
                              );
                              void tracked(
                                ev.evidence_id,
                                async () => {
                                  if (linked) {
                                    await api.unlinkHypothesisEvidence(
                                      hid,
                                      h.revision,
                                      ev.evidence_id,
                                    );
                                  } else {
                                    await api.linkHypothesisEvidence(
                                      hid,
                                      h.revision,
                                      ev.evidence_id,
                                    );
                                  }
                                },
                                reload,
                              );
                            }}
                          >
                            <option value="">hypothesis…</option>
                            {bundle.hypotheses.map((h) => (
                              <option
                                key={h.hypothesis_id}
                                value={h.hypothesis_id}
                              >
                                {h.linked_evidence_ids.includes(ev.evidence_id)
                                  ? "✓ "
                                  : ""}
                                {h.statement.slice(0, 40)}
                              </option>
                            ))}
                          </select>
                        </label>
                        <button
                          onClick={() => void showHistory(ev.evidence_id)}
                        >
                          {historyFor === ev.evidence_id
                            ? "hide history"
                            : "history"}
                        </button>
                        <button
                          onClick={() =>
                            setSnapshotFor(
                              snapshotFor === ev.evidence_id
                                ? null
                                : ev.evidence_id,
                            )
                          }
                        >
                          {snapshotFor === ev.evidence_id
                            ? "hide snapshot"
                            : "snapshot"}
                        </button>
                        <button
                          onClick={() =>
                            void tracked(
                              ev.evidence_id,
                              async () => {
                                await api.setEvidenceArchived(
                                  ev.evidence_id,
                                  ev.revision,
                                  !ev.archived,
                                );
                              },
                              reload,
                            )
                          }
                        >
                          {ev.archived ? "restore" : "archive"}
                        </button>
                      </div>
                      {ev.resolver_state !== "unverified" &&
                        ev.resolver_state !== "verified" && (
                          <details className="dim">
                            <summary>state detail</summary>
                            <pre className="snapshot">
                              {pretty(ev.resolver_detail_json)}
                            </pre>
                          </details>
                        )}
                      {snapshotFor === ev.evidence_id && (
                        <pre className="snapshot" aria-label="Captured snapshot">
                          {pretty(ev.snapshot_json)}
                        </pre>
                      )}
                      {historyFor === ev.evidence_id && (
                        <ul className="activity">
                          {history.map((h) => (
                            <li key={h.history_id}>
                              <span className="dim">{h.created_at}</span>{" "}
                              {h.action}{" "}
                              <span className="dim">rev {h.revision}</span>
                            </li>
                          ))}
                          {history.length === 0 && (
                            <li className="dim">no history</li>
                          )}
                        </ul>
                      )}
                    </>
                  )}
                </li>
              );
            })}
            {list.length === 0 && <li className="dim">empty</li>}
          </ul>
        </div>
      ))}
      {bundle.evidence.length === 0 && (
        <div className="dim">
          No evidence yet — pin events, selections, queries, groups, or
          histogram intervals from the Explorer.
        </div>
      )}
    </>
  );
}

function pretty(json: string): string {
  try {
    return JSON.stringify(JSON.parse(json), null, 2);
  } catch {
    return json;
  }
}

function SaveBadgeInline({ state }: { state: SaveState | undefined }) {
  if (!state) return null;
  const label =
    state.phase === "pending"
      ? "saving…"
      : state.phase === "saved"
        ? "saved"
        : state.phase === "conflicted"
          ? "conflicted"
          : "failed";
  return (
    <span
      className={`save-badge save-${state.phase}`}
      role="status"
      title={state.message}
    >
      {label}
    </span>
  );
}
