import type { CoreSnapshot } from "./backend";

export const PROTOCOL_VERSION = 1 as const;

export function parseSnapshotResult(value: unknown): CoreSnapshot {
  const raw = asRecord(value);
  if (!raw) throw new Error("Backend returned an invalid snapshot");
  return raw as unknown as CoreSnapshot;
}

export function asRecord(value: unknown): Record<string, unknown> | null {
  return typeof value === "object" && value !== null && !Array.isArray(value)
    ? (value as Record<string, unknown>)
    : null;
}
