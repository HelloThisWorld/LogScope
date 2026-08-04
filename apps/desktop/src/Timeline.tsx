// v0.3 investigation timeline: manual markers merged with pinned
// evidence in the deterministic order the Rust read model produces. The
// undated section is explicit and every undated entry states why it is
// there. Markers preserve the timestamp text exactly as entered.

import { useCallback, useEffect, useState } from "react";
import { api, errorText, MARKER_KINDS } from "./api";
import type {
  InvestigationBundleDto,
  TimelineDto,
  TimelineEntryDto,
} from "./api";
import type { SaveState } from "./Case";

function fmtNs(ns: number | null): string {
  if (ns === null) return "";
  return new Date(ns / 1e6).toISOString().replace("T", " ").replace(".000Z", "Z");
}

function EntryRow({ e }: { e: TimelineEntryDto }) {
  return (
    <li className={`tl-entry tl-${e.entry_kind}`}>
      <span className="tl-time mono">
        {e.at_nanos !== null ? (
          <>
            {fmtNs(e.at_nanos)}
            {e.end_nanos !== null && <> → {fmtNs(e.end_nanos)}</>}
          </>
        ) : (
          "—"
        )}
      </span>
      <span className={`kind-chip kind-${e.detail_kind}`}>{e.detail_kind}</span>
      <span className="tl-title">
        {e.title}
        {e.description && <span className="dim"> — {e.description}</span>}
        {e.original_time_text && (
          <span
            className="dim"
            title="Timestamp exactly as entered, with its original zone offset"
          >
            {" "}
            (entered: {e.original_time_text})
          </span>
        )}
        {e.undated_reason && (
          <span className="dim"> · {e.undated_reason}</span>
        )}
      </span>
    </li>
  );
}

export default function Timeline({
  bundle,
  saves,
  tracked,
  reload,
}: {
  bundle: InvestigationBundleDto;
  saves: Record<string, SaveState>;
  tracked: (
    id: string,
    work: () => Promise<void>,
    after?: () => Promise<void>,
  ) => Promise<void>;
  reload: () => Promise<void>;
}) {
  const invId = bundle.investigation.investigation_id;
  const [model, setModel] = useState<TimelineDto | null>(null);
  const [error, setError] = useState("");
  const [showMarkers, setShowMarkers] = useState(false);

  const [kind, setKind] = useState<string>("deployment");
  const [label, setLabel] = useState("");
  const [description, setDescription] = useState("");
  const [timeText, setTimeText] = useState("");
  const [endText, setEndText] = useState("");

  const load = useCallback(async () => {
    try {
      setModel(await api.investigationTimeline(invId));
      setError("");
    } catch (e) {
      setError(errorText(e));
    }
  }, [invId]);

  useEffect(() => {
    void load();
  }, [load, bundle]);

  const addMarker = () =>
    void tracked(
      "new-marker",
      async () => {
        await api.createMarker({
          investigation_id: invId,
          kind,
          label: label.trim(),
          description: description.trim() || null,
          time_text: timeText.trim() || null,
          end_time_text: endText.trim() || null,
        });
        setLabel("");
        setDescription("");
        setTimeText("");
        setEndText("");
      },
      async () => {
        await reload();
        await load();
      },
    );

  return (
    <>
      <h4>Timeline</h4>
      {error && <div className="error">{error}</div>}

      <div className="row">
        <select
          value={kind}
          onChange={(e) => setKind(e.target.value)}
          aria-label="Marker kind"
        >
          {MARKER_KINDS.map((k) => (
            <option key={k} value={k}>
              {k}
            </option>
          ))}
        </select>
        <input
          value={label}
          onChange={(e) => setLabel(e.target.value)}
          placeholder="marker label"
          aria-label="Marker label"
        />
        <input
          value={timeText}
          onChange={(e) => setTimeText(e.target.value)}
          placeholder="2026-08-04T10:00:00Z (blank = undated)"
          aria-label="Marker time (RFC 3339)"
          size={30}
        />
        <input
          value={endText}
          onChange={(e) => setEndText(e.target.value)}
          placeholder="interval end (optional)"
          aria-label="Marker interval end (RFC 3339, exclusive)"
          size={24}
        />
        <input
          value={description}
          onChange={(e) => setDescription(e.target.value)}
          placeholder="description (optional)"
          aria-label="Marker description"
        />
        <button onClick={addMarker} disabled={!label.trim()}>
          Add marker
        </button>
        <MarkerSaveBadge state={saves["new-marker"]} />
      </div>
      <p className="dim">
        Markers are always manual — deployments and changes are never
        inferred from log text. Offsets are preserved as entered; the
        timeline axis is UTC.
      </p>

      {model && (
        <>
          <ul className="tl-list">
            {model.dated.map((e) => (
              <EntryRow key={`${e.entry_kind}-${e.id}`} e={e} />
            ))}
            {model.dated.length === 0 && (
              <li className="dim">no dated entries yet</li>
            )}
          </ul>
          {model.undated.length > 0 && (
            <>
              <h5>Undated</h5>
              <ul className="tl-list">
                {model.undated.map((e) => (
                  <EntryRow key={`${e.entry_kind}-${e.id}`} e={e} />
                ))}
              </ul>
            </>
          )}
          {model.archived_excluded > 0 && (
            <p className="dim">
              {model.archived_excluded} archived entr
              {model.archived_excluded === 1 ? "y" : "ies"} excluded from this
              view.
            </p>
          )}
        </>
      )}

      <div className="row">
        <button onClick={() => setShowMarkers((s) => !s)}>
          {showMarkers ? "hide marker management" : "manage markers"}
        </button>
      </div>
      {showMarkers && (
        <ul className="case-cards">
          {bundle.markers.map((m) => (
            <li
              key={m.marker_id}
              className={`case-card${m.archived ? " archived" : ""}`}
            >
              <div className="row">
                <span className={`kind-chip kind-${m.kind}`}>{m.kind}</span>
                <strong>{m.label}</strong>
                <span className="dim">
                  {m.at_nanos !== null ? fmtNs(m.at_nanos) : "undated"}
                  {m.original_time_text && ` (entered: ${m.original_time_text})`}
                </span>
                <MarkerSaveBadge state={saves[m.marker_id]} />
                <span className="spacer" />
                <button
                  onClick={() =>
                    void tracked(
                      m.marker_id,
                      async () => {
                        await api.setMarkerArchived(
                          m.marker_id,
                          m.revision,
                          !m.archived,
                        );
                      },
                      async () => {
                        await reload();
                        await load();
                      },
                    )
                  }
                >
                  {m.archived ? "restore" : "archive"}
                </button>
              </div>
              {m.description && <div className="dim">{m.description}</div>}
            </li>
          ))}
          {bundle.markers.length === 0 && (
            <li className="dim">no markers yet</li>
          )}
        </ul>
      )}
    </>
  );
}

function MarkerSaveBadge({ state }: { state: SaveState | undefined }) {
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
