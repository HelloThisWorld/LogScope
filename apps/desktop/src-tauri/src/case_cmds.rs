//! v0.3 investigation/evidence commands: a thin typed layer over
//! `logscope_app::case` and the `case_meta` repositories. No case
//! semantics live here — pin capture, envelope handling, and
//! verification all run through the shared application services, and
//! the UI never parses an envelope itself.

use std::sync::Arc;

use std::path::PathBuf;

use logscope_app::case;
use logscope_app::dto::*;
use logscope_app::report::{self, ReportFormat};
use logscope_app::timeline;
use logscope_case::envelope::{self, DecodeOutcome, EvidenceReference};
use logscope_jobs::JobEvent;
use logscope_workspace::{
    EvidenceGroupRow, EvidenceRow, HistoryRow, HypothesisRow, InvestigationEdit, InvestigationRow,
    ItemRow, MarkerEdit, MarkerRow, NewHypothesis, NewInvestigation, NewItem, NewMarker,
    NewReportDef, ReportArtifactRow, ReportDefEdit, ReportDefRow, Workspace,
};
use tauri::{AppHandle, Emitter, Manager, State};

use crate::explorer_cmds::{err, strategy_from_dto, strategy_from_json, with_engine, ws_handle};
use crate::{AppState, CmdResult};

fn ws_err(e: logscope_workspace::WorkspaceError) -> ErrorDto {
    ErrorDto::new(e.code(), e)
}

fn jerr(e: &logscope_jobs::JobError) -> ErrorDto {
    ErrorDto::new(&e.code, &e.message)
}

fn tags_from_json(json: &str) -> Vec<String> {
    serde_json::from_str(json).unwrap_or_default()
}

fn inv_dto(row: InvestigationRow) -> InvestigationDto {
    InvestigationDto {
        investigation_id: row.investigation_id,
        title: row.title,
        description: row.description,
        status: row.status,
        severity: row.severity,
        owner_text: row.owner_text,
        tags: tags_from_json(&row.tags_json),
        created_at: row.created_at,
        updated_at: row.updated_at,
        status_changed_at: row.status_changed_at,
        incident_started_at: row.incident_started_at,
        mitigated_at: row.mitigated_at,
        resolved_at: row.resolved_at,
        window_start: row.window_start,
        window_end: row.window_end,
        revision: row.revision,
    }
}

fn hyp_dto(row: HypothesisRow, linked_evidence_ids: Vec<String>) -> HypothesisDto {
    HypothesisDto {
        hypothesis_id: row.hypothesis_id,
        investigation_id: row.investigation_id,
        statement: row.statement,
        rationale: row.rationale,
        state: row.state,
        position: row.position,
        created_at: row.created_at,
        updated_at: row.updated_at,
        revision: row.revision,
        linked_evidence_ids,
    }
}

/// Maps a hypothesis and loads its evidence links.
fn hyp_dto_loaded(ws: &Workspace, row: HypothesisRow) -> CmdResult<HypothesisDto> {
    let links = ws
        .meta
        .linked_evidence_ids(&row.hypothesis_id)
        .map_err(ws_err)?;
    Ok(hyp_dto(row, links))
}

fn item_dto(row: ItemRow) -> ItemDto {
    ItemDto {
        item_id: row.item_id,
        investigation_id: row.investigation_id,
        kind: row.kind,
        content: row.content,
        task_status: row.task_status,
        question_status: row.question_status,
        authored_by_user: row.authored_by_user,
        position: row.position,
        archived: row.archived,
        created_at: row.created_at,
        updated_at: row.updated_at,
        revision: row.revision,
    }
}

fn ev_dto(row: EvidenceRow) -> EvidenceDto {
    EvidenceDto {
        evidence_id: row.evidence_id,
        investigation_id: row.investigation_id,
        kind: row.kind,
        signal: row.signal,
        title: row.title,
        annotation: row.annotation,
        relevance: row.relevance,
        group_id: row.group_id,
        position: row.position,
        supersedes_evidence_id: row.supersedes_evidence_id,
        archived: row.archived,
        resolver_state: row.resolver_state,
        resolver_detail_json: row.resolver_detail_json,
        last_verified_at: row.last_verified_at,
        envelope_version: row.envelope_version,
        snapshot_json: row.snapshot_json,
        created_at: row.created_at,
        updated_at: row.updated_at,
        revision: row.revision,
    }
}

fn group_dto(row: EvidenceGroupRow) -> EvidenceGroupDto {
    EvidenceGroupDto {
        group_id: row.group_id,
        investigation_id: row.investigation_id,
        name: row.name,
        position: row.position,
        revision: row.revision,
    }
}

fn hist_dto(row: HistoryRow) -> HistoryDto {
    HistoryDto {
        history_id: row.history_id,
        entity_kind: row.entity_kind,
        entity_id: row.entity_id,
        revision: row.revision,
        action: row.action,
        detail_json: row.detail_json,
        created_at: row.created_at,
    }
}

fn marker_dto(row: MarkerRow) -> MarkerDto {
    MarkerDto {
        marker_id: row.marker_id,
        investigation_id: row.investigation_id,
        kind: row.kind,
        label: row.label,
        description: row.description,
        at_nanos: row.at_nanos,
        end_nanos: row.end_nanos,
        original_tz_offset_min: row.original_tz_offset_min,
        original_time_text: row.original_time_text,
        position: row.position,
        archived: row.archived,
        created_at: row.created_at,
        updated_at: row.updated_at,
        revision: row.revision,
    }
}

/// Parses the optional marker time texts. The instant is normalized to
/// UTC; the text and offset are preserved exactly as entered.
type MarkerTimes = (Option<i64>, Option<i64>, Option<i64>, Option<String>);

fn marker_times(
    time_text: &Option<String>,
    end_time_text: &Option<String>,
) -> CmdResult<MarkerTimes> {
    let jmap = |e: &logscope_jobs::JobError| ErrorDto::new(&e.code, &e.message);
    let (at, offset) = match time_text
        .as_deref()
        .map(str::trim)
        .filter(|t| !t.is_empty())
    {
        Some(t) => {
            let (n, off) = timeline::parse_marker_time(t).map_err(|e| jmap(&e))?;
            (Some(n), Some(off))
        }
        None => (None, None),
    };
    let end = match end_time_text
        .as_deref()
        .map(str::trim)
        .filter(|t| !t.is_empty())
    {
        Some(t) => Some(timeline::parse_marker_time(t).map_err(|e| jmap(&e))?.0),
        None => None,
    };
    let original = time_text.clone().filter(|t| !t.trim().is_empty());
    Ok((at, end, offset, original))
}

fn timeline_entry_dto(e: timeline::TimelineEntry) -> TimelineEntryDto {
    TimelineEntryDto {
        entry_kind: e.entry_kind,
        id: e.id,
        detail_kind: e.detail_kind,
        title: e.title,
        at_nanos: e.at_nanos,
        end_nanos: e.end_nanos,
        time_source: e.time_source,
        undated_reason: e.undated_reason,
        description: e.description,
        original_time_text: e.original_time_text,
        original_tz_offset_min: e.original_tz_offset_min,
    }
}

fn pin_common(dto: &PinCommonDto) -> case::PinCommon {
    case::PinCommon {
        investigation_id: dto.investigation_id.clone(),
        title: dto.title.clone(),
        annotation: dto.annotation.clone(),
        relevance: dto.relevance.clone(),
        group_id: dto.group_id.clone(),
    }
}

fn query_scope(dto: &QueryScopeDto) -> CmdResult<case::QueryScope> {
    Ok(case::QueryScope {
        query_text: dto.query_text.clone(),
        dataset_ids: dto.dataset_ids.clone(),
        time_strategy: strategy_from_dto(&dto.time_strategy)?,
    })
}

// ---- investigations -----------------------------------------------------

#[tauri::command]
pub fn list_investigations(
    state: State<'_, AppState>,
    include_archived: bool,
) -> CmdResult<Vec<InvestigationDto>> {
    let ws = ws_handle(&state)?;
    let rows = ws
        .meta
        .list_investigations(include_archived)
        .map_err(ws_err)?;
    Ok(rows.into_iter().map(inv_dto).collect())
}

#[tauri::command]
pub fn create_investigation(
    state: State<'_, AppState>,
    request: NewInvestigationDto,
) -> CmdResult<InvestigationDto> {
    let ws = ws_handle(&state)?;
    let new = NewInvestigation {
        investigation_id: format!("inv-{}", uuid::Uuid::new_v4()),
        title: request.title,
        description: request.description,
        severity: request.severity,
        owner_text: request.owner_text,
        tags_json: serde_json::to_string(&request.tags).unwrap_or_else(|_| "[]".into()),
        incident_started_at: request.incident_started_at,
        window_start: request.window_start,
        window_end: request.window_end,
    };
    ws.meta
        .create_investigation(&new)
        .map(inv_dto)
        .map_err(ws_err)
}

#[tauri::command]
pub fn update_investigation(
    state: State<'_, AppState>,
    request: InvestigationEditDto,
) -> CmdResult<InvestigationDto> {
    let ws = ws_handle(&state)?;
    let edit = InvestigationEdit {
        investigation_id: request.investigation_id,
        expected_revision: request.expected_revision,
        title: request.title,
        description: request.description,
        severity: request.severity,
        owner_text: request.owner_text,
        tags_json: serde_json::to_string(&request.tags).unwrap_or_else(|_| "[]".into()),
        incident_started_at: request.incident_started_at,
        mitigated_at: request.mitigated_at,
        resolved_at: request.resolved_at,
        window_start: request.window_start,
        window_end: request.window_end,
    };
    ws.meta
        .update_investigation(&edit)
        .map(inv_dto)
        .map_err(ws_err)
}

#[tauri::command]
pub fn set_investigation_status(
    state: State<'_, AppState>,
    investigation_id: String,
    expected_revision: i64,
    status: String,
) -> CmdResult<InvestigationDto> {
    let ws = ws_handle(&state)?;
    ws.meta
        .set_investigation_status(&investigation_id, expected_revision, &status, "set_status")
        .map(inv_dto)
        .map_err(ws_err)
}

#[tauri::command]
pub fn investigation_bundle(
    state: State<'_, AppState>,
    investigation_id: String,
) -> CmdResult<InvestigationBundleDto> {
    let ws = ws_handle(&state)?;
    let investigation = ws
        .meta
        .get_investigation(&investigation_id)
        .map_err(ws_err)?
        .ok_or_else(|| {
            err(
                "workspace/missing-entity",
                format!("investigation {investigation_id} does not exist"),
            )
        })?;
    let mut hypotheses = Vec::new();
    for row in ws.meta.list_hypotheses(&investigation_id).map_err(ws_err)? {
        hypotheses.push(hyp_dto_loaded(&ws, row)?);
    }
    let items = ws
        .meta
        .list_items(&investigation_id, true)
        .map_err(ws_err)?
        .into_iter()
        .map(item_dto)
        .collect();
    let evidence = ws
        .meta
        .list_evidence(&investigation_id, true)
        .map_err(ws_err)?
        .into_iter()
        .map(ev_dto)
        .collect();
    let groups = ws
        .meta
        .list_evidence_groups(&investigation_id)
        .map_err(ws_err)?
        .into_iter()
        .map(group_dto)
        .collect();
    let markers = ws
        .meta
        .list_markers(&investigation_id, true)
        .map_err(ws_err)?
        .into_iter()
        .map(marker_dto)
        .collect();
    Ok(InvestigationBundleDto {
        investigation: inv_dto(investigation),
        hypotheses,
        items,
        evidence,
        groups,
        markers,
    })
}

// ---- timeline markers ---------------------------------------------------

#[tauri::command]
pub fn create_marker(state: State<'_, AppState>, request: NewMarkerDto) -> CmdResult<MarkerDto> {
    let ws = ws_handle(&state)?;
    if logscope_case::MarkerKind::parse(&request.kind).is_none() {
        return Err(err(
            "case/invalid",
            format!(
                "unknown marker kind {:?} (expected {})",
                request.kind,
                logscope_case::MarkerKind::EXPECTED
            ),
        ));
    }
    let (at_nanos, end_nanos, original_tz_offset_min, original_time_text) =
        marker_times(&request.time_text, &request.end_time_text)?;
    let new = NewMarker {
        marker_id: format!("mark-{}", uuid::Uuid::new_v4()),
        investigation_id: request.investigation_id,
        kind: request.kind,
        label: request.label,
        description: request.description,
        at_nanos,
        end_nanos,
        original_tz_offset_min,
        original_time_text,
    };
    ws.meta.create_marker(&new).map(marker_dto).map_err(ws_err)
}

#[tauri::command]
pub fn update_marker(state: State<'_, AppState>, request: MarkerEditDto) -> CmdResult<MarkerDto> {
    let ws = ws_handle(&state)?;
    if logscope_case::MarkerKind::parse(&request.kind).is_none() {
        return Err(err(
            "case/invalid",
            format!(
                "unknown marker kind {:?} (expected {})",
                request.kind,
                logscope_case::MarkerKind::EXPECTED
            ),
        ));
    }
    let (at_nanos, end_nanos, original_tz_offset_min, original_time_text) =
        marker_times(&request.time_text, &request.end_time_text)?;
    let edit = MarkerEdit {
        marker_id: request.marker_id,
        expected_revision: request.expected_revision,
        kind: request.kind,
        label: request.label,
        description: request.description,
        at_nanos,
        end_nanos,
        original_tz_offset_min,
        original_time_text,
    };
    ws.meta.update_marker(&edit).map(marker_dto).map_err(ws_err)
}

#[tauri::command]
pub fn set_marker_archived(
    state: State<'_, AppState>,
    marker_id: String,
    expected_revision: i64,
    archived: bool,
) -> CmdResult<MarkerDto> {
    let ws = ws_handle(&state)?;
    ws.meta
        .set_marker_archived(&marker_id, expected_revision, archived)
        .map(marker_dto)
        .map_err(ws_err)
}

#[tauri::command]
pub fn investigation_timeline(
    state: State<'_, AppState>,
    investigation_id: String,
) -> CmdResult<TimelineDto> {
    let ws = ws_handle(&state)?;
    let model = timeline::timeline(&ws, &investigation_id).map_err(|e| jerr(&e))?;
    Ok(TimelineDto {
        dated: model.dated.into_iter().map(timeline_entry_dto).collect(),
        undated: model.undated.into_iter().map(timeline_entry_dto).collect(),
        archived_excluded: model.archived_excluded,
    })
}

#[tauri::command]
pub fn investigation_activity(
    state: State<'_, AppState>,
    investigation_id: String,
    limit: Option<u32>,
) -> CmdResult<Vec<HistoryDto>> {
    let ws = ws_handle(&state)?;
    let rows = ws
        .meta
        .list_investigation_activity(&investigation_id, limit.unwrap_or(200))
        .map_err(ws_err)?;
    Ok(rows.into_iter().map(hist_dto).collect())
}

// ---- hypotheses ---------------------------------------------------------

#[tauri::command]
pub fn create_hypothesis(
    state: State<'_, AppState>,
    investigation_id: String,
    statement: String,
    rationale: Option<String>,
) -> CmdResult<HypothesisDto> {
    let ws = ws_handle(&state)?;
    let new = NewHypothesis {
        hypothesis_id: format!("hyp-{}", uuid::Uuid::new_v4()),
        investigation_id,
        statement,
        rationale,
    };
    let row = ws.meta.create_hypothesis(&new).map_err(ws_err)?;
    Ok(hyp_dto(row, Vec::new()))
}

#[tauri::command]
pub fn update_hypothesis(
    state: State<'_, AppState>,
    hypothesis_id: String,
    expected_revision: i64,
    statement: String,
    rationale: Option<String>,
) -> CmdResult<HypothesisDto> {
    let ws = ws_handle(&state)?;
    let row = ws
        .meta
        .update_hypothesis(
            &hypothesis_id,
            expected_revision,
            &statement,
            rationale.as_deref(),
        )
        .map_err(ws_err)?;
    hyp_dto_loaded(&ws, row)
}

#[tauri::command]
pub fn set_hypothesis_state(
    state: State<'_, AppState>,
    hypothesis_id: String,
    expected_revision: i64,
    new_state: String,
) -> CmdResult<HypothesisDto> {
    let ws = ws_handle(&state)?;
    let row = ws
        .meta
        .set_hypothesis_state(&hypothesis_id, expected_revision, &new_state)
        .map_err(ws_err)?;
    hyp_dto_loaded(&ws, row)
}

#[tauri::command]
pub fn link_hypothesis_evidence(
    state: State<'_, AppState>,
    hypothesis_id: String,
    expected_revision: i64,
    evidence_id: String,
) -> CmdResult<HypothesisDto> {
    let ws = ws_handle(&state)?;
    let row = ws
        .meta
        .link_hypothesis_evidence(&hypothesis_id, expected_revision, &evidence_id)
        .map_err(ws_err)?;
    hyp_dto_loaded(&ws, row)
}

#[tauri::command]
pub fn unlink_hypothesis_evidence(
    state: State<'_, AppState>,
    hypothesis_id: String,
    expected_revision: i64,
    evidence_id: String,
) -> CmdResult<HypothesisDto> {
    let ws = ws_handle(&state)?;
    let row = ws
        .meta
        .unlink_hypothesis_evidence(&hypothesis_id, expected_revision, &evidence_id)
        .map_err(ws_err)?;
    hyp_dto_loaded(&ws, row)
}

// ---- items --------------------------------------------------------------

#[tauri::command]
pub fn create_item(state: State<'_, AppState>, request: NewItemDto) -> CmdResult<ItemDto> {
    let ws = ws_handle(&state)?;
    let new = NewItem {
        item_id: format!("itm-{}", uuid::Uuid::new_v4()),
        investigation_id: request.investigation_id,
        kind: request.kind,
        content: request.content,
        task_status: request.task_status,
        question_status: request.question_status,
    };
    ws.meta.create_item(&new).map(item_dto).map_err(ws_err)
}

#[tauri::command]
pub fn update_item_content(
    state: State<'_, AppState>,
    item_id: String,
    expected_revision: i64,
    content: String,
) -> CmdResult<ItemDto> {
    let ws = ws_handle(&state)?;
    ws.meta
        .update_item_content(&item_id, expected_revision, &content)
        .map(item_dto)
        .map_err(ws_err)
}

#[tauri::command]
pub fn set_item_status(
    state: State<'_, AppState>,
    item_id: String,
    expected_revision: i64,
    task_status: Option<String>,
    question_status: Option<String>,
) -> CmdResult<ItemDto> {
    let ws = ws_handle(&state)?;
    ws.meta
        .set_item_status(
            &item_id,
            expected_revision,
            task_status.as_deref(),
            question_status.as_deref(),
        )
        .map(item_dto)
        .map_err(ws_err)
}

#[tauri::command]
pub fn set_item_archived(
    state: State<'_, AppState>,
    item_id: String,
    expected_revision: i64,
    archived: bool,
) -> CmdResult<ItemDto> {
    let ws = ws_handle(&state)?;
    ws.meta
        .set_item_archived(&item_id, expected_revision, archived)
        .map(item_dto)
        .map_err(ws_err)
}

#[tauri::command]
pub fn reorder_case_children(
    state: State<'_, AppState>,
    investigation_id: String,
    expected_investigation_revision: i64,
    entity_kind: String,
    ordered_ids: Vec<String>,
) -> CmdResult<InvestigationDto> {
    let ws = ws_handle(&state)?;
    ws.meta
        .reorder_children(
            &investigation_id,
            expected_investigation_revision,
            &entity_kind,
            &ordered_ids,
        )
        .map(inv_dto)
        .map_err(ws_err)
}

// ---- evidence groups + evidence metadata --------------------------------

#[tauri::command]
pub fn create_evidence_group(
    state: State<'_, AppState>,
    investigation_id: String,
    name: String,
) -> CmdResult<EvidenceGroupDto> {
    let ws = ws_handle(&state)?;
    let group_id = format!("grp-{}", uuid::Uuid::new_v4());
    ws.meta
        .create_evidence_group(&group_id, &investigation_id, &name)
        .map(group_dto)
        .map_err(ws_err)
}

#[tauri::command]
pub fn rename_evidence_group(
    state: State<'_, AppState>,
    group_id: String,
    expected_revision: i64,
    name: String,
) -> CmdResult<EvidenceGroupDto> {
    let ws = ws_handle(&state)?;
    ws.meta
        .rename_evidence_group(&group_id, expected_revision, &name)
        .map(group_dto)
        .map_err(ws_err)
}

#[tauri::command]
pub fn delete_evidence_group(state: State<'_, AppState>, group_id: String) -> CmdResult<()> {
    let ws = ws_handle(&state)?;
    ws.meta.delete_evidence_group(&group_id).map_err(ws_err)
}

#[tauri::command]
pub fn update_evidence_annotation(
    state: State<'_, AppState>,
    evidence_id: String,
    expected_revision: i64,
    title: String,
    annotation: Option<String>,
    relevance: Option<String>,
) -> CmdResult<EvidenceDto> {
    let ws = ws_handle(&state)?;
    ws.meta
        .update_evidence_annotation(
            &evidence_id,
            expected_revision,
            &title,
            annotation.as_deref(),
            relevance.as_deref(),
        )
        .map(ev_dto)
        .map_err(ws_err)
}

#[tauri::command]
pub fn set_evidence_group(
    state: State<'_, AppState>,
    evidence_id: String,
    expected_revision: i64,
    group_id: Option<String>,
) -> CmdResult<EvidenceDto> {
    let ws = ws_handle(&state)?;
    ws.meta
        .set_evidence_group(&evidence_id, expected_revision, group_id.as_deref())
        .map(ev_dto)
        .map_err(ws_err)
}

#[tauri::command]
pub fn set_evidence_archived(
    state: State<'_, AppState>,
    evidence_id: String,
    expected_revision: i64,
    archived: bool,
) -> CmdResult<EvidenceDto> {
    let ws = ws_handle(&state)?;
    ws.meta
        .set_evidence_archived(&evidence_id, expected_revision, archived)
        .map(ev_dto)
        .map_err(ws_err)
}

#[tauri::command]
pub fn evidence_history(
    state: State<'_, AppState>,
    evidence_id: String,
) -> CmdResult<Vec<HistoryDto>> {
    let ws = ws_handle(&state)?;
    let rows = ws
        .meta
        .list_entity_history("evidence", &evidence_id)
        .map_err(ws_err)?;
    Ok(rows.into_iter().map(hist_dto).collect())
}

// ---- pins ---------------------------------------------------------------

#[tauri::command]
pub fn pin_event(state: State<'_, AppState>, request: PinEventDto) -> CmdResult<EvidenceDto> {
    let ws = ws_handle(&state)?;
    let req = case::PinEventRequest {
        common: pin_common(&request.common),
        dataset_id: request.dataset_id,
        record_id: request.record_id,
        display_fields: request.display_fields,
        include_raw_excerpt: request.include_raw_excerpt,
    };
    with_engine(&state, None, |engine, _| {
        case::pin_event(&ws, engine, &req)
            .map(ev_dto)
            .map_err(|e| jerr(&e))
    })
}

#[tauri::command]
pub fn pin_selection(
    state: State<'_, AppState>,
    request: PinSelectionDto,
) -> CmdResult<EvidenceDto> {
    let ws = ws_handle(&state)?;
    let req = case::PinSelectionRequest {
        common: pin_common(&request.common),
        record_ids: request.record_ids,
        scope: query_scope(&request.scope)?,
    };
    with_engine(&state, None, |engine, _| {
        case::pin_selection(&ws, engine, &req)
            .map(ev_dto)
            .map_err(|e| jerr(&e))
    })
}

#[tauri::command]
pub fn pin_query(state: State<'_, AppState>, request: PinQueryDto) -> CmdResult<EvidenceDto> {
    let ws = ws_handle(&state)?;
    let req = case::PinQueryRequest {
        common: pin_common(&request.common),
        scope: query_scope(&request.scope)?,
        saved_search_id: request.saved_search_id,
    };
    with_engine(&state, None, |engine, _| {
        case::pin_query(&ws, engine, &req)
            .map(ev_dto)
            .map_err(|e| jerr(&e))
    })
}

#[tauri::command]
pub fn pin_group(state: State<'_, AppState>, request: PinGroupDto) -> CmdResult<EvidenceDto> {
    let ws = ws_handle(&state)?;
    let req = case::PinGroupRequest {
        common: pin_common(&request.common),
        scope: query_scope(&request.scope)?,
        field: request.field,
        value_json: request.value_json,
    };
    with_engine(&state, None, |engine, _| {
        case::pin_group(&ws, engine, &req)
            .map(ev_dto)
            .map_err(|e| jerr(&e))
    })
}

#[tauri::command]
pub fn pin_interval(state: State<'_, AppState>, request: PinIntervalDto) -> CmdResult<EvidenceDto> {
    let ws = ws_handle(&state)?;
    let req = case::PinIntervalRequest {
        common: pin_common(&request.common),
        scope: query_scope(&request.scope)?,
        start: request.start,
        end: request.end,
        bucket_width_nanos: request.bucket_width_nanos,
        display_timezone: request.display_timezone,
        neighbor_buckets: request.neighbor_buckets,
    };
    with_engine(&state, None, |engine, _| {
        case::pin_interval(&ws, engine, &req)
            .map(ev_dto)
            .map_err(|e| jerr(&e))
    })
}

#[tauri::command]
pub fn pin_item(state: State<'_, AppState>, request: PinItemDto) -> CmdResult<EvidenceDto> {
    let ws = ws_handle(&state)?;
    let req = case::PinItemRequest {
        common: pin_common(&request.common),
        item_id: request.item_id,
    };
    case::pin_item(&ws, &req).map(ev_dto).map_err(|e| jerr(&e))
}

// ---- verification -------------------------------------------------------

/// Starts a batched verification job. Progress arrives on the shared
/// `job-event` channel, cancellation goes through the existing
/// `cancel_job` command, and the terminal report is emitted as
/// `verify-finished`.
#[tauri::command]
pub fn start_verify_evidence(
    app: AppHandle,
    state: State<'_, AppState>,
    investigation_id: String,
    only: Option<Vec<String>>,
) -> CmdResult<VerifyStartedDto> {
    let ws = ws_handle(&state)?;
    let total = match &only {
        Some(ids) => ids.len() as i64,
        None => ws
            .meta
            .list_evidence(&investigation_id, false)
            .map_err(ws_err)?
            .len() as i64,
    };

    let job_id = format!("job-{}", uuid::Uuid::new_v4());
    let (tx, rx) = crossbeam_channel::unbounded::<JobEvent>();
    let event_app = app.clone();
    std::thread::spawn(move || {
        for event in rx.iter() {
            let _ = event_app.emit("job-event", &event);
        }
    });

    // The verification job owns its own engine connection; the workspace
    // handle is shared read-only (resolver writes go through `ws.meta`).
    let engine =
        logscope_query::EngineConnection::open_in_memory().map_err(|e| err(e.code(), e))?;
    let ws_job: Arc<Workspace> = ws.clone();
    let inv_job = investigation_id.clone();
    let only_job = only.clone();
    let handle = logscope_jobs::spawn_job(job_id.clone(), "verify-evidence", tx, move |ctx| {
        case::verify_evidence(&ws_job, &engine, &inv_job, only_job.as_deref(), ctx)
    });
    state
        .jobs
        .lock()
        .insert(job_id.clone(), handle.control.clone());

    let watcher_app = app.clone();
    let watch_job = job_id.clone();
    std::thread::spawn(move || {
        let result = handle.join();
        let state = watcher_app.state::<AppState>();
        state.jobs.lock().remove(&watch_job);
        let payload = match result {
            Ok(report) => VerifyFinishedDto {
                job_id: watch_job.clone(),
                investigation_id,
                report: Some(report_dto(report)),
                error: None,
            },
            Err(e) => VerifyFinishedDto {
                job_id: watch_job.clone(),
                investigation_id,
                report: None,
                error: Some(jerr(&e)),
            },
        };
        let _ = watcher_app.emit("verify-finished", &payload);
    });

    Ok(VerifyStartedDto { job_id, total })
}

fn report_dto(r: case::VerificationReport) -> VerificationReportDto {
    VerificationReportDto {
        total: r.total as i64,
        updated: r.updated as i64,
        cancelled: r.cancelled,
        dataset_lookups: r.dataset_lookups as i64,
        states: r.states,
        outcomes: r
            .outcomes
            .into_iter()
            .map(|o| EvidenceOutcomeDto {
                evidence_id: o.evidence_id,
                state: o.state,
            })
            .collect(),
        duration_ms: r.duration_ms as i64,
    }
}

// ---- reports ------------------------------------------------------------

fn report_def_dto(row: ReportDefRow) -> CmdResult<ReportDefDto> {
    let sections: Vec<SectionDto> = serde_json::from_str(&row.sections_json)
        .map_err(|e| err("report/invalid-definition", e))?;
    let selected_evidence: Vec<SelectedRefDto> = serde_json::from_str(&row.selected_evidence_json)
        .map_err(|e| err("report/invalid-definition", e))?;
    let selected_markers: Vec<SelectedRefDto> = serde_json::from_str(&row.selected_markers_json)
        .map_err(|e| err("report/invalid-definition", e))?;
    Ok(ReportDefDto {
        report_def_id: row.report_def_id,
        investigation_id: row.investigation_id,
        title: row.title,
        subtitle: row.subtitle,
        sections,
        selected_evidence,
        selected_markers,
        created_at: row.created_at,
        updated_at: row.updated_at,
        revision: row.revision,
    })
}

fn artifact_dto(row: ReportArtifactRow) -> ReportArtifactDto {
    ReportArtifactDto {
        artifact_id: row.artifact_id,
        report_def_id: row.report_def_id,
        format: row.format,
        destination_path: row.destination_path,
        checksum_sha256: row.checksum_sha256,
        byte_size: row.byte_size,
        status: row.status,
        error_json: row.error_json,
        created_at: row.created_at,
        finished_at: row.finished_at,
    }
}

/// Serializes and fail-fast-validates the sections list.
fn sections_json_of(sections: &[SectionDto]) -> CmdResult<String> {
    let json = serde_json::to_string(sections).map_err(|e| err("report/invalid-definition", e))?;
    report::parse_sections(&json).map_err(|e| jerr(&e))?;
    Ok(json)
}

fn refs_json_of(refs: &[SelectedRefDto]) -> CmdResult<String> {
    serde_json::to_string(refs).map_err(|e| err("report/invalid-definition", e))
}

#[tauri::command]
pub fn create_report_def(
    state: State<'_, AppState>,
    request: NewReportDefDto,
) -> CmdResult<ReportDefDto> {
    let ws = ws_handle(&state)?;
    let new = NewReportDef {
        report_def_id: format!("rep-{}", uuid::Uuid::new_v4()),
        investigation_id: request.investigation_id,
        title: request.title,
        subtitle: request.subtitle,
        sections_json: sections_json_of(&request.sections)?,
        selected_evidence_json: refs_json_of(&request.selected_evidence)?,
        selected_markers_json: refs_json_of(&request.selected_markers)?,
        options_json: "{}".into(),
    };
    let row = ws.meta.create_report_def(&new).map_err(ws_err)?;
    report_def_dto(row)
}

#[tauri::command]
pub fn update_report_def(
    state: State<'_, AppState>,
    request: ReportDefEditDto,
) -> CmdResult<ReportDefDto> {
    let ws = ws_handle(&state)?;
    let edit = ReportDefEdit {
        report_def_id: request.report_def_id,
        expected_revision: request.expected_revision,
        title: request.title,
        subtitle: request.subtitle,
        sections_json: sections_json_of(&request.sections)?,
        selected_evidence_json: refs_json_of(&request.selected_evidence)?,
        selected_markers_json: refs_json_of(&request.selected_markers)?,
        options_json: "{}".into(),
    };
    let row = ws.meta.update_report_def(&edit).map_err(ws_err)?;
    report_def_dto(row)
}

#[tauri::command]
pub fn list_report_defs(
    state: State<'_, AppState>,
    investigation_id: String,
) -> CmdResult<Vec<ReportDefDto>> {
    let ws = ws_handle(&state)?;
    ws.meta
        .list_report_defs(&investigation_id)
        .map_err(ws_err)?
        .into_iter()
        .map(report_def_dto)
        .collect()
}

#[tauri::command]
pub fn generate_report(
    state: State<'_, AppState>,
    report_def_id: String,
    format: String,
    destination: String,
) -> CmdResult<ReportArtifactDto> {
    let ws = ws_handle(&state)?;
    let fmt = ReportFormat::parse(&format).ok_or_else(|| {
        err(
            "report/invalid-format",
            format!("unknown format {format:?}"),
        )
    })?;
    report::generate_report(&ws, &report_def_id, fmt, &PathBuf::from(destination))
        .map(artifact_dto)
        .map_err(|e| jerr(&e))
}

#[tauri::command]
pub fn list_report_artifacts(
    state: State<'_, AppState>,
    investigation_id: String,
) -> CmdResult<Vec<ReportArtifactDto>> {
    let ws = ws_handle(&state)?;
    Ok(ws
        .meta
        .list_report_artifacts(&investigation_id)
        .map_err(ws_err)?
        .into_iter()
        .map(artifact_dto)
        .collect())
}

// ---- jump-back ----------------------------------------------------------

/// Decodes one evidence reference into restore instructions for the
/// Explorer. The captured context is returned verbatim — exactly the
/// query, datasets, and bounds that were pinned, never broadened.
#[tauri::command]
pub fn evidence_restore_context(
    state: State<'_, AppState>,
    evidence_id: String,
) -> CmdResult<RestoreContextDto> {
    let ws = ws_handle(&state)?;
    let row = ws
        .meta
        .get_evidence(&evidence_id)
        .map_err(ws_err)?
        .ok_or_else(|| {
            err(
                "workspace/missing-entity",
                format!("evidence {evidence_id} does not exist"),
            )
        })?;

    let reference = match envelope::decode_reference(row.envelope_version, &row.reference_json) {
        DecodeOutcome::Decoded(r) => r,
        DecodeOutcome::UnsupportedVersion { stored, supported } => {
            return Err(err(
                "case/unsupported-envelope",
                format!(
                    "evidence was written by a newer build \
                     (envelope {stored}, supported {supported})"
                ),
            ))
        }
        DecodeOutcome::Undecodable { error } => {
            return Err(err(
                "case/undecodable-reference",
                format!("the stored reference cannot be interpreted: {error}"),
            ))
        }
    };

    let empty = RestoreContextDto {
        kind: row.kind.clone(),
        query_text: None,
        dataset_ids: Vec::new(),
        time_strategy: None,
        resolved_start: None,
        resolved_end: None,
        record_id: None,
        dataset_id: None,
        record_ids: Vec::new(),
        interval_start: None,
        interval_end: None,
        item_id: None,
    };

    let from_context = |ctx: &envelope::QueryContext, base: RestoreContextDto| RestoreContextDto {
        query_text: Some(ctx.query_text.clone()),
        dataset_ids: ctx.dataset_ids.clone(),
        time_strategy: Some(strategy_from_json(&ctx.time_strategy_json)),
        resolved_start: ctx.resolved_start,
        resolved_end: ctx.resolved_end,
        ..base
    };

    Ok(match reference {
        EvidenceReference::Event(e) => RestoreContextDto {
            record_id: Some(e.record_id),
            dataset_id: Some(e.dataset_id.clone()),
            dataset_ids: vec![e.dataset_id],
            ..empty
        },
        EvidenceReference::Selection(s) => {
            let base = from_context(&s.context, empty);
            RestoreContextDto {
                record_ids: s.record_ids,
                ..base
            }
        }
        EvidenceReference::Query(q) => from_context(&q.context, empty),
        EvidenceReference::ExplorerGroup(g) => {
            let base = from_context(&g.context, empty);
            RestoreContextDto {
                // The composed query selects exactly the pinned group.
                query_text: Some(case::compose_group_query(
                    &g.context.query_text,
                    &g.predicate_text,
                )),
                ..base
            }
        }
        EvidenceReference::HistogramInterval(i) => {
            let base = from_context(&i.context, empty);
            RestoreContextDto {
                interval_start: Some(i.start),
                interval_end: Some(i.end),
                ..base
            }
        }
        EvidenceReference::ItemRef(i) => RestoreContextDto {
            item_id: Some(i.item_id),
            ..empty
        },
    })
}
