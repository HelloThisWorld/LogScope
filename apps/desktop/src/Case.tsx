// v0.3 Investigation workspace: create/select/edit investigations,
// hypotheses with audited states, typed items (notes/tasks/findings/
// questions), and the evidence panel. Every mutation sends the row's
// revision back; a stale revision surfaces as "conflicted", never as a
// silent overwrite.

import { useCallback, useEffect, useState } from "react";
import {
  api,
  errorText,
  isStaleRevision,
  INVESTIGATION_STATUSES,
  SEVERITIES,
  HYPOTHESIS_STATES,
  ITEM_KINDS,
  TASK_STATUSES,
  QUESTION_STATUSES,
} from "./api";
import type {
  InvestigationDto,
  InvestigationBundleDto,
  HistoryDto,
  RestoreContextDto,
} from "./api";
import EvidencePanel from "./EvidencePanel";

/** Per-entity save indicator; `conflicted` means the row changed under us. */
export type SaveState = {
  phase: "pending" | "saved" | "failed" | "conflicted";
  message?: string;
};

function nsToIso(ns: number | null): string {
  if (ns === null) return "";
  return new Date(ns / 1e6).toISOString();
}

/** "" → null; invalid text → undefined (kept out of the request). */
function isoToNs(text: string): number | null | undefined {
  const t = text.trim();
  if (!t) return null;
  const ms = Date.parse(t);
  if (Number.isNaN(ms)) return undefined;
  return ms * 1e6;
}

type InvDraft = {
  title: string;
  description: string;
  severity: string;
  owner_text: string;
  tags: string;
  incident_started_at: string;
  mitigated_at: string;
  resolved_at: string;
  window_start: string;
  window_end: string;
};

function draftFrom(inv: InvestigationDto): InvDraft {
  return {
    title: inv.title,
    description: inv.description ?? "",
    severity: inv.severity ?? "",
    owner_text: inv.owner_text ?? "",
    tags: inv.tags.join(", "),
    incident_started_at: nsToIso(inv.incident_started_at),
    mitigated_at: nsToIso(inv.mitigated_at),
    resolved_at: nsToIso(inv.resolved_at),
    window_start: nsToIso(inv.window_start),
    window_end: nsToIso(inv.window_end),
  };
}

function SaveBadge({ state }: { state: SaveState | undefined }) {
  if (!state) return null;
  const label =
    state.phase === "pending"
      ? "saving…"
      : state.phase === "saved"
        ? "saved"
        : state.phase === "conflicted"
          ? "conflicted — reload to pick up the newer revision"
          : `failed: ${state.message ?? "unknown error"}`;
  return (
    <span className={`save-badge save-${state.phase}`} role="status">
      {label}
    </span>
  );
}

export default function CaseView({
  onBack,
  onJumpToExplorer,
}: {
  onBack: () => void;
  onJumpToExplorer: (ctx: RestoreContextDto) => void;
}) {
  const [list, setList] = useState<InvestigationDto[]>([]);
  const [includeArchived, setIncludeArchived] = useState(false);
  const [selectedId, setSelectedId] = useState<string | null>(null);
  const [bundle, setBundle] = useState<InvestigationBundleDto | null>(null);
  const [activity, setActivity] = useState<HistoryDto[]>([]);
  const [error, setError] = useState("");
  const [saves, setSaves] = useState<Record<string, SaveState>>({});

  const [newTitle, setNewTitle] = useState("");
  const [draft, setDraft] = useState<InvDraft | null>(null);

  const [newHypStatement, setNewHypStatement] = useState("");
  const [newHypRationale, setNewHypRationale] = useState("");
  const [hypDrafts, setHypDrafts] = useState<
    Record<string, { statement: string; rationale: string }>
  >({});

  const [newItemKind, setNewItemKind] = useState<string>("note");
  const [newItemContent, setNewItemContent] = useState("");
  const [itemFilter, setItemFilter] = useState<string>("all");
  const [showArchivedItems, setShowArchivedItems] = useState(false);
  const [itemDrafts, setItemDrafts] = useState<Record<string, string>>({});

  const refreshList = useCallback(async () => {
    try {
      setList(await api.listInvestigations(includeArchived));
      setError("");
    } catch (e) {
      setError(errorText(e));
    }
  }, [includeArchived]);

  const loadBundle = useCallback(async (id: string) => {
    try {
      const b = await api.investigationBundle(id);
      setBundle(b);
      setDraft(draftFrom(b.investigation));
      setActivity(await api.investigationActivity(id, 100));
      setError("");
    } catch (e) {
      setError(errorText(e));
    }
  }, []);

  useEffect(() => {
    void refreshList();
  }, [refreshList]);

  useEffect(() => {
    if (selectedId) void loadBundle(selectedId);
    else {
      setBundle(null);
      setDraft(null);
      setActivity([]);
    }
  }, [selectedId, loadBundle]);

  /** Runs one mutation with a per-entity save indicator. */
  const tracked = async (
    entityId: string,
    work: () => Promise<void>,
    after?: () => Promise<void>,
  ) => {
    setSaves((s) => ({ ...s, [entityId]: { phase: "pending" } }));
    try {
      await work();
      setSaves((s) => ({ ...s, [entityId]: { phase: "saved" } }));
      if (after) await after();
    } catch (e) {
      setSaves((s) => ({
        ...s,
        [entityId]: {
          phase: isStaleRevision(e) ? "conflicted" : "failed",
          message: errorText(e),
        },
      }));
    }
  };

  const reloadSelected = useCallback(async () => {
    if (selectedId) await loadBundle(selectedId);
    await refreshList();
  }, [selectedId, loadBundle, refreshList]);

  const doCreate = () =>
    tracked(
      "new-investigation",
      async () => {
        const title = newTitle.trim();
        if (!title) throw { code: "ui/blank-title", message: "title must not be blank" };
        const created = await api.createInvestigation({
          title,
          description: null,
          severity: null,
          owner_text: null,
          tags: [],
          incident_started_at: null,
          window_start: null,
          window_end: null,
        });
        setNewTitle("");
        setSelectedId(created.investigation_id);
      },
      refreshList,
    );

  const doSaveInvestigation = () => {
    if (!bundle || !draft) return;
    const inv = bundle.investigation;
    const fields: [string, string][] = [
      ["incident started", draft.incident_started_at],
      ["mitigated", draft.mitigated_at],
      ["resolved", draft.resolved_at],
      ["window start", draft.window_start],
      ["window end", draft.window_end],
    ];
    for (const [label, text] of fields) {
      if (isoToNs(text) === undefined) {
        setSaves((s) => ({
          ...s,
          [inv.investigation_id]: {
            phase: "failed",
            message: `${label}: not a valid ISO-8601 timestamp`,
          },
        }));
        return;
      }
    }
    void tracked(
      inv.investigation_id,
      async () => {
        await api.updateInvestigation({
          investigation_id: inv.investigation_id,
          expected_revision: inv.revision,
          title: draft.title.trim(),
          description: draft.description.trim() || null,
          severity: draft.severity || null,
          owner_text: draft.owner_text.trim() || null,
          tags: draft.tags
            .split(",")
            .map((t) => t.trim())
            .filter(Boolean),
          incident_started_at: isoToNs(draft.incident_started_at) as number | null,
          mitigated_at: isoToNs(draft.mitigated_at) as number | null,
          resolved_at: isoToNs(draft.resolved_at) as number | null,
          window_start: isoToNs(draft.window_start) as number | null,
          window_end: isoToNs(draft.window_end) as number | null,
        });
      },
      reloadSelected,
    );
  };

  const doSetStatus = (status: string) => {
    if (!bundle) return;
    const inv = bundle.investigation;
    void tracked(
      inv.investigation_id,
      async () => {
        await api.setInvestigationStatus(
          inv.investigation_id,
          inv.revision,
          status,
        );
      },
      reloadSelected,
    );
  };

  const visibleItems = bundle
    ? bundle.items.filter(
        (it) =>
          (showArchivedItems || !it.archived) &&
          (itemFilter === "all" || it.kind === itemFilter),
      )
    : [];

  const moveItem = (itemId: string, dir: -1 | 1) => {
    if (!bundle) return;
    // The repository requires the full id list, archived included.
    const ordered = bundle.items.map((i) => i.item_id);
    const from = ordered.indexOf(itemId);
    const to = from + dir;
    if (from < 0 || to < 0 || to >= ordered.length) return;
    [ordered[from], ordered[to]] = [ordered[to], ordered[from]];
    void tracked(
      itemId,
      async () => {
        await api.reorderCaseChildren(
          bundle.investigation.investigation_id,
          bundle.investigation.revision,
          "item",
          ordered,
        );
      },
      reloadSelected,
    );
  };

  return (
    <div className="case-root">
      <div className="row case-header">
        <button onClick={onBack} aria-label="Back to workspace overview">
          ← Back
        </button>
        <h2>Investigations</h2>
        {error && <div className="error">{error}</div>}
      </div>

      <div className="case-layout">
        <aside className="case-sidebar" aria-label="Investigation list">
          <div className="row">
            <input
              value={newTitle}
              onChange={(e) => setNewTitle(e.target.value)}
              placeholder="new investigation title"
              aria-label="New investigation title"
              onKeyDown={(e) => {
                if (e.key === "Enter") doCreate();
              }}
            />
            <button onClick={doCreate} disabled={!newTitle.trim()}>
              Create
            </button>
          </div>
          <SaveBadge state={saves["new-investigation"]} />
          <label className="row dim">
            <input
              type="checkbox"
              checked={includeArchived}
              onChange={(e) => setIncludeArchived(e.target.checked)}
            />
            show archived
          </label>
          <ul className="case-list">
            {list.map((inv) => (
              <li key={inv.investigation_id}>
                <button
                  className={
                    inv.investigation_id === selectedId ? "link selected" : "link"
                  }
                  aria-current={inv.investigation_id === selectedId}
                  onClick={() => setSelectedId(inv.investigation_id)}
                >
                  <span className={`status-chip status-${inv.status}`}>
                    {inv.status}
                  </span>{" "}
                  {inv.title}
                  {inv.severity ? (
                    <span className="dim"> · {inv.severity}</span>
                  ) : null}
                </button>
              </li>
            ))}
            {list.length === 0 && (
              <li className="dim">no investigations yet</li>
            )}
          </ul>
        </aside>

        {bundle && draft ? (
          <section className="case-detail">
            <div className="row">
              <h3>{bundle.investigation.title}</h3>
              <SaveBadge state={saves[bundle.investigation.investigation_id]} />
              {saves[bundle.investigation.investigation_id]?.phase ===
                "conflicted" && (
                <button onClick={() => void reloadSelected()}>Reload</button>
              )}
            </div>

            <div className="row">
              <span className="dim">status: {bundle.investigation.status}</span>
              {INVESTIGATION_STATUSES.filter(
                (s) => s !== bundle.investigation.status,
              ).map((s) => (
                <button key={s} onClick={() => doSetStatus(s)}>
                  {s === "archived" ? "archive" : `mark ${s}`}
                </button>
              ))}
            </div>

            <div className="case-form">
              <label>
                Title
                <input
                  value={draft.title}
                  onChange={(e) => setDraft({ ...draft, title: e.target.value })}
                  aria-label="Investigation title"
                />
              </label>
              <label>
                Description
                <textarea
                  value={draft.description}
                  onChange={(e) =>
                    setDraft({ ...draft, description: e.target.value })
                  }
                  rows={3}
                  aria-label="Investigation description"
                />
              </label>
              <div className="row">
                <label>
                  Severity
                  <select
                    value={draft.severity}
                    onChange={(e) =>
                      setDraft({ ...draft, severity: e.target.value })
                    }
                    aria-label="Severity"
                  >
                    <option value="">(none)</option>
                    {SEVERITIES.map((s) => (
                      <option key={s} value={s}>
                        {s}
                      </option>
                    ))}
                  </select>
                </label>
                <label>
                  Owner
                  <input
                    value={draft.owner_text}
                    onChange={(e) =>
                      setDraft({ ...draft, owner_text: e.target.value })
                    }
                    placeholder="free text, no OS identity"
                    aria-label="Owner"
                  />
                </label>
                <label>
                  Tags
                  <input
                    value={draft.tags}
                    onChange={(e) => setDraft({ ...draft, tags: e.target.value })}
                    placeholder="comma, separated"
                    aria-label="Tags"
                  />
                </label>
              </div>
              <div className="row">
                {(
                  [
                    ["Incident started", "incident_started_at"],
                    ["Mitigated", "mitigated_at"],
                    ["Resolved", "resolved_at"],
                  ] as const
                ).map(([label, key]) => (
                  <label key={key}>
                    {label} (UTC)
                    <input
                      value={draft[key]}
                      onChange={(e) =>
                        setDraft({ ...draft, [key]: e.target.value })
                      }
                      placeholder="2026-08-04T10:00:00Z"
                      aria-label={`${label} timestamp`}
                    />
                  </label>
                ))}
              </div>
              <div className="row">
                {(
                  [
                    ["Window start", "window_start"],
                    ["Window end", "window_end"],
                  ] as const
                ).map(([label, key]) => (
                  <label key={key}>
                    {label} (UTC)
                    <input
                      value={draft[key]}
                      onChange={(e) =>
                        setDraft({ ...draft, [key]: e.target.value })
                      }
                      placeholder="2026-08-04T10:00:00Z"
                      aria-label={label}
                    />
                  </label>
                ))}
                <button onClick={doSaveInvestigation}>Save changes</button>
              </div>
            </div>

            <h4>Hypotheses</h4>
            <div className="row">
              <input
                value={newHypStatement}
                onChange={(e) => setNewHypStatement(e.target.value)}
                placeholder="hypothesis statement"
                aria-label="New hypothesis statement"
              />
              <input
                value={newHypRationale}
                onChange={(e) => setNewHypRationale(e.target.value)}
                placeholder="rationale (optional)"
                aria-label="New hypothesis rationale"
              />
              <button
                disabled={!newHypStatement.trim()}
                onClick={() =>
                  void tracked(
                    "new-hypothesis",
                    async () => {
                      await api.createHypothesis(
                        bundle.investigation.investigation_id,
                        newHypStatement.trim(),
                        newHypRationale.trim() || null,
                      );
                      setNewHypStatement("");
                      setNewHypRationale("");
                    },
                    reloadSelected,
                  )
                }
              >
                Add hypothesis
              </button>
              <SaveBadge state={saves["new-hypothesis"]} />
            </div>
            <ul className="case-cards">
              {bundle.hypotheses.map((h) => {
                const d = hypDrafts[h.hypothesis_id];
                return (
                  <li key={h.hypothesis_id} className="case-card">
                    {d ? (
                      <>
                        <textarea
                          value={d.statement}
                          rows={2}
                          aria-label="Hypothesis statement"
                          onChange={(e) =>
                            setHypDrafts((s) => ({
                              ...s,
                              [h.hypothesis_id]: { ...d, statement: e.target.value },
                            }))
                          }
                        />
                        <textarea
                          value={d.rationale}
                          rows={2}
                          placeholder="rationale"
                          aria-label="Hypothesis rationale"
                          onChange={(e) =>
                            setHypDrafts((s) => ({
                              ...s,
                              [h.hypothesis_id]: { ...d, rationale: e.target.value },
                            }))
                          }
                        />
                        <div className="row">
                          <button
                            onClick={() =>
                              void tracked(
                                h.hypothesis_id,
                                async () => {
                                  await api.updateHypothesis(
                                    h.hypothesis_id,
                                    h.revision,
                                    d.statement.trim(),
                                    d.rationale.trim() || null,
                                  );
                                  setHypDrafts((s) => {
                                    const { [h.hypothesis_id]: _, ...rest } = s;
                                    return rest;
                                  });
                                },
                                reloadSelected,
                              )
                            }
                          >
                            Save
                          </button>
                          <button
                            onClick={() =>
                              setHypDrafts((s) => {
                                const { [h.hypothesis_id]: _, ...rest } = s;
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
                        <div className="row">
                          <strong>{h.statement}</strong>
                          <SaveBadge state={saves[h.hypothesis_id]} />
                        </div>
                        {h.rationale && <div className="dim">{h.rationale}</div>}
                        <div className="row">
                          <label>
                            state
                            <select
                              value={h.state}
                              aria-label="Hypothesis state"
                              onChange={(e) =>
                                void tracked(
                                  h.hypothesis_id,
                                  async () => {
                                    await api.setHypothesisState(
                                      h.hypothesis_id,
                                      h.revision,
                                      e.target.value,
                                    );
                                  },
                                  reloadSelected,
                                )
                              }
                            >
                              {HYPOTHESIS_STATES.map((s) => (
                                <option key={s} value={s}>
                                  {s}
                                </option>
                              ))}
                            </select>
                          </label>
                          <span className="dim">
                            {h.linked_evidence_ids.length} linked evidence
                          </span>
                          <button
                            onClick={() =>
                              setHypDrafts((s) => ({
                                ...s,
                                [h.hypothesis_id]: {
                                  statement: h.statement,
                                  rationale: h.rationale ?? "",
                                },
                              }))
                            }
                          >
                            Edit
                          </button>
                        </div>
                      </>
                    )}
                  </li>
                );
              })}
              {bundle.hypotheses.length === 0 && (
                <li className="dim">no hypotheses yet</li>
              )}
            </ul>

            <h4>Notes, tasks, findings, questions</h4>
            <div className="row">
              <select
                value={newItemKind}
                onChange={(e) => setNewItemKind(e.target.value)}
                aria-label="New item kind"
              >
                {ITEM_KINDS.map((k) => (
                  <option key={k} value={k}>
                    {k}
                  </option>
                ))}
              </select>
              <input
                value={newItemContent}
                onChange={(e) => setNewItemContent(e.target.value)}
                placeholder="content"
                aria-label="New item content"
                onKeyDown={(e) => {
                  if (e.key === "Enter" && newItemContent.trim()) {
                    e.preventDefault();
                    void tracked(
                      "new-item",
                      async () => {
                        await api.createItem({
                          investigation_id: bundle.investigation.investigation_id,
                          kind: newItemKind,
                          content: newItemContent.trim(),
                          task_status: newItemKind === "task" ? "todo" : null,
                          question_status:
                            newItemKind === "question" ? "open" : null,
                        });
                        setNewItemContent("");
                      },
                      reloadSelected,
                    );
                  }
                }}
              />
              <button
                disabled={!newItemContent.trim()}
                onClick={() =>
                  void tracked(
                    "new-item",
                    async () => {
                      await api.createItem({
                        investigation_id: bundle.investigation.investigation_id,
                        kind: newItemKind,
                        content: newItemContent.trim(),
                        task_status: newItemKind === "task" ? "todo" : null,
                        question_status:
                          newItemKind === "question" ? "open" : null,
                      });
                      setNewItemContent("");
                    },
                    reloadSelected,
                  )
                }
              >
                Add
              </button>
              <SaveBadge state={saves["new-item"]} />
            </div>
            <div className="row" role="tablist" aria-label="Item kind filter">
              {["all", ...ITEM_KINDS].map((k) => (
                <button
                  key={k}
                  role="tab"
                  aria-selected={itemFilter === k}
                  className={itemFilter === k ? "selected" : ""}
                  onClick={() => setItemFilter(k)}
                >
                  {k}
                </button>
              ))}
              <label className="dim">
                <input
                  type="checkbox"
                  checked={showArchivedItems}
                  onChange={(e) => setShowArchivedItems(e.target.checked)}
                />
                show archived
              </label>
            </div>
            <ul className="case-cards">
              {visibleItems.map((it) => {
                const editing = itemDrafts[it.item_id];
                return (
                  <li
                    key={it.item_id}
                    className={`case-card${it.archived ? " archived" : ""}`}
                  >
                    <div className="row">
                      <span className={`kind-chip kind-${it.kind}`}>
                        {it.kind}
                      </span>
                      {it.kind === "task" && (
                        <select
                          value={it.task_status ?? "todo"}
                          aria-label="Task status"
                          onChange={(e) =>
                            void tracked(
                              it.item_id,
                              async () => {
                                await api.setItemStatus(
                                  it.item_id,
                                  it.revision,
                                  e.target.value,
                                  null,
                                );
                              },
                              reloadSelected,
                            )
                          }
                        >
                          {TASK_STATUSES.map((s) => (
                            <option key={s} value={s}>
                              {s}
                            </option>
                          ))}
                        </select>
                      )}
                      {it.kind === "question" && (
                        <select
                          value={it.question_status ?? "open"}
                          aria-label="Question status"
                          onChange={(e) =>
                            void tracked(
                              it.item_id,
                              async () => {
                                await api.setItemStatus(
                                  it.item_id,
                                  it.revision,
                                  null,
                                  e.target.value,
                                );
                              },
                              reloadSelected,
                            )
                          }
                        >
                          {QUESTION_STATUSES.map((s) => (
                            <option key={s} value={s}>
                              {s}
                            </option>
                          ))}
                        </select>
                      )}
                      <SaveBadge state={saves[it.item_id]} />
                      <span className="spacer" />
                      <button
                        aria-label="Move item up"
                        onClick={() => moveItem(it.item_id, -1)}
                      >
                        ↑
                      </button>
                      <button
                        aria-label="Move item down"
                        onClick={() => moveItem(it.item_id, 1)}
                      >
                        ↓
                      </button>
                      <button
                        title="Pin this item as evidence (captures its current revision)"
                        onClick={() =>
                          void tracked(
                            it.item_id,
                            async () => {
                              await api.pinItem({
                                common: {
                                  investigation_id:
                                    bundle.investigation.investigation_id,
                                  title: `${it.kind}: ${it.content.slice(0, 60)}`,
                                  annotation: null,
                                  relevance: null,
                                  group_id: null,
                                },
                                item_id: it.item_id,
                              });
                            },
                            reloadSelected,
                          )
                        }
                      >
                        📌 pin
                      </button>
                      <button
                        onClick={() =>
                          void tracked(
                            it.item_id,
                            async () => {
                              await api.setItemArchived(
                                it.item_id,
                                it.revision,
                                !it.archived,
                              );
                            },
                            reloadSelected,
                          )
                        }
                      >
                        {it.archived ? "restore" : "archive"}
                      </button>
                    </div>
                    {editing !== undefined ? (
                      <>
                        <textarea
                          value={editing}
                          rows={3}
                          aria-label="Item content"
                          onChange={(e) =>
                            setItemDrafts((s) => ({
                              ...s,
                              [it.item_id]: e.target.value,
                            }))
                          }
                        />
                        <div className="row">
                          <button
                            onClick={() =>
                              void tracked(
                                it.item_id,
                                async () => {
                                  await api.updateItemContent(
                                    it.item_id,
                                    it.revision,
                                    editing,
                                  );
                                  setItemDrafts((s) => {
                                    const { [it.item_id]: _, ...rest } = s;
                                    return rest;
                                  });
                                },
                                reloadSelected,
                              )
                            }
                          >
                            Save
                          </button>
                          <button
                            onClick={() =>
                              setItemDrafts((s) => {
                                const { [it.item_id]: _, ...rest } = s;
                                return rest;
                              })
                            }
                          >
                            Cancel
                          </button>
                        </div>
                      </>
                    ) : (
                      <div
                        className="item-content"
                        onDoubleClick={() =>
                          setItemDrafts((s) => ({
                            ...s,
                            [it.item_id]: it.content,
                          }))
                        }
                      >
                        {it.content}{" "}
                        <button
                          className="link dim"
                          aria-label="Edit item content"
                          onClick={() =>
                            setItemDrafts((s) => ({
                              ...s,
                              [it.item_id]: it.content,
                            }))
                          }
                        >
                          edit
                        </button>
                      </div>
                    )}
                  </li>
                );
              })}
              {visibleItems.length === 0 && <li className="dim">no items</li>}
            </ul>

            <EvidencePanel
              bundle={bundle}
              saves={saves}
              tracked={tracked}
              reload={reloadSelected}
              onJumpToExplorer={onJumpToExplorer}
            />

            <h4>Activity</h4>
            <ul className="activity">
              {activity.map((a) => (
                <li key={a.history_id}>
                  <span className="dim">{a.created_at}</span> {a.entity_kind}{" "}
                  {a.action}{" "}
                  <span className="dim">
                    ({a.entity_id.slice(0, 12)}… rev {a.revision})
                  </span>
                </li>
              ))}
              {activity.length === 0 && <li className="dim">no activity</li>}
            </ul>
          </section>
        ) : (
          <section className="case-detail dim">
            Select or create an investigation.
          </section>
        )}
      </div>
    </div>
  );
}
