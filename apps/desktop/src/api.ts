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
};

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
};
