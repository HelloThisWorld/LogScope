//! Deterministic report generation (v0.3 W5).
//!
//! A report renders a *definition* — chosen sections, chosen evidence at
//! exact captured revisions, chosen markers — into one artifact. The
//! rules, in order of importance:
//!
//! - **Nothing is synthesized.** Narrative sections carry user-authored
//!   text; a blank narrative renders the explicit word `unknown` so
//!   silence can never be mistaken for completeness. Projection sections
//!   (timeline / hypotheses / evidence) render stored data only.
//! - **Deterministic bytes.** The same definition revision over the same
//!   data produces byte-identical output: everything renders in a
//!   documented order, and no wall-clock time enters the document — the
//!   generation instant lives in the artifact record, not the bytes.
//! - **Exact captured revisions.** Selected evidence/markers name the
//!   revision the author chose. If the live row has moved on, the
//!   captured state is recovered from the history ledger and rendered
//!   with an explicit "as captured" label; if it cannot be recovered,
//!   that is stated too. Nothing is silently substituted.
//! - **Hostile log content stays inert.** Markdown escapes everything
//!   and sizes code fences past any backtick run in the content; HTML is
//!   one self-contained file with a restrictive CSP meta, zero scripts,
//!   zero links, and context-escaped text everywhere.

use std::collections::BTreeMap;
use std::io::Write as _;
use std::path::Path;

use logscope_jobs::JobError;
use logscope_workspace::{
    EvidenceRow, HypothesisRow, InvestigationRow, MarkerRow, ReportArtifactRow, ReportDefRow,
    Workspace,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::timeline::{evidence_entry, marker_entry, order_entries, TimelineEntry};

/// Section vocabulary in canonical render order.
pub const SECTION_KINDS: &[&str] = &[
    "summary",
    "impact",
    "symptoms",
    "timeline",
    "hypotheses",
    "evidence",
    "root_cause",
    "resolution",
    "validation",
    "follow_up",
];

const NARRATIVE_KINDS: &[&str] = &[
    "summary",
    "impact",
    "symptoms",
    "root_cause",
    "resolution",
    "validation",
    "follow_up",
];

/// One ordered section on a definition.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SectionDef {
    pub kind: String,
    /// Narrative text for narrative kinds; ignored for projections.
    #[serde(default)]
    pub content: Option<String>,
}

/// A selected child at an exact revision.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SelectedRef {
    #[serde(alias = "evidence_id", alias = "marker_id")]
    pub id: String,
    pub revision: i64,
}

/// How a selected row was materialized for rendering.
#[derive(Debug, Clone)]
enum Capture<T> {
    /// Live row already at the selected revision.
    Live(T),
    /// Live row moved on; state recovered from history at the exact
    /// selected revision (live revision noted for the label).
    FromHistory { row: T, live_revision: i64 },
    /// Selected revision unrecoverable; the reason is rendered.
    Unavailable { reason: String },
}

fn ws_err(e: logscope_workspace::WorkspaceError) -> JobError {
    JobError::new(e.code(), e.to_string())
}

fn bad_def(msg: impl std::fmt::Display) -> JobError {
    JobError::new("report/invalid-definition", msg.to_string())
}

/// Parses and validates sections_json: known kinds only, no duplicates.
pub fn parse_sections(sections_json: &str) -> Result<Vec<SectionDef>, JobError> {
    let sections: Vec<SectionDef> = serde_json::from_str(sections_json)
        .map_err(|e| bad_def(format!("sections_json does not parse: {e}")))?;
    let mut seen = std::collections::HashSet::new();
    for s in &sections {
        if !SECTION_KINDS.contains(&s.kind.as_str()) {
            return Err(bad_def(format!(
                "unknown section kind {:?} (expected one of {})",
                s.kind,
                SECTION_KINDS.join("|")
            )));
        }
        if !seen.insert(s.kind.clone()) {
            return Err(bad_def(format!("duplicate section kind {:?}", s.kind)));
        }
    }
    Ok(sections)
}

/// Everything a renderer needs, assembled once. Revisions used are
/// recorded into the artifact's snapshot metadata.
pub struct ReportSnapshot {
    investigation: InvestigationRow,
    def: ReportDefRow,
    sections: Vec<SectionDef>,
    hypotheses: Vec<HypothesisRow>,
    evidence: Vec<(SelectedRef, Capture<EvidenceRow>)>,
    markers: Vec<(SelectedRef, Capture<MarkerRow>)>,
}

/// Recovers an entity's state at an exact revision from the history
/// ledger (payload_json holds the full row after each action).
fn at_revision<T: serde::de::DeserializeOwned>(
    ws: &Workspace,
    entity_kind: &str,
    id: &str,
    revision: i64,
) -> Result<Option<T>, JobError> {
    let history = ws
        .meta
        .list_entity_history(entity_kind, id)
        .map_err(ws_err)?;
    for h in history {
        if h.revision == revision {
            if let Ok(row) = serde_json::from_str::<T>(&h.payload_json) {
                return Ok(Some(row));
            }
        }
    }
    Ok(None)
}

fn capture_evidence(ws: &Workspace, sel: &SelectedRef) -> Result<Capture<EvidenceRow>, JobError> {
    match ws.meta.get_evidence(&sel.id).map_err(ws_err)? {
        Some(live) if live.revision == sel.revision => Ok(Capture::Live(live)),
        Some(live) => match at_revision::<EvidenceRow>(ws, "evidence", &sel.id, sel.revision)? {
            Some(row) => Ok(Capture::FromHistory {
                row,
                live_revision: live.revision,
            }),
            None => Ok(Capture::Unavailable {
                reason: format!(
                    "selected revision {} is not recoverable (live revision {})",
                    sel.revision, live.revision
                ),
            }),
        },
        None => Ok(Capture::Unavailable {
            reason: "evidence no longer exists in this workspace".into(),
        }),
    }
}

fn capture_marker(ws: &Workspace, sel: &SelectedRef) -> Result<Capture<MarkerRow>, JobError> {
    // Markers have no single-row getter; the history ledger carries the
    // full state at every revision, so recovery starts there.
    match at_revision::<MarkerRow>(ws, "marker", &sel.id, sel.revision)? {
        Some(row) => {
            // Distinguish live-vs-history for the label.
            let live_rev = ws
                .meta
                .list_markers(&row.investigation_id, true)
                .map_err(ws_err)?
                .into_iter()
                .find(|m| m.marker_id == sel.id)
                .map(|m| m.revision);
            match live_rev {
                Some(lr) if lr == sel.revision => Ok(Capture::Live(row)),
                Some(lr) => Ok(Capture::FromHistory {
                    row,
                    live_revision: lr,
                }),
                None => Ok(Capture::FromHistory {
                    row,
                    live_revision: sel.revision,
                }),
            }
        }
        None => Ok(Capture::Unavailable {
            reason: format!("selected revision {} is not recoverable", sel.revision),
        }),
    }
}

/// Assembles the snapshot for one definition.
pub fn snapshot(ws: &Workspace, def: ReportDefRow) -> Result<ReportSnapshot, JobError> {
    let investigation = ws
        .meta
        .get_investigation(&def.investigation_id)
        .map_err(ws_err)?
        .ok_or_else(|| {
            JobError::new(
                "workspace/missing-entity",
                format!("investigation {} does not exist", def.investigation_id),
            )
        })?;
    let sections = parse_sections(&def.sections_json)?;
    let selected_evidence: Vec<SelectedRef> = serde_json::from_str(&def.selected_evidence_json)
        .map_err(|e| bad_def(format!("selected_evidence_json does not parse: {e}")))?;
    let selected_markers: Vec<SelectedRef> = serde_json::from_str(&def.selected_markers_json)
        .map_err(|e| bad_def(format!("selected_markers_json does not parse: {e}")))?;

    let mut evidence = Vec::with_capacity(selected_evidence.len());
    for sel in selected_evidence {
        let cap = capture_evidence(ws, &sel)?;
        evidence.push((sel, cap));
    }
    let mut markers = Vec::with_capacity(selected_markers.len());
    for sel in selected_markers {
        let cap = capture_marker(ws, &sel)?;
        markers.push((sel, cap));
    }
    let hypotheses = ws
        .meta
        .list_hypotheses(&def.investigation_id)
        .map_err(ws_err)?;

    Ok(ReportSnapshot {
        investigation,
        def,
        sections,
        hypotheses,
        evidence,
        markers,
    })
}

/// Snapshot metadata recorded on the artifact row: every revision and
/// version that shaped the bytes.
pub fn snapshot_meta(s: &ReportSnapshot) -> String {
    let mut evidence: BTreeMap<String, serde_json::Value> = BTreeMap::new();
    for (sel, cap) in &s.evidence {
        let state = match cap {
            Capture::Live(_) => "live",
            Capture::FromHistory { .. } => "from_history",
            Capture::Unavailable { .. } => "unavailable",
        };
        evidence.insert(
            sel.id.clone(),
            serde_json::json!({"revision": sel.revision, "capture": state}),
        );
    }
    let mut markers: BTreeMap<String, serde_json::Value> = BTreeMap::new();
    for (sel, cap) in &s.markers {
        let state = match cap {
            Capture::Live(_) => "live",
            Capture::FromHistory { .. } => "from_history",
            Capture::Unavailable { .. } => "unavailable",
        };
        markers.insert(
            sel.id.clone(),
            serde_json::json!({"revision": sel.revision, "capture": state}),
        );
    }
    serde_json::json!({
        "investigation_id": s.investigation.investigation_id,
        "investigation_revision": s.investigation.revision,
        "report_def_id": s.def.report_def_id,
        "report_def_revision": s.def.revision,
        "evidence": evidence,
        "markers": markers,
        "hypothesis_revisions": s.hypotheses.iter()
            .map(|h| (h.hypothesis_id.clone(), h.revision))
            .collect::<BTreeMap<_, _>>(),
        "envelope_version": logscope_case::EVIDENCE_ENVELOPE_VERSION,
        "app_version": env!("CARGO_PKG_VERSION"),
        "display_timezone": "UTC",
    })
    .to_string()
}

// ---- shared formatting -------------------------------------------------------

fn fmt_ns(ns: i64) -> String {
    chrono::DateTime::from_timestamp_nanos(ns)
        .format("%Y-%m-%d %H:%M:%S%.3fZ")
        .to_string()
}

fn entry_label(e: &TimelineEntry) -> String {
    match (e.at_nanos, e.end_nanos) {
        (Some(a), Some(b)) => format!("{} → {}", fmt_ns(a), fmt_ns(b)),
        (Some(a), None) => fmt_ns(a),
        _ => "undated".into(),
    }
}

/// The timeline section is a projection of the SELECTED markers and
/// evidence only, using the same entry derivation as the timeline view.
fn selected_timeline(s: &ReportSnapshot) -> (Vec<TimelineEntry>, Vec<TimelineEntry>) {
    let mut entries = Vec::new();
    for (_, cap) in &s.markers {
        match cap {
            Capture::Live(m) | Capture::FromHistory { row: m, .. } => entries.push(marker_entry(m)),
            Capture::Unavailable { .. } => {}
        }
    }
    for (_, cap) in &s.evidence {
        match cap {
            Capture::Live(ev) | Capture::FromHistory { row: ev, .. } => {
                entries.push(evidence_entry(ev))
            }
            Capture::Unavailable { .. } => {}
        }
    }
    order_entries(entries)
}

// ---- Markdown renderer -------------------------------------------------------

/// Escapes Markdown-significant characters in inline prose.
fn md_escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '\\' | '`' | '*' | '_' | '{' | '}' | '[' | ']' | '<' | '>' | '#' | '|' => {
                out.push('\\');
                out.push(c);
            }
            _ => out.push(c),
        }
    }
    out
}

/// Fences a block with more backticks than any run inside it, so
/// embedded fences can never escape.
fn md_fence(content: &str) -> String {
    let mut longest = 0usize;
    let mut current = 0usize;
    for c in content.chars() {
        if c == '`' {
            current += 1;
            longest = longest.max(current);
        } else {
            current = 0;
        }
    }
    let fence = "`".repeat((longest + 1).max(3));
    // A trailing newline inside the block keeps the closing fence on its
    // own line even when the content does not end with one.
    let body = if content.ends_with('\n') {
        content.to_string()
    } else {
        format!("{content}\n")
    };
    format!("{fence}\n{body}{fence}")
}

fn md_narrative(out: &mut String, content: &Option<String>) {
    match content.as_deref().map(str::trim).filter(|c| !c.is_empty()) {
        Some(text) => {
            out.push_str(&md_escape(text));
            out.push('\n');
        }
        // The documented rule: a blank narrative renders the explicit
        // word so silence cannot read as completeness.
        None => out.push_str("*unknown*\n"),
    }
}

fn section_title(kind: &str) -> String {
    let mut words: Vec<String> = kind
        .split('_')
        .map(|w| {
            let mut cs = w.chars();
            match cs.next() {
                Some(f) => f.to_uppercase().collect::<String>() + cs.as_str(),
                None => String::new(),
            }
        })
        .collect();
    if words.is_empty() {
        words.push(kind.to_string());
    }
    words.join(" ")
}

fn capture_note_md(sel: &SelectedRef, cap: &Capture<EvidenceRow>) -> Option<String> {
    match cap {
        Capture::Live(_) => None,
        Capture::FromHistory { live_revision, .. } => Some(format!(
            "> **As captured at revision {}** — the live evidence has since moved to revision {}.",
            sel.revision, live_revision
        )),
        Capture::Unavailable { reason } => Some(format!(
            "> **Selected revision {} unavailable:** {}.",
            sel.revision,
            md_escape(reason)
        )),
    }
}

/// Renders the deterministic Markdown document (UTF-8, LF only).
pub fn render_markdown(s: &ReportSnapshot) -> String {
    let mut out = String::new();
    out.push_str(&format!("# {}\n", md_escape(&s.def.title)));
    if let Some(sub) = s
        .def
        .subtitle
        .as_deref()
        .map(str::trim)
        .filter(|x| !x.is_empty())
    {
        out.push_str(&format!("*{}*\n", md_escape(sub)));
    }
    out.push('\n');
    out.push_str(&format!(
        "Investigation: {} (`{}`, revision {}) · status {} · report definition `{}` revision {} · times UTC\n",
        md_escape(&s.investigation.title),
        s.investigation.investigation_id,
        s.investigation.revision,
        md_escape(&s.investigation.status),
        s.def.report_def_id,
        s.def.revision,
    ));

    for section in &s.sections {
        out.push('\n');
        out.push_str(&format!("## {}\n\n", section_title(&section.kind)));
        match section.kind.as_str() {
            k if NARRATIVE_KINDS.contains(&k) => md_narrative(&mut out, &section.content),
            "timeline" => {
                let (dated, undated) = selected_timeline(s);
                if dated.is_empty() && undated.is_empty() {
                    out.push_str("*No timeline entries selected.*\n");
                }
                for e in &dated {
                    out.push_str(&format!(
                        "- `{}` — [{}] {}{}\n",
                        entry_label(e),
                        md_escape(&e.detail_kind),
                        md_escape(&e.title),
                        e.original_time_text
                            .as_deref()
                            .map(|t| format!(" (entered: {})", md_escape(t)))
                            .unwrap_or_default(),
                    ));
                }
                if !undated.is_empty() {
                    out.push_str("\n**Undated** (reason stated per entry):\n");
                    for e in &undated {
                        out.push_str(&format!(
                            "- [{}] {} — {}\n",
                            md_escape(&e.detail_kind),
                            md_escape(&e.title),
                            md_escape(e.undated_reason.as_deref().unwrap_or("undated")),
                        ));
                    }
                }
            }
            "hypotheses" => {
                if s.hypotheses.is_empty() {
                    out.push_str("*No hypotheses recorded.*\n");
                }
                for h in &s.hypotheses {
                    out.push_str(&format!(
                        "- **{}** — {}{}\n",
                        md_escape(&h.state),
                        md_escape(&h.statement),
                        h.rationale
                            .as_deref()
                            .map(|r| format!(" ({})", md_escape(r)))
                            .unwrap_or_default(),
                    ));
                }
            }
            "evidence" => {
                if s.evidence.is_empty() {
                    out.push_str("*No evidence selected.*\n");
                }
                for (sel, cap) in &s.evidence {
                    // The heading carries the stable `ev-` token, so any
                    // renderer-generated anchor contains it.
                    out.push_str(&format!("### ev-{}\n\n", sel.id));
                    if let Some(note) = capture_note_md(sel, cap) {
                        out.push_str(&note);
                        out.push('\n');
                        out.push('\n');
                    }
                    match cap {
                        Capture::Live(ev) | Capture::FromHistory { row: ev, .. } => {
                            out.push_str(&format!(
                                "**{}** — kind {}, integrity state `{}`\n\n",
                                md_escape(&ev.title),
                                md_escape(&ev.kind),
                                ev.resolver_state,
                            ));
                            if let Some(a) = ev.annotation.as_deref() {
                                out.push_str(&format!("{}\n\n", md_escape(a)));
                            }
                            if let Some(r) = ev.relevance.as_deref() {
                                out.push_str(&format!("Why it matters: {}\n\n", md_escape(r)));
                            }
                            out.push_str("Captured snapshot (verbatim, bounded at pin time):\n\n");
                            out.push_str(&md_fence(&pretty_json(&ev.snapshot_json)));
                            out.push('\n');
                        }
                        Capture::Unavailable { .. } => {
                            out.push_str("*Not rendered.*\n");
                        }
                    }
                }
            }
            other => {
                // parse_sections guarantees this cannot happen; render
                // honestly if it ever does.
                out.push_str(&format!("*Unsupported section {}*\n", md_escape(other)));
            }
        }
    }

    // Deterministic single trailing newline, LF endings by construction.
    while out.ends_with('\n') {
        out.pop();
    }
    out.push('\n');
    out
}

fn pretty_json(json: &str) -> String {
    serde_json::from_str::<serde_json::Value>(json)
        .and_then(|v| serde_json::to_string_pretty(&v))
        .unwrap_or_else(|_| json.to_string())
}

// ---- HTML renderer -----------------------------------------------------------

fn h(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            _ => out.push(c),
        }
    }
    out
}

const REPORT_CSS: &str = "\
body{font-family:system-ui,-apple-system,'Segoe UI',sans-serif;max-width:60rem;\
margin:2rem auto;padding:0 1rem;color:#1a222e;line-height:1.5}\
h1{border-bottom:2px solid #d0d7e2;padding-bottom:.3rem}\
h2{border-bottom:1px solid #d0d7e2;padding-bottom:.2rem;margin-top:2rem}\
code,pre{font-family:ui-monospace,Consolas,monospace;background:#f2f4f8;\
border-radius:3px}\
code{padding:0 .25em}\
pre{padding:.6rem;overflow-x:auto;border:1px solid #d0d7e2;white-space:pre-wrap;\
word-break:break-word}\
.meta{color:#5b6472;font-size:.9rem}\
.warn{background:#fff6e0;border-left:4px solid #d9a521;padding:.4rem .6rem;\
margin:.4rem 0}\
.state{font-size:.85rem;padding:.05rem .45rem;border-radius:8px;\
background:#e7ebf2}\
ul{padding-left:1.4rem}";

fn html_narrative(out: &mut String, content: &Option<String>) {
    match content.as_deref().map(str::trim).filter(|c| !c.is_empty()) {
        Some(text) => out.push_str(&format!("<p>{}</p>\n", h(text))),
        None => out.push_str("<p><em>unknown</em></p>\n"),
    }
}

/// Renders the self-contained HTML document: embedded CSS only, zero
/// scripts/links/frames/remote anything, restrictive CSP meta, every
/// value context-escaped. Paths and URLs render as text, never links.
pub fn render_html(s: &ReportSnapshot) -> String {
    let mut b = String::new();
    b.push_str("<!DOCTYPE html>\n<html lang=\"en\">\n<head>\n<meta charset=\"utf-8\">\n");
    b.push_str(
        "<meta http-equiv=\"Content-Security-Policy\" \
         content=\"default-src 'none'; style-src 'unsafe-inline'\">\n",
    );
    b.push_str(&format!("<title>{}</title>\n", h(&s.def.title)));
    b.push_str(&format!("<style>{REPORT_CSS}</style>\n</head>\n<body>\n"));
    b.push_str(&format!("<h1>{}</h1>\n", h(&s.def.title)));
    if let Some(sub) = s
        .def
        .subtitle
        .as_deref()
        .map(str::trim)
        .filter(|x| !x.is_empty())
    {
        b.push_str(&format!("<p class=\"meta\"><em>{}</em></p>\n", h(sub)));
    }
    b.push_str(&format!(
        "<p class=\"meta\">Investigation: {} (<code>{}</code>, revision {}) · status {} · \
         report definition <code>{}</code> revision {} · times UTC</p>\n",
        h(&s.investigation.title),
        h(&s.investigation.investigation_id),
        s.investigation.revision,
        h(&s.investigation.status),
        h(&s.def.report_def_id),
        s.def.revision,
    ));

    for section in &s.sections {
        b.push_str(&format!("<h2>{}</h2>\n", h(&section_title(&section.kind))));
        match section.kind.as_str() {
            k if NARRATIVE_KINDS.contains(&k) => html_narrative(&mut b, &section.content),
            "timeline" => {
                let (dated, undated) = selected_timeline(s);
                if dated.is_empty() && undated.is_empty() {
                    b.push_str("<p><em>No timeline entries selected.</em></p>\n");
                }
                if !dated.is_empty() {
                    b.push_str("<ul>\n");
                    for e in &dated {
                        b.push_str(&format!(
                            "<li><code>{}</code> — <span class=\"state\">{}</span> {}{}</li>\n",
                            h(&entry_label(e)),
                            h(&e.detail_kind),
                            h(&e.title),
                            e.original_time_text
                                .as_deref()
                                .map(|t| format!(
                                    " <span class=\"meta\">(entered: {})</span>",
                                    h(t)
                                ))
                                .unwrap_or_default(),
                        ));
                    }
                    b.push_str("</ul>\n");
                }
                if !undated.is_empty() {
                    b.push_str(
                        "<p><strong>Undated</strong> (reason stated per entry):</p>\n<ul>\n",
                    );
                    for e in &undated {
                        b.push_str(&format!(
                            "<li><span class=\"state\">{}</span> {} — <span class=\"meta\">{}</span></li>\n",
                            h(&e.detail_kind),
                            h(&e.title),
                            h(e.undated_reason.as_deref().unwrap_or("undated")),
                        ));
                    }
                    b.push_str("</ul>\n");
                }
            }
            "hypotheses" => {
                if s.hypotheses.is_empty() {
                    b.push_str("<p><em>No hypotheses recorded.</em></p>\n");
                } else {
                    b.push_str("<ul>\n");
                    for hy in &s.hypotheses {
                        b.push_str(&format!(
                            "<li><span class=\"state\">{}</span> <strong>{}</strong>{}</li>\n",
                            h(&hy.state),
                            h(&hy.statement),
                            hy.rationale
                                .as_deref()
                                .map(|r| format!(" <span class=\"meta\">({})</span>", h(r)))
                                .unwrap_or_default(),
                        ));
                    }
                    b.push_str("</ul>\n");
                }
            }
            "evidence" => {
                if s.evidence.is_empty() {
                    b.push_str("<p><em>No evidence selected.</em></p>\n");
                }
                for (sel, cap) in &s.evidence {
                    b.push_str(&format!("<section id=\"ev-{}\">\n", h(&sel.id)));
                    b.push_str(&format!("<h3><code>ev-{}</code></h3>\n", h(&sel.id)));
                    match cap {
                        Capture::FromHistory { live_revision, .. } => {
                            b.push_str(&format!(
                                "<div class=\"warn\">As captured at revision {} — the live \
                                 evidence has since moved to revision {}.</div>\n",
                                sel.revision, live_revision
                            ));
                        }
                        Capture::Unavailable { reason } => {
                            b.push_str(&format!(
                                "<div class=\"warn\">Selected revision {} unavailable: {}.</div>\n",
                                sel.revision,
                                h(reason)
                            ));
                        }
                        Capture::Live(_) => {}
                    }
                    match cap {
                        Capture::Live(ev) | Capture::FromHistory { row: ev, .. } => {
                            b.push_str(&format!(
                                "<p><strong>{}</strong> — kind {}, integrity state \
                                 <span class=\"state\">{}</span></p>\n",
                                h(&ev.title),
                                h(&ev.kind),
                                h(&ev.resolver_state),
                            ));
                            if let Some(a) = ev.annotation.as_deref() {
                                b.push_str(&format!("<p>{}</p>\n", h(a)));
                            }
                            if let Some(r) = ev.relevance.as_deref() {
                                b.push_str(&format!("<p>Why it matters: {}</p>\n", h(r)));
                            }
                            b.push_str(
                                "<p class=\"meta\">Captured snapshot (verbatim, bounded at pin \
                                 time):</p>\n",
                            );
                            b.push_str(&format!(
                                "<pre>{}</pre>\n",
                                h(&pretty_json(&ev.snapshot_json))
                            ));
                        }
                        Capture::Unavailable { .. } => {
                            b.push_str("<p><em>Not rendered.</em></p>\n");
                        }
                    }
                    b.push_str("</section>\n");
                }
            }
            other => {
                b.push_str(&format!(
                    "<p><em>Unsupported section {}</em></p>\n",
                    h(other)
                ));
            }
        }
    }
    b.push_str("</body>\n</html>\n");
    b
}

// ---- generation --------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReportFormat {
    Markdown,
    Html,
}

impl ReportFormat {
    pub fn as_str(self) -> &'static str {
        match self {
            ReportFormat::Markdown => "markdown",
            ReportFormat::Html => "html",
        }
    }
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "markdown" => Some(ReportFormat::Markdown),
            "html" => Some(ReportFormat::Html),
            _ => None,
        }
    }
}

fn hex(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        use std::fmt::Write;
        let _ = write!(s, "{b:02x}");
    }
    s
}

/// Generates one report artifact: staged write in the destination
/// directory, atomic rename, SHA-256 recorded on the immutable artifact
/// row. The destination must not exist (no silent overwrite).
pub fn generate_report(
    ws: &Workspace,
    report_def_id: &str,
    format: ReportFormat,
    destination: &Path,
) -> Result<ReportArtifactRow, JobError> {
    let def = ws
        .meta
        .get_report_def(report_def_id)
        .map_err(ws_err)?
        .ok_or_else(|| {
            JobError::new(
                "workspace/missing-entity",
                format!("report definition {report_def_id} does not exist"),
            )
        })?;
    let snap = snapshot(ws, def)?;
    let meta = snapshot_meta(&snap);

    if destination.exists() {
        return Err(JobError::new(
            "report/destination-exists",
            format!(
                "destination already exists: {} (choose a new file name)",
                destination.display()
            ),
        ));
    }
    let dir = destination
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .ok_or_else(|| {
            JobError::new(
                "report/invalid-destination",
                "destination has no parent directory",
            )
        })?;
    std::fs::create_dir_all(dir)
        .map_err(|e| JobError::new("report/io", format!("{}: {e}", dir.display())))?;

    let artifact_id = format!("art-{}", uuid::Uuid::new_v4());
    ws.meta
        .start_report_artifact(
            &artifact_id,
            report_def_id,
            &snap.investigation.investigation_id,
            format.as_str(),
            &destination.display().to_string(),
            &meta,
        )
        .map_err(ws_err)?;

    let result = (|| -> Result<(String, i64), JobError> {
        let bytes = match format {
            ReportFormat::Markdown => render_markdown(&snap),
            ReportFormat::Html => render_html(&snap),
        };
        let temp = dir.join(format!(
            ".{}.partial-{}",
            destination
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_else(|| "report".into()),
            uuid::Uuid::new_v4()
        ));
        let write = (|| -> std::io::Result<()> {
            let mut f = std::fs::File::create(&temp)?;
            f.write_all(bytes.as_bytes())?;
            f.sync_all()
        })();
        if let Err(e) = write {
            let _ = std::fs::remove_file(&temp);
            return Err(JobError::new("report/io", e.to_string()));
        }
        if let Err(e) = std::fs::rename(&temp, destination) {
            let _ = std::fs::remove_file(&temp);
            return Err(JobError::new(
                "report/publish",
                format!("could not move report into place: {e}"),
            ));
        }
        let digest = hex(&Sha256::digest(bytes.as_bytes()));
        Ok((digest, bytes.len() as i64))
    })();

    match result {
        Ok((digest, size)) => ws
            .meta
            .finish_report_artifact(&artifact_id, "completed", Some(&digest), Some(size), None)
            .map_err(ws_err),
        Err(e) => {
            let _ = ws.meta.finish_report_artifact(
                &artifact_id,
                "failed",
                None,
                None,
                serde_json::to_string(&e).ok().as_deref(),
            );
            Err(e)
        }
    }
}
