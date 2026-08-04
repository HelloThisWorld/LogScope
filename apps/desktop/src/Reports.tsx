// v0.3 report builder: definitions (sections + exact-revision evidence/
// marker selection), generation to deterministic Markdown or
// self-contained HTML, and the immutable artifact list. Selection
// captures the revision visible at click time; generation renders that
// exact revision or says why it cannot.

import { useCallback, useEffect, useState } from "react";
import { save as saveDialog } from "@tauri-apps/plugin-dialog";
import {
  api,
  errorText,
  NARRATIVE_SECTION_KINDS,
  REPORT_SECTION_KINDS,
} from "./api";
import type {
  InvestigationBundleDto,
  ReportArtifactDto,
  ReportDefDto,
  SectionDto,
  SelectedRefDto,
} from "./api";
import type { SaveState } from "./Case";

type Narrative = (typeof NARRATIVE_SECTION_KINDS)[number];

function isNarrative(kind: string): kind is Narrative {
  return (NARRATIVE_SECTION_KINDS as readonly string[]).includes(kind);
}

type Draft = {
  title: string;
  subtitle: string;
  /// kind -> included; narrative kinds also carry text.
  included: Record<string, boolean>;
  narrative: Record<string, string>;
  evidence: Record<string, number>; // id -> selected revision
  markers: Record<string, number>;
};

function draftFromDef(def: ReportDefDto): Draft {
  const included: Record<string, boolean> = {};
  const narrative: Record<string, string> = {};
  for (const s of def.sections) {
    included[s.kind] = true;
    if (isNarrative(s.kind)) narrative[s.kind] = s.content ?? "";
  }
  const evidence: Record<string, number> = {};
  for (const r of def.selected_evidence) evidence[r.id] = r.revision;
  const markers: Record<string, number> = {};
  for (const r of def.selected_markers) markers[r.id] = r.revision;
  return {
    title: def.title,
    subtitle: def.subtitle ?? "",
    included,
    narrative,
    evidence,
    markers,
  };
}

function emptyDraft(): Draft {
  const included: Record<string, boolean> = {};
  for (const k of REPORT_SECTION_KINDS) included[k] = true;
  return {
    title: "",
    subtitle: "",
    included,
    narrative: {},
    evidence: {},
    markers: {},
  };
}

function draftSections(d: Draft): SectionDto[] {
  return REPORT_SECTION_KINDS.filter((k) => d.included[k]).map((k) => ({
    kind: k,
    content: isNarrative(k) ? (d.narrative[k] ?? "") : null,
  }));
}

function refs(sel: Record<string, number>): SelectedRefDto[] {
  return Object.entries(sel)
    .sort(([a], [b]) => a.localeCompare(b))
    .map(([id, revision]) => ({ id, revision }));
}

export default function Reports({
  bundle,
  saves,
  tracked,
}: {
  bundle: InvestigationBundleDto;
  saves: Record<string, SaveState>;
  tracked: (
    id: string,
    work: () => Promise<void>,
    after?: () => Promise<void>,
  ) => Promise<void>;
}) {
  const invId = bundle.investigation.investigation_id;
  const [defs, setDefs] = useState<ReportDefDto[]>([]);
  const [artifacts, setArtifacts] = useState<ReportArtifactDto[]>([]);
  const [editingId, setEditingId] = useState<string | null>(null); // null = closed, "" = new
  const [draft, setDraft] = useState<Draft>(emptyDraft());
  const [status, setStatus] = useState("");

  const load = useCallback(async () => {
    try {
      setDefs(await api.listReportDefs(invId));
      setArtifacts(await api.listReportArtifacts(invId));
    } catch (e) {
      setStatus(errorText(e));
    }
  }, [invId]);

  useEffect(() => {
    void load();
  }, [load]);

  const saveDef = () => {
    const isNew = editingId === "";
    const existing = defs.find((d) => d.report_def_id === editingId);
    void tracked(
      isNew ? "new-report" : (editingId as string),
      async () => {
        if (!draft.title.trim())
          throw { code: "ui/blank-title", message: "title must not be blank" };
        if (isNew) {
          await api.createReportDef({
            investigation_id: invId,
            title: draft.title.trim(),
            subtitle: draft.subtitle.trim() || null,
            sections: draftSections(draft),
            selected_evidence: refs(draft.evidence),
            selected_markers: refs(draft.markers),
          });
        } else if (existing) {
          await api.updateReportDef({
            report_def_id: existing.report_def_id,
            expected_revision: existing.revision,
            title: draft.title.trim(),
            subtitle: draft.subtitle.trim() || null,
            sections: draftSections(draft),
            selected_evidence: refs(draft.evidence),
            selected_markers: refs(draft.markers),
          });
        }
        setEditingId(null);
      },
      load,
    );
  };

  const generate = async (def: ReportDefDto, format: "markdown" | "html") => {
    const ext = format === "markdown" ? "md" : "html";
    const dest = (await saveDialog({
      title: `Save ${format} report`,
      defaultPath: `${def.title.replace(/[^\w.-]+/g, "_")}.${ext}`,
      filters: [{ name: format, extensions: [ext] }],
    })) as string | null;
    if (!dest) return;
    try {
      const art = await api.generateReport(def.report_def_id, format, dest);
      setStatus(
        `generated ${art.format} · ${art.byte_size} bytes · sha256 ${art.checksum_sha256?.slice(0, 16)}…`,
      );
    } catch (e) {
      setStatus(errorText(e));
    }
    await load();
  };

  return (
    <>
      <h4>Reports</h4>
      {status && (
        <div className="dim" role="status">
          {status}
        </div>
      )}

      <div className="row">
        <button
          onClick={() => {
            setEditingId("");
            setDraft(emptyDraft());
          }}
        >
          New report definition
        </button>
      </div>

      <ul className="case-cards">
        {defs.map((d) => (
          <li key={d.report_def_id} className="case-card">
            <div className="row">
              <strong>{d.title}</strong>
              {d.subtitle && <span className="dim">{d.subtitle}</span>}
              <span className="dim">
                {d.sections.length} sections · {d.selected_evidence.length}{" "}
                evidence · {d.selected_markers.length} markers · rev{" "}
                {d.revision}
              </span>
              <SaveBadge state={saves[d.report_def_id]} />
              <span className="spacer" />
              <button onClick={() => void generate(d, "markdown")}>
                Generate Markdown
              </button>
              <button onClick={() => void generate(d, "html")}>
                Generate HTML
              </button>
              <button
                onClick={() => {
                  setEditingId(d.report_def_id);
                  setDraft(draftFromDef(d));
                }}
              >
                edit
              </button>
            </div>
          </li>
        ))}
        {defs.length === 0 && <li className="dim">no report definitions</li>}
      </ul>

      {editingId !== null && (
        <div className="case-form">
          <div className="row">
            <label>
              Title
              <input
                value={draft.title}
                onChange={(e) => setDraft({ ...draft, title: e.target.value })}
                aria-label="Report title"
              />
            </label>
            <label>
              Subtitle
              <input
                value={draft.subtitle}
                onChange={(e) =>
                  setDraft({ ...draft, subtitle: e.target.value })
                }
                aria-label="Report subtitle"
              />
            </label>
            <SaveBadge state={saves[editingId === "" ? "new-report" : editingId]} />
          </div>

          <p className="dim">
            Sections render in canonical order. A blank narrative renders the
            explicit word "unknown" — nothing is ever synthesized.
          </p>
          {REPORT_SECTION_KINDS.map((k) => (
            <div key={k} className="row report-section-row">
              <label className="dim">
                <input
                  type="checkbox"
                  checked={!!draft.included[k]}
                  onChange={(e) =>
                    setDraft({
                      ...draft,
                      included: { ...draft.included, [k]: e.target.checked },
                    })
                  }
                />
                {k}
              </label>
              {isNarrative(k) && draft.included[k] && (
                <textarea
                  value={draft.narrative[k] ?? ""}
                  rows={2}
                  placeholder={`${k} — leave blank to render "unknown"`}
                  aria-label={`${k} narrative`}
                  onChange={(e) =>
                    setDraft({
                      ...draft,
                      narrative: { ...draft.narrative, [k]: e.target.value },
                    })
                  }
                />
              )}
              {!isNarrative(k) && (
                <span className="dim">(projection of selected data)</span>
              )}
            </div>
          ))}

          <p className="dim">
            Selection captures the revision shown; the report renders exactly
            that revision or says why it cannot.
          </p>
          <div className="row">
            <div className="report-pick">
              <strong>Evidence</strong>
              {bundle.evidence
                .filter((ev) => !ev.archived)
                .map((ev) => (
                  <label key={ev.evidence_id} className="dim">
                    <input
                      type="checkbox"
                      checked={ev.evidence_id in draft.evidence}
                      onChange={(e) => {
                        const next = { ...draft.evidence };
                        if (e.target.checked)
                          next[ev.evidence_id] = ev.revision;
                        else delete next[ev.evidence_id];
                        setDraft({ ...draft, evidence: next });
                      }}
                    />
                    {ev.title} (r{ev.revision})
                  </label>
                ))}
              {bundle.evidence.filter((e) => !e.archived).length === 0 && (
                <span className="dim">none</span>
              )}
            </div>
            <div className="report-pick">
              <strong>Markers</strong>
              {bundle.markers
                .filter((m) => !m.archived)
                .map((m) => (
                  <label key={m.marker_id} className="dim">
                    <input
                      type="checkbox"
                      checked={m.marker_id in draft.markers}
                      onChange={(e) => {
                        const next = { ...draft.markers };
                        if (e.target.checked) next[m.marker_id] = m.revision;
                        else delete next[m.marker_id];
                        setDraft({ ...draft, markers: next });
                      }}
                    />
                    {m.label} (r{m.revision})
                  </label>
                ))}
              {bundle.markers.filter((m) => !m.archived).length === 0 && (
                <span className="dim">none</span>
              )}
            </div>
          </div>

          <div className="row">
            <button onClick={saveDef} disabled={!draft.title.trim()}>
              Save definition
            </button>
            <button onClick={() => setEditingId(null)}>Cancel</button>
          </div>
        </div>
      )}

      {artifacts.length > 0 && (
        <>
          <h5>Generated artifacts</h5>
          <ul className="activity">
            {artifacts.map((a) => (
              <li key={a.artifact_id}>
                <span className={`status-chip status-${a.status}`}>
                  {a.status}
                </span>{" "}
                {a.format} · <span className="mono">{a.destination_path}</span>
                {a.byte_size !== null && ` · ${a.byte_size} bytes`}
                {a.checksum_sha256 && (
                  <span className="dim mono">
                    {" "}
                    · sha256 {a.checksum_sha256.slice(0, 16)}…
                  </span>
                )}
                <span className="dim"> · {a.created_at}</span>
              </li>
            ))}
          </ul>
        </>
      )}
    </>
  );
}

function SaveBadge({ state }: { state: SaveState | undefined }) {
  if (!state) return null;
  const label =
    state.phase === "pending"
      ? "saving…"
      : state.phase === "saved"
        ? "saved"
        : state.phase === "conflicted"
          ? "conflicted"
          : `failed: ${state.message ?? ""}`;
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
