import type {
  BackendClient,
  BackendPhase,
  BridgeStatus,
  CoreEvent,
  CoreSnapshot,
  ModelProgressPayload,
  OutputEventPayload,
} from "../api/backend";
import { PROTOCOL_VERSION, asRecord, parseSnapshotResult } from "../api/protocol";
import { decideTranscriptOutput } from "../domain/outputPolicy";
import { PttMachine } from "../domain/pttMachine";
import type { ControllerButtonEvent, PttAction } from "../domain/pttMachine";
import {
  DEFAULT_SETTINGS,
  bindingsEqual,
  normalizeSettings,
} from "../domain/settings";
import type { OutputMode, Settings } from "../domain/settings";
import {
  registerControllerInput,
} from "./controllerInput";
import type { ControllerInputRegistration } from "./controllerInput";
import type { OutputCoordinator, OutputOutcome } from "./outputCoordinator";

export type VisibleStatus =
  | "Connecting"
  | "Stopped"
  | "Setup required"
  | "Ready"
  | "Recording"
  | "Transcribing"
  | "Failed";

export interface RuntimeState {
  initialized: boolean;
  settings: Settings;
  phase: BackendPhase;
  enabled: boolean;
  controllerReady: boolean;
  controllerError: string | null;
  modelInstalled: boolean;
  modelDownloading: boolean;
  modelProgressPercent: number | null;
  engineBackend: string | null;
  lastOutcome: OutputOutcome | null;
  error: string | null;
  bridgeCode: string | null;
}

interface ActiveSession {
  id: number;
  instanceId: string;
  startOutputMode: OutputMode;
  outputConsumed: boolean;
  cancelled: boolean;
}

type StateListener = () => void;

type ControllerRegistrar = (
  handler: (event: ControllerButtonEvent) => void,
) => ControllerInputRegistration;

interface RuntimeDependencies {
  /** @internal Test seam; production uses SteamClient registration. */
  registerController?: ControllerRegistrar;
}

const INITIAL_STATE: RuntimeState = {
  initialized: false,
  settings: { ...DEFAULT_SETTINGS },
  phase: "stopped",
  enabled: false,
  controllerReady: false,
  controllerError: null,
  modelInstalled: false,
  modelDownloading: false,
  modelProgressPercent: null,
  engineBackend: null,
  lastOutcome: null,
  error: null,
  bridgeCode: null,
};

export class DeckyVoxRuntime {
  private state: RuntimeState = { ...INITIAL_STATE, settings: { ...DEFAULT_SETTINGS } };
  private readonly listeners = new Set<StateListener>();
  private readonly ptt = new PttMachine({
    mode: DEFAULT_SETTINGS.ptt_mode,
    primary: DEFAULT_SETTINGS.controller_primary,
    secondary: DEFAULT_SETTINGS.controller_secondary,
  });
  private controller: ControllerInputRegistration | null = null;
  private removeCoreListener: (() => void) | null = null;
  private removeBridgeListener: (() => void) | null = null;
  private currentInstanceId: string | null = null;
  private lastEventSeq = -1;
  private nextSessionId = 1;
  private currentSession: ActiveSession | null = null;
  private autoSendAcknowledged = false;
  private commandTail: Promise<void> = Promise.resolve();
  private initializationInFlight = false;
  private retryInitializationAfterReady = false;
  private started = false;
  private disposed = false;
  private readonly registerController: ControllerRegistrar;

  constructor(
    private readonly backend: BackendClient,
    private readonly output: OutputCoordinator,
    dependencies: RuntimeDependencies = {},
  ) {
    this.registerController = dependencies.registerController ?? registerControllerInput;
  }

  start(): void {
    if (this.started || this.disposed) return;
    this.started = true;
    this.removeCoreListener = this.backend.onCoreEvent((event) => this.onCoreEvent(event));
    this.removeBridgeListener = this.backend.onBridgeStatus((status) =>
      this.onBridgeStatus(status),
    );
    this.controller = this.registerController((event) => this.onControllerEvent(event));
    this.patchState({
      controllerReady: this.controller.ready,
      controllerError: this.controller.error,
    });
    this.beginInitialize();
  }

  getState = (): RuntimeState => this.state;

  subscribe = (listener: StateListener): (() => void) => {
    this.listeners.add(listener);
    return () => this.listeners.delete(listener);
  };

  visibleStatus(): VisibleStatus {
    if (!this.state.initialized && !this.state.error) return "Connecting";
    if (this.state.phase === "failed" || this.state.error || !this.state.controllerReady) {
      return "Failed";
    }
    switch (this.state.phase) {
      case "stopped":
        return "Stopped";
      case "setup_required":
        return "Setup required";
      case "ready":
        return "Ready";
      case "recording":
        return "Recording";
      case "transcribing":
        return "Transcribing";
    }
  }

  async setEnabled(enabled: boolean): Promise<void> {
    if (this.disposed) return;
    if (!enabled) {
      this.cancelLocalSession();
      this.patchState({ enabled: false, phase: "stopped", error: null });
      await this.serialize(async () => {
        // set_enabled(false) is the authoritative backend cancellation path:
        // it drops the session and stops the daemon in one transition. Calling
        // cancel_session first would unnecessarily restart voxtype only to stop
        // it immediately afterwards.
        await this.applySnapshotResponse(this.backend.setEnabled(false));
      });
      return;
    }

    this.patchState({ error: null, bridgeCode: null });
    await this.serialize(async () => {
      await this.applySnapshotResponse(this.backend.setEnabled(true));
    });
  }

  async updateSettings(partial: Partial<Settings>): Promise<void> {
    if (this.disposed) return;
    const previous = this.state.settings;
    const updated = normalizeSettings({ ...previous, ...partial });
    if (updated.output_mode !== previous.output_mode) {
      // Enabling auto-send is not effective until a persisted core snapshot
      // acknowledges it. Disabling it is effective immediately.
      this.autoSendAcknowledged = false;
    }
    const changedBindings = !bindingsEqual(previous, updated);
    let cancelledSessionId: number | null = null;
    if (changedBindings) cancelledSessionId = this.cancelLocalSession();
    this.ptt.configure({
      mode: updated.ptt_mode,
      primary: updated.controller_primary,
      secondary: updated.controller_secondary,
    });
    this.patchState({ settings: updated, error: null });

    await this.serialize(async () => {
      if (cancelledSessionId !== null) {
        await this.applyOptionalSnapshot(this.backend.cancelSession(cancelledSessionId));
      }
      await this.applySnapshotResponse(this.backend.updateSettings(updated));
    });
  }

  async installModel(): Promise<void> {
    if (this.disposed || this.state.modelDownloading) return;
    this.patchState({
      modelDownloading: true,
      modelProgressPercent: 0,
      error: null,
    });
    try {
      await this.backend.installModel(this.state.settings.model);
    } catch (error) {
      this.patchState({ modelDownloading: false, error: errorMessage(error) });
    }
  }

  async cancelModelInstall(): Promise<void> {
    if (this.disposed) return;
    try {
      await this.backend.cancelModel();
    } finally {
      this.patchState({ modelDownloading: false, modelProgressPercent: null });
    }
  }

  dispose(): void {
    if (this.disposed) return;
    this.disposed = true;
    const sessionId = this.cancelLocalSession();
    const controller = this.controller;
    this.controller = null;
    bestEffort(() => controller?.dispose());
    const removeCoreListener = this.removeCoreListener;
    this.removeCoreListener = null;
    bestEffort(() => removeCoreListener?.());
    const removeBridgeListener = this.removeBridgeListener;
    this.removeBridgeListener = null;
    bestEffort(() => removeBridgeListener?.());
    bestEffort(() => this.output.dispose());
    if (sessionId !== null) void this.bestEffortCancelBackend(sessionId);
    bestEffort(() => this.listeners.clear());
  }

  private async initialize(): Promise<void> {
    try {
      const hello = await this.backend.hello();
      if (hello.protocol_version !== PROTOCOL_VERSION) {
        throw new Error(
          `Protocol mismatch: frontend=${PROTOCOL_VERSION}, backend=${hello.protocol_version}`,
        );
      }
      if (this.disposed) return;
      this.currentInstanceId = hello.instance_id;
      this.lastEventSeq = -1;
      await this.applySnapshotResponse(this.backend.getSnapshot());
      this.patchState({ initialized: true, bridgeCode: null });
    } catch (error) {
      this.invalidateCore("PROTOCOL_OR_BACKEND_ERROR", errorMessage(error));
    }
  }

  private onControllerEvent(event: ControllerButtonEvent): void {
    if (this.disposed) return;
    const canStart =
      this.state.enabled &&
      this.state.phase === "ready" &&
      this.state.controllerReady &&
      this.currentInstanceId !== null;
    const action = this.ptt.handle(event, canStart);
    if (action) this.handlePttAction(action);
  }

  private handlePttAction(action: PttAction): void {
    if (action.type === "start") {
      if (
        !this.state.enabled ||
        this.state.phase !== "ready" ||
        this.currentInstanceId === null ||
        this.currentSession !== null
      ) {
        this.ptt.reset();
        return;
      }
      const session: ActiveSession = {
        id: this.nextSessionId++,
        instanceId: this.currentInstanceId,
        startOutputMode:
          this.state.settings.output_mode === "steam_input_send" &&
          !this.autoSendAcknowledged
            ? "steam_input"
            : this.state.settings.output_mode,
        outputConsumed: false,
        cancelled: false,
      };
      this.currentSession = session;
      this.patchState({ phase: "recording", lastOutcome: null, error: null });
      void this.serialize(async () => {
        try {
          await this.applySnapshotResponse(this.backend.recordStart(session.id));
        } catch (error) {
          if (this.currentSession === session) {
            this.cancelLocalSession();
            this.patchState({ phase: "failed" });
          }
          throw error;
        }
      }).catch(() => {});
      return;
    }

    const session = this.currentSession;
    if (!session) return;
    this.patchState({ phase: "transcribing" });
    void this.serialize(async () => {
      try {
        await this.applySnapshotResponse(this.backend.recordStop(session.id));
      } catch (error) {
        if (this.currentSession === session) {
          this.cancelLocalSession();
          this.patchState({ phase: "failed" });
        }
        await this.bestEffortCancelBackend(session.id);
        throw error;
      }
    }).catch(() => {});
  }

  private onCoreEvent(event: CoreEvent): void {
    if (
      this.disposed ||
      event.v !== PROTOCOL_VERSION ||
      event.instance_id !== this.currentInstanceId ||
      !Number.isSafeInteger(event.seq) ||
      event.seq <= this.lastEventSeq
    ) {
      return;
    }
    this.lastEventSeq = event.seq;

    switch (event.name) {
      case "snapshot":
      case "status":
      case "phase": {
        try {
          this.applySnapshot(parseSnapshotResult(event.payload), event.seq);
        } catch (error) {
          this.invalidateCore(
            "INVALID_BACKEND_EVENT",
            `Invalid backend event: ${errorMessage(error)}`,
          );
        }
        break;
      }
      case "settings":
        this.applySettingsEvent(event.payload);
        break;
      case "output": {
        const payload = parseOutputPayload(event.payload);
        if (payload) void this.handleOutput(payload, event.instance_id);
        break;
      }
      case "model_progress":
        this.applyModelProgress(event.payload);
        break;
      case "error":
        this.applyErrorEvent(event.payload);
        break;
      default:
        // Forward-compatible: unknown version-1 events do not change safety state.
        break;
    }
  }

  private onBridgeStatus(status: BridgeStatus): void {
    if (status.status === "ready" || status.status === "ok") {
      if (this.currentInstanceId === null) {
        if (this.initializationInFlight) {
          this.retryInitializationAfterReady = true;
        } else {
          this.beginInitialize();
        }
      }
      return;
    }
    this.retryInitializationAfterReady = false;
    this.invalidateCore(status.code, status.message ?? status.status);
  }

  private beginInitialize(): void {
    if (this.disposed || this.currentInstanceId !== null || this.initializationInFlight) {
      return;
    }
    this.initializationInFlight = true;
    void this.initialize().finally(() => {
      this.initializationInFlight = false;
      const retry = this.retryInitializationAfterReady;
      this.retryInitializationAfterReady = false;
      if (retry && !this.disposed && this.currentInstanceId === null) {
        this.beginInitialize();
      }
    });
  }

  private async handleOutput(payload: OutputEventPayload, instanceId: string): Promise<void> {
    const session = this.currentSession;
    if (session && payload.session_id === session.id && !payload.ok) {
      // A failure for the active session is authoritative even if a successful
      // payload already injected text and is waiting in the auto-send delay.
      // Invalidating the session here makes that delay fail closed.
      this.cancelLocalSession();
      this.patchState({
        lastOutcome:
          payload.error_code === "NO_SPEECH" ? "No speech recognized" : "Failed",
        error:
          payload.error_code === "NO_SPEECH"
            ? null
            : payload.error ?? "Transcription failed",
      });
      return;
    }
    const decision = decideTranscriptOutput({
      ok: payload.ok,
      rawText: payload.text,
      eventSessionId: payload.session_id,
      currentSessionId: session?.id ?? null,
      serviceEnabled: this.state.enabled,
      alreadyConsumed: session?.outputConsumed ?? false,
      sessionStartOutputMode: session?.startOutputMode ?? null,
      currentOutputMode: this.state.settings.output_mode,
      errorCode: payload.error_code,
    });

    if (decision.kind === "ignore") {
      if (session && payload.session_id === session.id && !session.outputConsumed) {
        session.outputConsumed = true;
        if (decision.reason === "empty_transcript" || decision.reason === "no_speech") {
          this.patchState({ lastOutcome: "No speech recognized", error: null });
          this.finishLocalSession(session);
        } else if (decision.reason === "transcription_failed") {
          this.patchState({ lastOutcome: "Failed", error: payload.error ?? "Transcription failed" });
          this.finishLocalSession(session);
        }
      }
      return;
    }

    if (!session) return;
    session.outputConsumed = true;
    const result = await this.output.execute(
      decision,
      this.state.settings.send_delay_ms,
      () =>
        !this.disposed &&
        this.state.enabled &&
        this.currentInstanceId === instanceId &&
        this.currentSession === session &&
        !session.cancelled &&
        session.startOutputMode === "steam_input_send" &&
        this.autoSendAcknowledged &&
        this.state.settings.output_mode === "steam_input_send" &&
        this.state.phase === "ready" &&
        this.state.error === null &&
        this.state.bridgeCode === null,
    );
    if (this.currentSession === session && !session.cancelled) {
      this.patchState({ lastOutcome: result.outcome, error: result.error ?? null });
      this.finishLocalSession(session);
    }
  }

  private applySettingsEvent(value: unknown): void {
    const settings = normalizeSettings(value);
    if (!bindingsEqual(this.state.settings, settings)) {
      const hadIntent = this.ptt.configure({
        mode: settings.ptt_mode,
        primary: settings.controller_primary,
        secondary: settings.controller_secondary,
      });
      if (hadIntent) {
        const sessionId = this.cancelLocalSession();
        if (sessionId !== null) void this.backend.cancelSession(sessionId).catch(() => {});
      }
    }
    this.autoSendAcknowledged = settings.output_mode === "steam_input_send";
    this.patchState({ settings });
  }

  private applyModelProgress(value: unknown): void {
    const payload = (asRecord(value) ?? {}) as ModelProgressPayload;
    const percent =
      typeof payload.percent === "number" && Number.isFinite(payload.percent)
        ? clampPercent(payload.percent)
        : bytesPercent(payload.downloaded_bytes, payload.total_bytes);
    const status = payload.status?.toLowerCase();
    this.patchState({
      modelDownloading: status !== "completed" && status !== "cancelled" && status !== "failed",
      modelProgressPercent: percent,
      error: status === "failed" ? "Model download failed" : this.state.error,
    });
  }

  private applyErrorEvent(value: unknown): void {
    const payload = asRecord(value);
    const message =
      typeof payload?.message === "string"
        ? payload.message
        : typeof payload?.error === "string"
          ? payload.error
          : "Backend operation failed";
    this.cancelActiveSessionAndBackend();
    this.patchState({ error: message, modelDownloading: false });
  }

  private async applySnapshotResponse(response: Promise<CoreSnapshot>): Promise<void> {
    this.applySnapshot(await response);
  }

  private async applyOptionalSnapshot(
    response: Promise<CoreSnapshot | null>,
  ): Promise<void> {
    const snapshot = await response;
    if (snapshot) this.applySnapshot(snapshot);
  }

  private applySnapshot(snapshot: CoreSnapshot, eventSeq?: number): void {
    if (snapshot.protocol_version !== PROTOCOL_VERSION) {
      throw new Error(`Protocol mismatch in snapshot: ${snapshot.protocol_version}`);
    }
    if (snapshot.instance_id !== this.currentInstanceId) return;
    const sequence = eventSeq ?? snapshot.seq;
    if (!Number.isSafeInteger(sequence) || sequence < this.lastEventSeq) return;
    this.lastEventSeq = Math.max(this.lastEventSeq, sequence);

    const settings = normalizeSettings(snapshot.settings);
    const previousPhase = this.state.phase;
    if (!bindingsEqual(this.state.settings, settings)) {
      const hadIntent = this.ptt.configure({
        mode: settings.ptt_mode,
        primary: settings.controller_primary,
        secondary: settings.controller_secondary,
      });
      if (hadIntent) {
        const sessionId = this.cancelLocalSession();
        if (sessionId !== null) void this.backend.cancelSession(sessionId).catch(() => {});
      }
    }
    const phase = normalizePhase(snapshot.phase);
    const snapshotError = snapshotErrorMessage(snapshot.error);
    this.autoSendAcknowledged = settings.output_mode === "steam_input_send";
    if (
      this.currentSession &&
      (snapshot.enabled !== true ||
        phase === "stopped" ||
        phase === "setup_required" ||
        phase === "failed" ||
        snapshotError !== null)
    ) {
      // Once an active session observes a backend terminal/error state it must
      // never become eligible for Enter again, even if a later snapshot is Ready.
      this.cancelLocalSession();
    }
    if (
      this.currentSession &&
      !this.currentSession.outputConsumed &&
      previousPhase !== "ready" &&
      (phase === "ready" ||
        phase === "stopped" ||
        phase === "setup_required" ||
        phase === "failed")
    ) {
      this.cancelLocalSession();
    }
    if (phase === "ready" && previousPhase !== "ready" && this.currentSession === null) {
      this.ptt.reset();
    }
    this.patchState({
      initialized: true,
      settings,
      phase,
      enabled: snapshot.enabled === true,
      modelInstalled: snapshot.model_installed === true,
      modelDownloading:
        snapshot.model_installed === true || phase === "failed"
          ? false
          : this.state.modelDownloading,
      modelProgressPercent:
        snapshot.model_installed === true ? 100 : this.state.modelProgressPercent,
      engineBackend:
        typeof snapshot.engine_backend === "string" ? snapshot.engine_backend : null,
      error: snapshotError,
      bridgeCode: null,
    });
  }

  private cancelLocalSession(): number | null {
    const session = this.currentSession;
    if (session) session.cancelled = true;
    this.currentSession = null;
    bestEffort(() => this.ptt.reset());
    bestEffort(() => this.output.releaseReturn());
    return session?.id ?? null;
  }

  private async bestEffortCancelBackend(sessionId: number): Promise<void> {
    try {
      await this.backend.cancelSession(sessionId);
    } catch {
      // Local invalidation already prevents output; backend cancellation is cleanup only.
    }
  }

  private cancelActiveSessionAndBackend(): void {
    const sessionId = this.cancelLocalSession();
    if (sessionId !== null) void this.bestEffortCancelBackend(sessionId);
  }

  private finishLocalSession(session: ActiveSession): void {
    if (this.currentSession !== session) return;
    this.currentSession = null;
    this.ptt.reset();
  }

  private invalidateCore(code: string, message: string): void {
    this.cancelActiveSessionAndBackend();
    this.currentInstanceId = null;
    this.lastEventSeq = -1;
    this.autoSendAcknowledged = false;
    this.patchState({
      initialized: true,
      phase: "failed",
      enabled: false,
      modelDownloading: false,
      error: message,
      bridgeCode: code,
    });
  }

  private serialize(task: () => Promise<void>): Promise<void> {
    const run = this.commandTail.then(task);
    this.commandTail = run.then(
      () => undefined,
      (error: unknown) => {
        if (!this.disposed) {
          this.cancelActiveSessionAndBackend();
          this.patchState({ error: errorMessage(error) });
        }
      },
    );
    return run;
  }

  private patchState(patch: Partial<RuntimeState>): void {
    if (this.disposed) return;
    this.state = { ...this.state, ...patch };
    for (const listener of this.listeners) listener();
  }
}

function parseOutputPayload(value: unknown): OutputEventPayload | null {
  const raw = asRecord(value);
  if (
    !raw ||
    !Number.isSafeInteger(raw.session_id) ||
    typeof raw.ok !== "boolean" ||
    typeof raw.text !== "string"
  ) {
    return null;
  }
  return raw as unknown as OutputEventPayload;
}

function normalizePhase(value: unknown): BackendPhase {
  switch (value) {
    case "stopped":
    case "setup_required":
    case "ready":
    case "recording":
    case "transcribing":
    case "failed":
      return value;
    default:
      return "failed";
  }
}

function bytesPercent(downloaded: unknown, total: unknown): number | null {
  if (
    typeof downloaded !== "number" ||
    typeof total !== "number" ||
    !Number.isFinite(downloaded) ||
    !Number.isFinite(total) ||
    total <= 0
  ) {
    return null;
  }
  return clampPercent((downloaded / total) * 100);
}

function clampPercent(value: number): number {
  return Math.min(100, Math.max(0, value));
}

function errorMessage(error: unknown): string {
  return error instanceof Error ? error.message : String(error);
}

function snapshotErrorMessage(error: CoreSnapshot["error"]): string | null {
  if (typeof error === "string") return error;
  if (error && typeof error.message === "string") {
    return error.code ? `${error.code}: ${error.message}` : error.message;
  }
  return null;
}

function bestEffort(action: () => void): void {
  try {
    action();
  } catch {
    // Cleanup steps are isolated so one broken Decky/Steam subscription cannot
    // prevent Return release or backend session cancellation.
  }
}
