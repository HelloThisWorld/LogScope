// v0.3 pin dialog: the one place Explorer state becomes evidence. It
// shows exactly what will be captured (kind, target, scope, bounds) and
// sends the capture through the authoritative pin services — nothing is
// computed client-side.

import { useEffect, useState } from "react";
import { api, errorText } from "./api";
import type {
  EvidenceGroupDto,
  InvestigationDto,
  TimeStrategyDto,
} from "./api";

export type PinScope = {
  queryText: string;
  datasetIds: string[];
  timeStrategy: TimeStrategyDto;
};

export type PinTarget =
  | {
      kind: "event";
      datasetId: string;
      recordId: string;
      displayFields: string[];
    }
  | { kind: "selection"; recordIds: string[]; scope: PinScope }
  | { kind: "query"; scope: PinScope; savedSearchId: string | null }
  | {
      kind: "group";
      scope: PinScope;
      field: string;
      valueJson: string;
      label: string;
    }
  | {
      kind: "interval";
      scope: PinScope;
      start: number;
      end: number;
      bucketWidthNanos: number;
      displayTimezone: string;
      neighborBuckets: [number, number][];
    };

function defaultTitle(t: PinTarget): string {
  switch (t.kind) {
    case "event":
      return `Event ${t.recordId.slice(0, 16)}…`;
    case "selection":
      return `${t.recordIds.length} selected events`;
    case "query":
      return t.scope.queryText.trim()
        ? `Query: ${t.scope.queryText.trim().slice(0, 60)}`
        : "Query: (match all)";
    case "group":
      return `${t.field} = ${t.label}`;
    case "interval":
      return `Interval ${new Date(t.start / 1e6).toISOString()} – ${new Date(
        t.end / 1e6,
      ).toISOString()}`;
  }
}

function strategyLabel(s: TimeStrategyDto): string {
  if (s.kind === "absolute" && s.start != null && s.end != null) {
    return `absolute ${new Date(Number(s.start) / 1e6).toISOString()} – ${new Date(
      Number(s.end) / 1e6,
    ).toISOString()}`;
  }
  if (s.kind === "relative_to_latest" && s.duration_nanos != null) {
    return `relative to newest (${Number(s.duration_nanos) / 1e9}s) — frozen to concrete bounds at pin time`;
  }
  return "all time";
}

export default function PinDialog({
  target,
  onClose,
  onPinned,
}: {
  target: PinTarget;
  onClose: () => void;
  onPinned: (message: string) => void;
}) {
  const [investigations, setInvestigations] = useState<InvestigationDto[]>([]);
  const [invId, setInvId] = useState("");
  const [groups, setGroups] = useState<EvidenceGroupDto[]>([]);
  const [groupId, setGroupId] = useState("");
  const [title, setTitle] = useState(defaultTitle(target));
  const [annotation, setAnnotation] = useState("");
  const [relevance, setRelevance] = useState("");
  const [includeRaw, setIncludeRaw] = useState(true);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState("");

  useEffect(() => {
    api
      .listInvestigations(false)
      .then((list) => {
        setInvestigations(list);
        if (list.length === 1) setInvId(list[0].investigation_id);
      })
      .catch((e) => setError(errorText(e)));
  }, []);

  useEffect(() => {
    setGroups([]);
    setGroupId("");
    if (!invId) return;
    api
      .investigationBundle(invId)
      .then((b) => setGroups(b.groups))
      .catch(() => setGroups([]));
  }, [invId]);

  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") onClose();
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [onClose]);

  const doPin = async () => {
    if (!invId || !title.trim()) return;
    setBusy(true);
    setError("");
    const common = {
      investigation_id: invId,
      title: title.trim(),
      annotation: annotation.trim() || null,
      relevance: relevance.trim() || null,
      group_id: groupId || null,
    };
    const scopeDto = (s: PinScope) => ({
      query_text: s.queryText,
      dataset_ids: s.datasetIds,
      time_strategy: s.timeStrategy,
    });
    try {
      const ev = await (() => {
        switch (target.kind) {
          case "event":
            return api.pinEvent({
              common,
              dataset_id: target.datasetId,
              record_id: target.recordId,
              display_fields: target.displayFields,
              include_raw_excerpt: includeRaw,
            });
          case "selection":
            return api.pinSelection({
              common,
              record_ids: target.recordIds,
              scope: scopeDto(target.scope),
            });
          case "query":
            return api.pinQuery({
              common,
              scope: scopeDto(target.scope),
              saved_search_id: target.savedSearchId,
            });
          case "group":
            return api.pinGroup({
              common,
              scope: scopeDto(target.scope),
              field: target.field,
              value_json: target.valueJson,
            });
          case "interval":
            return api.pinInterval({
              common,
              scope: scopeDto(target.scope),
              start: target.start,
              end: target.end,
              bucket_width_nanos: target.bucketWidthNanos,
              display_timezone: target.displayTimezone,
              neighbor_buckets: target.neighborBuckets,
            });
        }
      })();
      onPinned(`pinned "${ev.title}" (${ev.kind}) — state ${ev.resolver_state}`);
      onClose();
    } catch (e) {
      setError(errorText(e));
    } finally {
      setBusy(false);
    }
  };

  const scope = "scope" in target ? target.scope : null;

  return (
    <div className="modal-overlay" onClick={onClose}>
      <div
        className="modal"
        role="dialog"
        aria-modal="true"
        aria-label="Pin as evidence"
        onClick={(e) => e.stopPropagation()}
      >
        <h3>Pin as evidence</h3>

        <div className="pin-summary">
          <div>
            <span className="dim">kind:</span> {target.kind}
          </div>
          {target.kind === "event" && (
            <>
              <div>
                <span className="dim">record:</span> {target.recordId}
              </div>
              <div>
                <span className="dim">dataset:</span> {target.datasetId}
              </div>
              <div>
                <span className="dim">captured fields:</span>{" "}
                {target.displayFields.join(", ") || "(canonical columns only)"}
              </div>
            </>
          )}
          {target.kind === "selection" && (
            <div>
              <span className="dim">records:</span> {target.recordIds.length}{" "}
              (bounded server-side; truncation is recorded, never silent)
            </div>
          )}
          {target.kind === "group" && (
            <div>
              <span className="dim">group:</span> {target.field} ={" "}
              {target.label}
            </div>
          )}
          {target.kind === "interval" && (
            <div>
              <span className="dim">interval:</span>{" "}
              {new Date(target.start / 1e6).toISOString()} –{" "}
              {new Date(target.end / 1e6).toISOString()} (half-open, UTC)
            </div>
          )}
          {scope && (
            <>
              <div>
                <span className="dim">query:</span>{" "}
                <code>{scope.queryText.trim() || "(match all)"}</code>
              </div>
              <div>
                <span className="dim">datasets:</span>{" "}
                {scope.datasetIds.length
                  ? `${scope.datasetIds.length} selected`
                  : "all published log datasets"}
              </div>
              <div>
                <span className="dim">time:</span>{" "}
                {strategyLabel(scope.timeStrategy)}
              </div>
            </>
          )}
          <div className="dim">
            Snapshots are bounded; anything cut off is marked as truncated.
          </div>
        </div>

        {error && <div className="error">{error}</div>}

        <label>
          Investigation
          <select
            value={invId}
            onChange={(e) => setInvId(e.target.value)}
            aria-label="Target investigation"
            autoFocus
          >
            <option value="">choose…</option>
            {investigations.map((i) => (
              <option key={i.investigation_id} value={i.investigation_id}>
                {i.title} ({i.status})
              </option>
            ))}
          </select>
        </label>
        {investigations.length === 0 && (
          <div className="dim">
            No active investigations — create one in the Investigations view
            first.
          </div>
        )}

        <label>
          Title
          <input
            value={title}
            onChange={(e) => setTitle(e.target.value)}
            aria-label="Evidence title"
          />
        </label>
        <label>
          Annotation
          <textarea
            value={annotation}
            onChange={(e) => setAnnotation(e.target.value)}
            rows={2}
            placeholder="optional note"
            aria-label="Evidence annotation"
          />
        </label>
        <label>
          Why is this relevant?
          <textarea
            value={relevance}
            onChange={(e) => setRelevance(e.target.value)}
            rows={2}
            placeholder="optional relevance explanation"
            aria-label="Evidence relevance"
          />
        </label>
        {groups.length > 0 && (
          <label>
            Group
            <select
              value={groupId}
              onChange={(e) => setGroupId(e.target.value)}
              aria-label="Evidence group"
            >
              <option value="">(none)</option>
              {groups.map((g) => (
                <option key={g.group_id} value={g.group_id}>
                  {g.name}
                </option>
              ))}
            </select>
          </label>
        )}
        {target.kind === "event" && (
          <label className="row">
            <input
              type="checkbox"
              checked={includeRaw}
              onChange={(e) => setIncludeRaw(e.target.checked)}
            />
            capture bounded raw source excerpt
          </label>
        )}

        <div className="row">
          <button onClick={doPin} disabled={busy || !invId || !title.trim()}>
            {busy ? "pinning…" : "Pin"}
          </button>
          <button onClick={onClose} disabled={busy}>
            Cancel
          </button>
        </div>
      </div>
    </div>
  );
}
