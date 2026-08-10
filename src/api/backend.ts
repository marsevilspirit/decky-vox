import {
  addEventListener,
  callable,
  removeEventListener,
} from "@decky/api";

import type { Settings } from "../domain/settings";
import { asRecord, parseSnapshotResult } from "./protocol";

export { PROTOCOL_VERSION, asRecord, parseSnapshotResult } from "./protocol";
export const CORE_EVENT_NAME = "decky_vox_event";
export const BRIDGE_STATUS_EVENT_NAME = "decky_vox_bridge_status";

export type BackendPhase =
  | "stopped"
  | "setup_required"
  | "ready"
  | "recording"
  | "transcribing"
  | "failed";

export interface CoreSnapshot {
  protocol_version: number;
  instance_id: string;
  seq: number;
  settings: Settings;
  phase: BackendPhase;
  enabled: boolean;
  model_installed: boolean;
  engine_backend: string | null;
  error: string | { code?: string; message?: string } | null;
}

export interface HelloResult {
  protocol_version: number;
  instance_id: string;
  capabilities?: readonly string[];
}

export interface CoreEvent {
  v: number;
  kind: "event";
  instance_id: string;
  seq: number;
  name: string;
  payload: unknown;
}

export interface BridgeStatus {
  bridge_instance: string;
  status: string;
  code: string;
  message?: string;
}

export interface OutputEventPayload {
  session_id: number;
  ok: boolean;
  text: string;
  error?: string | null;
  error_code?: string | null;
}

export interface ModelProgressPayload {
  model?: string;
  downloaded_bytes?: number;
  total_bytes?: number;
  percent?: number;
  status?: string;
}

const helloCall = callable<[], unknown>("hello");
const getSnapshotCall = callable<[], unknown>("get_snapshot");
const updateSettingsCall = callable<[settings: Settings], unknown>("update_settings");
const setEnabledCall = callable<[enabled: boolean], unknown>("set_enabled");
const recordStartCall = callable<[sessionId: number], unknown>("record_start");
const recordStopCall = callable<[sessionId: number], unknown>("record_stop");
const cancelSessionCall = callable<[sessionId: number | null], unknown>("cancel_session");
const installModelCall = callable<[model: string], unknown>("install_model");
const cancelModelCall = callable<[], unknown>("cancel_model");

export interface BackendClient {
  hello(): Promise<HelloResult>;
  getSnapshot(): Promise<CoreSnapshot>;
  updateSettings(settings: Settings): Promise<CoreSnapshot>;
  setEnabled(enabled: boolean): Promise<CoreSnapshot>;
  recordStart(sessionId: number): Promise<CoreSnapshot>;
  recordStop(sessionId: number): Promise<CoreSnapshot>;
  cancelSession(sessionId: number | null): Promise<CoreSnapshot | null>;
  installModel(model: string): Promise<void>;
  cancelModel(): Promise<void>;
  onCoreEvent(listener: (event: CoreEvent) => void): () => void;
  onBridgeStatus(listener: (status: BridgeStatus) => void): () => void;
}

export function createBackendClient(): BackendClient {
  return {
    async hello() {
      return parseHello(await helloCall());
    },
    async getSnapshot() {
      return parseSnapshotResult(await getSnapshotCall());
    },
    async updateSettings(settings) {
      return parseSnapshotResult(await updateSettingsCall(settings));
    },
    async setEnabled(enabled) {
      return parseSnapshotResult(await setEnabledCall(enabled));
    },
    async recordStart(sessionId) {
      return parseSnapshotResult(await recordStartCall(sessionId));
    },
    async recordStop(sessionId) {
      return parseSnapshotResult(await recordStopCall(sessionId));
    },
    async cancelSession(sessionId) {
      return parseOptionalSnapshotResult(await cancelSessionCall(sessionId));
    },
    async installModel(model) {
      await installModelCall(model);
    },
    async cancelModel() {
      await cancelModelCall();
    },
    onCoreEvent(listener) {
      const registration = addEventListener<[event: unknown]>(CORE_EVENT_NAME, (raw) => {
        const event = parseCoreEvent(raw);
        if (event) listener(event);
      });
      return () => removeEventListener(CORE_EVENT_NAME, registration);
    },
    onBridgeStatus(listener) {
      const registration = addEventListener<[status: unknown]>(
        BRIDGE_STATUS_EVENT_NAME,
        (raw) => {
          const status = parseBridgeStatus(raw);
          if (status) listener(status);
        },
      );
      return () => removeEventListener(BRIDGE_STATUS_EVENT_NAME, registration);
    },
  };
}

function parseOptionalSnapshotResult(value: unknown): CoreSnapshot | null {
  if (value === null || value === undefined) return null;
  const outer = asRecord(value);
  if (outer && outer.snapshot === null) return null;
  return parseSnapshotResult(value);
}

function parseHello(value: unknown): HelloResult {
  const outer = asRecord(value);
  const raw = outer && "hello" in outer ? asRecord(outer.hello) : outer;
  if (
    !raw ||
    typeof raw.protocol_version !== "number" ||
    typeof raw.instance_id !== "string"
  ) {
    throw new Error("Backend returned an invalid hello response");
  }
  return raw as unknown as HelloResult;
}

function parseCoreEvent(value: unknown): CoreEvent | null {
  const raw = asRecord(value);
  if (
    !raw ||
    typeof raw.v !== "number" ||
    raw.kind !== "event" ||
    typeof raw.instance_id !== "string" ||
    typeof raw.seq !== "number" ||
    typeof raw.name !== "string"
  ) {
    return null;
  }
  return raw as unknown as CoreEvent;
}

function parseBridgeStatus(value: unknown): BridgeStatus | null {
  const raw = asRecord(value);
  if (
    !raw ||
    typeof raw.bridge_instance !== "string" ||
    typeof raw.status !== "string" ||
    typeof raw.code !== "string"
  ) {
    return null;
  }
  return raw as unknown as BridgeStatus;
}
