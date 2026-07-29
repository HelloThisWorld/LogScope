// Typed wrappers over the Tauri command boundary. Types are generated from
// the Rust DTOs by ts-rs (`cargo test -p logscope-app export_bindings`).

import { invoke } from "@tauri-apps/api/core";
import type { WorkspaceInfoDto } from "./bindings/WorkspaceInfoDto";
import type { OverviewDto } from "./bindings/OverviewDto";
import type { StartImportDto } from "./bindings/StartImportDto";
import type { LogQueryDto } from "./bindings/LogQueryDto";
import type { LogPageDto } from "./bindings/LogPageDto";
import type { ErrorDto } from "./bindings/ErrorDto";

export type {
  WorkspaceInfoDto,
  OverviewDto,
  StartImportDto,
  LogQueryDto,
  LogPageDto,
  ErrorDto,
};

export function isErrorDto(e: unknown): e is ErrorDto {
  return (
    typeof e === "object" &&
    e !== null &&
    "code" in e &&
    "message" in e
  );
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
  queryLogs: (request: LogQueryDto) =>
    invoke<LogPageDto>("query_logs", { request }),
};
