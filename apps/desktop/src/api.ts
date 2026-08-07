// Typed wrappers over the Tauri command boundary. Types are generated from
// the Rust DTOs by ts-rs (`cargo test -p logscope-app export_bindings`).

import { invoke } from "@tauri-apps/api/core";
import type { WorkspaceInfoDto } from "./bindings/WorkspaceInfoDto";
import type { OverviewDto } from "./bindings/OverviewDto";
import type { StartImportDto } from "./bindings/StartImportDto";
import type { ErrorDto } from "./bindings/ErrorDto";
import type { QueryAnalysisDto } from "./bindings/QueryAnalysisDto";
import type { FieldCatalogDto } from "./bindings/FieldCatalogDto";
import type { RunQueryDto } from "./bindings/RunQueryDto";
import type { QueryPageV2Dto } from "./bindings/QueryPageV2Dto";
import type { HistogramRequestDto } from "./bindings/HistogramRequestDto";
import type { HistogramDto } from "./bindings/HistogramDto";
import type { FacetsRequestDto } from "./bindings/FacetsRequestDto";
import type { FacetDto } from "./bindings/FacetDto";
import type { FieldSummaryRequestDto } from "./bindings/FieldSummaryRequestDto";
import type { FieldSummaryDto } from "./bindings/FieldSummaryDto";
import type { RecordDetailDto } from "./bindings/RecordDetailDto";
import type { SourceContextRequestDto } from "./bindings/SourceContextRequestDto";
import type { SourceContextDto } from "./bindings/SourceContextDto";
import type { SavedSearchDto } from "./bindings/SavedSearchDto";
import type { ColumnSetDto } from "./bindings/ColumnSetDto";
import type { RecentSearchDto } from "./bindings/RecentSearchDto";
import type { TimeStrategyDto } from "./bindings/TimeStrategyDto";
import type { StartExportDto } from "./bindings/StartExportDto";
import type { ExportStatusDto } from "./bindings/ExportStatusDto";
import type { IndexStateDto } from "./bindings/IndexStateDto";
import type { LogRowV2Dto } from "./bindings/LogRowV2Dto";
import type { DiagnosticDto } from "./bindings/DiagnosticDto";
import type { HighlightDto } from "./bindings/HighlightDto";
import type { FieldInfoDto } from "./bindings/FieldInfoDto";
import type { InvestigationDto } from "./bindings/InvestigationDto";
import type { NewInvestigationDto } from "./bindings/NewInvestigationDto";
import type { InvestigationEditDto } from "./bindings/InvestigationEditDto";
import type { InvestigationBundleDto } from "./bindings/InvestigationBundleDto";
import type { HypothesisDto } from "./bindings/HypothesisDto";
import type { ItemDto } from "./bindings/ItemDto";
import type { NewItemDto } from "./bindings/NewItemDto";
import type { EvidenceDto } from "./bindings/EvidenceDto";
import type { EvidenceGroupDto } from "./bindings/EvidenceGroupDto";
import type { HistoryDto } from "./bindings/HistoryDto";
import type { PinEventDto } from "./bindings/PinEventDto";
import type { PinSelectionDto } from "./bindings/PinSelectionDto";
import type { PinQueryDto } from "./bindings/PinQueryDto";
import type { PinGroupDto } from "./bindings/PinGroupDto";
import type { PinIntervalDto } from "./bindings/PinIntervalDto";
import type { PinItemDto } from "./bindings/PinItemDto";
import type { VerifyStartedDto } from "./bindings/VerifyStartedDto";
import type { VerifyFinishedDto } from "./bindings/VerifyFinishedDto";
import type { VerificationReportDto } from "./bindings/VerificationReportDto";
import type { RestoreContextDto } from "./bindings/RestoreContextDto";
import type { MarkerDto } from "./bindings/MarkerDto";
import type { NewMarkerDto } from "./bindings/NewMarkerDto";
import type { MarkerEditDto } from "./bindings/MarkerEditDto";
import type { TimelineDto } from "./bindings/TimelineDto";
import type { TimelineEntryDto } from "./bindings/TimelineEntryDto";
import type { SectionDto } from "./bindings/SectionDto";
import type { SelectedRefDto } from "./bindings/SelectedRefDto";
import type { ReportDefDto } from "./bindings/ReportDefDto";
import type { NewReportDefDto } from "./bindings/NewReportDefDto";
import type { ReportDefEditDto } from "./bindings/ReportDefEditDto";
import type { ReportArtifactDto } from "./bindings/ReportArtifactDto";
import type { RedactionProfileDto } from "./bindings/RedactionProfileDto";
import type { BundleExportDto } from "./bindings/BundleExportDto";
import type { BundleImportSummaryDto } from "./bindings/BundleImportSummaryDto";

export type {
  WorkspaceInfoDto,
  OverviewDto,
  StartImportDto,
  ErrorDto,
  QueryAnalysisDto,
  FieldCatalogDto,
  RunQueryDto,
  QueryPageV2Dto,
  HistogramDto,
  FacetDto,
  FieldSummaryDto,
  RecordDetailDto,
  SourceContextDto,
  SavedSearchDto,
  ColumnSetDto,
  RecentSearchDto,
  TimeStrategyDto,
  ExportStatusDto,
  IndexStateDto,
  LogRowV2Dto,
  DiagnosticDto,
  HighlightDto,
  FieldInfoDto,
  InvestigationDto,
  NewInvestigationDto,
  InvestigationEditDto,
  InvestigationBundleDto,
  HypothesisDto,
  ItemDto,
  NewItemDto,
  EvidenceDto,
  EvidenceGroupDto,
  HistoryDto,
  PinEventDto,
  PinSelectionDto,
  PinQueryDto,
  PinGroupDto,
  PinIntervalDto,
  PinItemDto,
  VerifyStartedDto,
  VerifyFinishedDto,
  VerificationReportDto,
  RestoreContextDto,
  MarkerDto,
  NewMarkerDto,
  MarkerEditDto,
  TimelineDto,
  TimelineEntryDto,
  SectionDto,
  SelectedRefDto,
  ReportDefDto,
  NewReportDefDto,
  ReportDefEditDto,
  ReportArtifactDto,
  RedactionProfileDto,
  BundleExportDto,
  BundleImportSummaryDto,
};

/// Section vocabulary in canonical order (mirrors report::SECTION_KINDS).
export const REPORT_SECTION_KINDS = [
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
] as const;

export const NARRATIVE_SECTION_KINDS = [
  "summary",
  "impact",
  "symptoms",
  "root_cause",
  "resolution",
  "validation",
  "follow_up",
] as const;

export const MARKER_KINDS = [
  "deployment",
  "config_change",
  "operator_action",
  "custom",
] as const;

/// Vocabulary mirrors of `logscope-case::vocab` (storage strings).
export const INVESTIGATION_STATUSES = [
  "open",
  "investigating",
  "mitigated",
  "resolved",
  "archived",
] as const;
export const SEVERITIES = ["sev1", "sev2", "sev3", "sev4"] as const;
export const HYPOTHESIS_STATES = [
  "unverified",
  "supported",
  "rejected",
  "confirmed",
] as const;
export const ITEM_KINDS = ["note", "task", "finding", "question"] as const;
export const TASK_STATUSES = ["todo", "doing", "done", "dropped"] as const;
export const QUESTION_STATUSES = ["open", "answered", "deferred"] as const;

export function isStaleRevision(e: unknown): boolean {
  return isErrorDto(e) && e.code === "workspace/stale-revision";
}

export function isErrorDto(e: unknown): e is ErrorDto {
  return typeof e === "object" && e !== null && "code" in e && "message" in e;
}

export function errorText(e: unknown): string {
  if (isErrorDto(e)) return `[${e.code}] ${e.message}`;
  return String(e);
}

export const api = {
  recentWorkspaces: () => invoke<string[]>("recent_workspaces"),
  createWorkspace: (path: string, name: string) =>
    invoke<WorkspaceInfoDto>("create_workspace", { path, name }),
  openWorkspace: (path: string) =>
    invoke<WorkspaceInfoDto>("open_workspace", { path }),
  closeWorkspace: () => invoke<boolean>("close_workspace"),
  overview: () => invoke<OverviewDto>("overview"),
  startImport: (request: StartImportDto) =>
    invoke<string>("start_import", { request }),
  cancelJob: (jobId: string) => invoke<boolean>("cancel_job", { jobId }),

  // ---- v0.2 Explorer ----
  validateQuery: (datasetIds: string[], queryText: string) =>
    invoke<QueryAnalysisDto>("validate_query", { datasetIds, queryText }),
  fieldCatalog: (datasetIds: string[]) =>
    invoke<FieldCatalogDto>("field_catalog", { datasetIds }),
  runQuery: (request: RunQueryDto) =>
    invoke<QueryPageV2Dto>("run_query", { request }),
  runHistogram: (request: HistogramRequestDto) =>
    invoke<HistogramDto>("run_histogram", { request }),
  runFacets: (request: FacetsRequestDto) =>
    invoke<FacetDto[]>("run_facets", { request }),
  fieldSummary: (request: FieldSummaryRequestDto) =>
    invoke<FieldSummaryDto>("field_summary", { request }),
  cancelQuery: (requestId: string) =>
    invoke<boolean>("cancel_query", { requestId }),
  getRecord: (datasetId: string, recordId: string) =>
    invoke<RecordDetailDto>("get_record", { datasetId, recordId }),
  sourceContext: (request: SourceContextRequestDto) =>
    invoke<SourceContextDto>("source_context", { request }),
  buildPredicate: (field: string, value: string, negate: boolean) =>
    invoke<string>("build_predicate", { field, value, negate }),
  buildMissingPredicate: (field: string) =>
    invoke<string>("build_missing_predicate", { field }),
  savedSearches: () => invoke<SavedSearchDto[]>("saved_searches"),
  saveSearch: (args: {
    savedSearchId: string | null;
    name: string;
    queryText: string;
    datasetIds: string[];
    timeStrategy: TimeStrategyDto;
  }) => invoke<string>("save_search", args),
  deleteSavedSearch: (savedSearchId: string) =>
    invoke<boolean>("delete_saved_search", { savedSearchId }),
  columnSets: () => invoke<ColumnSetDto[]>("column_sets"),
  saveColumnSet: (args: {
    columnSetId: string | null;
    name: string;
    columns: string[];
    isDefault: boolean;
  }) => invoke<string>("save_column_set", args),
  deleteColumnSet: (columnSetId: string) =>
    invoke<boolean>("delete_column_set", { columnSetId }),
  recentSearches: () => invoke<RecentSearchDto[]>("recent_searches"),
  deleteRecentSearch: (recentId: number) =>
    invoke<boolean>("delete_recent_search", { recentId }),
  clearRecentSearches: () => invoke<boolean>("clear_recent_searches"),
  startExport: (request: StartExportDto) =>
    invoke<ExportStatusDto>("start_export", { request }),
  exportStatus: (exportId: string) =>
    invoke<ExportStatusDto>("export_status", { exportId }),
  indexStatus: () => invoke<IndexStateDto[]>("index_status"),
  rebuildIndexes: () => invoke<string>("rebuild_indexes"),
  listImportProfiles: () => invoke<[string, string][]>("list_import_profiles"),

  // ---- v0.3 investigations + evidence ----
  listInvestigations: (includeArchived: boolean) =>
    invoke<InvestigationDto[]>("list_investigations", { includeArchived }),
  createInvestigation: (request: NewInvestigationDto) =>
    invoke<InvestigationDto>("create_investigation", { request }),
  updateInvestigation: (request: InvestigationEditDto) =>
    invoke<InvestigationDto>("update_investigation", { request }),
  setInvestigationStatus: (
    investigationId: string,
    expectedRevision: number,
    status: string,
  ) =>
    invoke<InvestigationDto>("set_investigation_status", {
      investigationId,
      expectedRevision,
      status,
    }),
  investigationBundle: (investigationId: string) =>
    invoke<InvestigationBundleDto>("investigation_bundle", { investigationId }),
  investigationActivity: (investigationId: string, limit?: number) =>
    invoke<HistoryDto[]>("investigation_activity", { investigationId, limit }),

  createHypothesis: (
    investigationId: string,
    statement: string,
    rationale: string | null,
  ) =>
    invoke<HypothesisDto>("create_hypothesis", {
      investigationId,
      statement,
      rationale,
    }),
  updateHypothesis: (
    hypothesisId: string,
    expectedRevision: number,
    statement: string,
    rationale: string | null,
  ) =>
    invoke<HypothesisDto>("update_hypothesis", {
      hypothesisId,
      expectedRevision,
      statement,
      rationale,
    }),
  setHypothesisState: (
    hypothesisId: string,
    expectedRevision: number,
    newState: string,
  ) =>
    invoke<HypothesisDto>("set_hypothesis_state", {
      hypothesisId,
      expectedRevision,
      newState,
    }),
  linkHypothesisEvidence: (
    hypothesisId: string,
    expectedRevision: number,
    evidenceId: string,
  ) =>
    invoke<HypothesisDto>("link_hypothesis_evidence", {
      hypothesisId,
      expectedRevision,
      evidenceId,
    }),
  unlinkHypothesisEvidence: (
    hypothesisId: string,
    expectedRevision: number,
    evidenceId: string,
  ) =>
    invoke<HypothesisDto>("unlink_hypothesis_evidence", {
      hypothesisId,
      expectedRevision,
      evidenceId,
    }),

  createItem: (request: NewItemDto) =>
    invoke<ItemDto>("create_item", { request }),
  updateItemContent: (
    itemId: string,
    expectedRevision: number,
    content: string,
  ) =>
    invoke<ItemDto>("update_item_content", {
      itemId,
      expectedRevision,
      content,
    }),
  setItemStatus: (
    itemId: string,
    expectedRevision: number,
    taskStatus: string | null,
    questionStatus: string | null,
  ) =>
    invoke<ItemDto>("set_item_status", {
      itemId,
      expectedRevision,
      taskStatus,
      questionStatus,
    }),
  setItemArchived: (
    itemId: string,
    expectedRevision: number,
    archived: boolean,
  ) =>
    invoke<ItemDto>("set_item_archived", { itemId, expectedRevision, archived }),
  reorderCaseChildren: (
    investigationId: string,
    expectedInvestigationRevision: number,
    entityKind: string,
    orderedIds: string[],
  ) =>
    invoke<InvestigationDto>("reorder_case_children", {
      investigationId,
      expectedInvestigationRevision,
      entityKind,
      orderedIds,
    }),

  createEvidenceGroup: (investigationId: string, name: string) =>
    invoke<EvidenceGroupDto>("create_evidence_group", {
      investigationId,
      name,
    }),
  renameEvidenceGroup: (
    groupId: string,
    expectedRevision: number,
    name: string,
  ) =>
    invoke<EvidenceGroupDto>("rename_evidence_group", {
      groupId,
      expectedRevision,
      name,
    }),
  deleteEvidenceGroup: (groupId: string) =>
    invoke<void>("delete_evidence_group", { groupId }),
  updateEvidenceAnnotation: (
    evidenceId: string,
    expectedRevision: number,
    title: string,
    annotation: string | null,
    relevance: string | null,
  ) =>
    invoke<EvidenceDto>("update_evidence_annotation", {
      evidenceId,
      expectedRevision,
      title,
      annotation,
      relevance,
    }),
  setEvidenceGroup: (
    evidenceId: string,
    expectedRevision: number,
    groupId: string | null,
  ) =>
    invoke<EvidenceDto>("set_evidence_group", {
      evidenceId,
      expectedRevision,
      groupId,
    }),
  setEvidenceArchived: (
    evidenceId: string,
    expectedRevision: number,
    archived: boolean,
  ) =>
    invoke<EvidenceDto>("set_evidence_archived", {
      evidenceId,
      expectedRevision,
      archived,
    }),
  evidenceHistory: (evidenceId: string) =>
    invoke<HistoryDto[]>("evidence_history", { evidenceId }),

  pinEvent: (request: PinEventDto) =>
    invoke<EvidenceDto>("pin_event", { request }),
  pinSelection: (request: PinSelectionDto) =>
    invoke<EvidenceDto>("pin_selection", { request }),
  pinQuery: (request: PinQueryDto) =>
    invoke<EvidenceDto>("pin_query", { request }),
  pinGroup: (request: PinGroupDto) =>
    invoke<EvidenceDto>("pin_group", { request }),
  pinInterval: (request: PinIntervalDto) =>
    invoke<EvidenceDto>("pin_interval", { request }),
  pinItem: (request: PinItemDto) =>
    invoke<EvidenceDto>("pin_item", { request }),

  startVerifyEvidence: (investigationId: string, only: string[] | null) =>
    invoke<VerifyStartedDto>("start_verify_evidence", {
      investigationId,
      only,
    }),
  evidenceRestoreContext: (evidenceId: string) =>
    invoke<RestoreContextDto>("evidence_restore_context", { evidenceId }),

  createMarker: (request: NewMarkerDto) =>
    invoke<MarkerDto>("create_marker", { request }),
  updateMarker: (request: MarkerEditDto) =>
    invoke<MarkerDto>("update_marker", { request }),
  setMarkerArchived: (
    markerId: string,
    expectedRevision: number,
    archived: boolean,
  ) =>
    invoke<MarkerDto>("set_marker_archived", {
      markerId,
      expectedRevision,
      archived,
    }),
  investigationTimeline: (investigationId: string) =>
    invoke<TimelineDto>("investigation_timeline", { investigationId }),

  createReportDef: (request: NewReportDefDto) =>
    invoke<ReportDefDto>("create_report_def", { request }),
  updateReportDef: (request: ReportDefEditDto) =>
    invoke<ReportDefDto>("update_report_def", { request }),
  listReportDefs: (investigationId: string) =>
    invoke<ReportDefDto[]>("list_report_defs", { investigationId }),
  generateReport: (
    reportDefId: string,
    format: "markdown" | "html",
    destination: string,
  ) =>
    invoke<ReportArtifactDto>("generate_report", {
      reportDefId,
      format,
      destination,
    }),
  listReportArtifacts: (investigationId: string) =>
    invoke<ReportArtifactDto[]>("list_report_artifacts", { investigationId }),

  createRedactionProfile: (
    name: string,
    rulesJson: string,
    postureJson: string,
  ) =>
    invoke<RedactionProfileDto>("create_redaction_profile", {
      name,
      rulesJson,
      postureJson,
    }),
  updateRedactionProfile: (
    profileId: string,
    expectedRevision: number,
    name: string,
    rulesJson: string,
    postureJson: string,
  ) =>
    invoke<RedactionProfileDto>("update_redaction_profile", {
      profileId,
      expectedRevision,
      name,
      rulesJson,
      postureJson,
    }),
  listRedactionProfiles: () =>
    invoke<RedactionProfileDto[]>("list_redaction_profiles"),
  setReportDefRedaction: (
    reportDefId: string,
    expectedRevision: number,
    profileId: string | null,
  ) =>
    invoke<ReportDefDto>("set_report_def_redaction", {
      reportDefId,
      expectedRevision,
      profileId,
    }),
  previewReport: (reportDefId: string, format: "markdown" | "html") =>
    invoke<string>("preview_report", { reportDefId, format }),

  exportCaseBundle: (
    investigationId: string,
    destination: string,
    redactionProfileId: string | null,
    includeReports: boolean,
  ) =>
    invoke<BundleExportDto>("export_case_bundle", {
      investigationId,
      destination,
      redactionProfileId,
      includeReports,
    }),
  listBundleExports: (investigationId: string) =>
    invoke<BundleExportDto[]>("list_bundle_exports", { investigationId }),
  importCaseBundle: (
    bundlePath: string,
    newWorkspaceRoot: string,
    workspaceName: string,
  ) =>
    invoke<BundleImportSummaryDto>("import_case_bundle", {
      bundlePath,
      newWorkspaceRoot,
      workspaceName,
    }),

  // ---- v0.4 pattern analysis ----
  listAnalysisDefinitions: () =>
    invoke<AnalysisDefinitionDto[]>("list_analysis_definitions"),
  createPatternDefinition: (dto: NewPatternDefinitionDto) =>
    invoke<AnalysisDefinitionDto>("create_pattern_definition", { new: dto }),
  listAnalysisRuns: (definitionId?: string) =>
    invoke<AnalysisRunDto[]>("list_analysis_runs", {
      definitionId: definitionId ?? null,
    }),
  checkAnalysisRun: (runId: string) =>
    invoke<string | null>("check_analysis_run", { runId }),
  startPatternAnalysis: (definitionId: string) =>
    invoke<AnalysisStartedDto>("start_pattern_analysis", { definitionId }),
  listPatterns: (runId: string, offset: number, limit: number) =>
    invoke<PatternSummaryDto[]>("list_patterns", { runId, offset, limit }),
  patternRecords: (runId: string, patternId: string, limit: number) =>
    invoke<LogRowV2Dto[]>("pattern_records", { runId, patternId, limit }),

  // ---- v0.4 window comparison ----
  createComparisonDefinition: (dto: NewComparisonDefinitionDto) =>
    invoke<AnalysisDefinitionDto>("create_comparison_definition", { new: dto }),
  startComparisonAnalysis: (definitionId: string) =>
    invoke<AnalysisStartedDto>("start_comparison_analysis", { definitionId }),
  listComparisonResults: (runId: string, offset: number, limit: number) =>
    invoke<ComparisonResultDto[]>("list_comparison_results", {
      runId,
      offset,
      limit,
    }),
  comparisonRecords: (
    runId: string,
    key: string,
    side: "baseline" | "suspect",
    limit: number,
  ) => invoke<LogRowV2Dto[]>("comparison_records", { runId, key, side, limit }),

  // ---- v0.4 correlation ----
  createCorrelationDefinition: (dto: NewCorrelationDefinitionDto) =>
    invoke<AnalysisDefinitionDto>("create_correlation_definition", { new: dto }),
  startCorrelationAnalysis: (definitionId: string) =>
    invoke<AnalysisStartedDto>("start_correlation_analysis", { definitionId }),
  listCorrelationGroups: (runId: string, offset: number, limit: number) =>
    invoke<CorrelationGroupDto[]>("list_correlation_groups", {
      runId,
      offset,
      limit,
    }),
  listCorrelationEdges: (runId: string, groupId: string, limit: number) =>
    invoke<CorrelationEdgeDto[]>("list_correlation_edges", {
      runId,
      groupId,
      limit,
    }),
  listCorrelationSignals: (runId: string, groupId: string, limit: number) =>
    invoke<CorrelationSignalDto[]>("list_correlation_signals", {
      runId,
      groupId,
      limit,
    }),
  correlationRecords: (runId: string, groupId: string, limit: number) =>
    invoke<LogRowV2Dto[]>("correlation_records", { runId, groupId, limit }),
  probableNeighborhood: (
    runId: string,
    anchorRecordId: string,
    compatibleFields: string[],
    // A duration, not an absolute time: 2^53 ns is ~104 days, so this
    // stays a number while epoch-nanosecond values remain bigint.
    toleranceNanos: number,
    maxNeighbors: number,
  ) =>
    invoke<ProbableNeighborhoodDto>("probable_neighborhood", {
      runId,
      anchorRecordId,
      compatibleFields,
      toleranceNanos,
      maxNeighbors,
    }),
};

// ---- v0.4 pattern analysis types --------------------------------------

import type { AnalysisDefinitionDto } from "./bindings/AnalysisDefinitionDto";
import type { NewPatternDefinitionDto } from "./bindings/NewPatternDefinitionDto";
import type { AnalysisRunDto } from "./bindings/AnalysisRunDto";
import type { AnalysisStartedDto } from "./bindings/AnalysisStartedDto";
import type { PatternSummaryDto } from "./bindings/PatternSummaryDto";
import type { NewComparisonDefinitionDto } from "./bindings/NewComparisonDefinitionDto";
import type { ComparisonResultDto } from "./bindings/ComparisonResultDto";
import type { NewCorrelationDefinitionDto } from "./bindings/NewCorrelationDefinitionDto";
import type { CorrelationGroupDto } from "./bindings/CorrelationGroupDto";
import type { CorrelationEdgeDto } from "./bindings/CorrelationEdgeDto";
import type { CorrelationSignalDto } from "./bindings/CorrelationSignalDto";
import type { ProbableNeighborhoodDto } from "./bindings/ProbableNeighborhoodDto";
import type { ProbableNeighborDto } from "./bindings/ProbableNeighborDto";

export type {
  AnalysisDefinitionDto,
  NewPatternDefinitionDto,
  AnalysisRunDto,
  AnalysisStartedDto,
  PatternSummaryDto,
  NewComparisonDefinitionDto,
  ComparisonResultDto,
  NewCorrelationDefinitionDto,
  CorrelationGroupDto,
  CorrelationEdgeDto,
  CorrelationSignalDto,
  ProbableNeighborhoodDto,
  ProbableNeighborDto,
};
export type { AnalysisFinishedDto } from "./bindings/AnalysisFinishedDto";
