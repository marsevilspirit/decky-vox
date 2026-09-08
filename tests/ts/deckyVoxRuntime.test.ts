import assert from "node:assert/strict";
import test from "node:test";

import type {
  BackendClient,
  BridgeStatus,
  CoreEvent,
  CoreSnapshot,
} from "../../src/api/backend.ts";
import { parseSnapshotResult } from "../../src/api/protocol.ts";
import { DEFAULT_SETTINGS } from "../../src/domain/settings.ts";
import type { ControllerButtonEvent } from "../../src/domain/pttMachine.ts";
import { DeckyVoxRuntime } from "../../src/runtime/deckyVoxRuntime.ts";
import { OutputCoordinator } from "../../src/runtime/outputCoordinator.ts";
import type { Delay } from "../../src/runtime/outputCoordinator.ts";

const INSTANCE_ID = "core-a";

function snapshot(
  phase: CoreSnapshot["phase"],
  enabled = true,
  overrides: Partial<CoreSnapshot> = {},
): CoreSnapshot {
  return {
    protocol_version: 1,
    instance_id: INSTANCE_ID,
    seq: 0,
    settings: { ...DEFAULT_SETTINGS },
    phase,
    enabled,
    model_installed: true,
    engine_backend: "fake",
    error: null,
    ...overrides,
  };
}

class FakeBackend implements BackendClient {
  readonly calls: string[] = [];
  settings: CoreSnapshot["settings"] = { ...DEFAULT_SETTINGS };
  coreListener: ((event: CoreEvent) => void) | null = null;
  bridgeListener: ((status: BridgeStatus) => void) | null = null;
  deferredStart: Promise<CoreSnapshot> | null = null;
  failUpdateSettings = false;
  failRecordStop = false;
  helloFailures = 0;
  throwOnRemove = false;
  coreRemoveCalls = 0;
  bridgeRemoveCalls = 0;

  async hello() {
    this.calls.push("hello");
    if (this.helloFailures > 0) {
      this.helloFailures -= 1;
      throw new Error("backend is still starting");
    }
    return { protocol_version: 1, instance_id: INSTANCE_ID };
  }

  async getSnapshot() {
    this.calls.push("get_snapshot");
    return snapshot("ready", true, { settings: this.settings });
  }

  async updateSettings(settings: CoreSnapshot["settings"]) {
    this.calls.push("update_settings");
    if (this.failUpdateSettings) throw new Error("settings update failed");
    this.settings = { ...settings };
    return snapshot("ready", true, { settings: this.settings });
  }

  async setEnabled(enabled: boolean) {
    this.calls.push(`set_enabled:${enabled}`);
    return snapshot(enabled ? "ready" : "stopped", enabled, { settings: this.settings });
  }

  async recordStart(sessionId: number) {
    this.calls.push(`record_start:${sessionId}`);
    return this.deferredStart ?? snapshot("recording", true, { settings: this.settings });
  }

  async recordStop(sessionId: number) {
    this.calls.push(`record_stop:${sessionId}`);
    if (this.failRecordStop) throw new Error("record stop failed");
    return snapshot("transcribing", true, { settings: this.settings });
  }

  async cancelSession(sessionId: number | null) {
    this.calls.push(`cancel_session:${sessionId}`);
    return snapshot("ready", true, { settings: this.settings });
  }

  async installModel(_model: string) {}

  async cancelModel() {}

  onCoreEvent(listener: (event: CoreEvent) => void) {
    this.coreListener = listener;
    return () => {
      this.coreRemoveCalls += 1;
      if (this.throwOnRemove) throw new Error("core unsubscribe failed");
      this.coreListener = null;
    };
  }

  onBridgeStatus(listener: (status: BridgeStatus) => void) {
    this.bridgeListener = listener;
    return () => {
      this.bridgeRemoveCalls += 1;
      if (this.throwOnRemove) throw new Error("bridge unsubscribe failed");
      this.bridgeListener = null;
    };
  }

  emitCore(event: CoreEvent): void {
    this.coreListener?.(event);
  }

  emitBridge(status: BridgeStatus): void {
    this.bridgeListener?.(status);
  }
}

interface OutputTracker {
  coordinator: OutputCoordinator;
  texts: string[];
  copied: string[];
  keys: boolean[];
}

function createOutputTracker(delay?: Delay): OutputTracker {
  const texts: string[] = [];
  const copied: string[] = [];
  const keys: boolean[] = [];
  return {
    texts,
    copied,
    keys,
    coordinator: new OutputCoordinator(
      {
        sendText: (text) => texts.push(text),
        setKeyState: (_key, pressed) => keys.push(pressed),
      },
      {
        writeText: async (text) => {
          copied.push(text);
          return true;
        },
      },
      delay ?? (async () => {}),
    ),
  };
}

interface RuntimeHarness {
  runtime: DeckyVoxRuntime;
  press(event: ControllerButtonEvent): void;
  controllerDisposeCalls(): number;
}

function createRuntime(
  backend: FakeBackend,
  output = createOutputTracker().coordinator,
  controllerDisposeThrows = false,
): RuntimeHarness {
  let handler: ((event: ControllerButtonEvent) => void) | null = null;
  let disposeCalls = 0;
  const runtime = new DeckyVoxRuntime(backend, output, {
    registerController: (registeredHandler) => {
      handler = registeredHandler;
      return {
        ready: true,
        error: null,
        dispose() {
          disposeCalls += 1;
          if (controllerDisposeThrows) throw new Error("controller dispose failed");
        },
      };
    },
  });
  runtime.start();
  return {
    runtime,
    press(event) {
      if (!handler) throw new Error("controller handler was not registered");
      handler(event);
    },
    controllerDisposeCalls: () => disposeCalls,
  };
}

function r4(pressed: boolean): ControllerButtonEvent {
  return { controllerId: 0, button: "R4", pressed };
}

function outputEvent(
  instanceId: string,
  seq: number,
  sessionId: number,
  text = "绝不能输出",
): CoreEvent {
  return {
    v: 1,
    kind: "event",
    instance_id: instanceId,
    seq,
    name: "output",
    payload: { session_id: sessionId, ok: true, text },
  };
}

function snapshotEvent(
  seq: number,
  phase: CoreSnapshot["phase"],
  enabled: boolean,
  error: CoreSnapshot["error"] = null,
  settings: CoreSnapshot["settings"] = {
    ...DEFAULT_SETTINGS,
    output_mode: "steam_input_send",
  },
): CoreEvent {
  return {
    v: 1,
    kind: "event",
    instance_id: INSTANCE_ID,
    seq,
    name: "snapshot",
    payload: snapshot(phase, enabled, {
      seq,
      settings,
      error,
    }),
  };
}

function failedOutputEvent(seq: number, sessionId: number): CoreEvent {
  return {
    v: 1,
    kind: "event",
    instance_id: INSTANCE_ID,
    seq,
    name: "output",
    payload: {
      session_id: sessionId,
      ok: false,
      text: "",
      error: "transcription failed after output",
      error_code: "TRANSCRIPTION_FAILED",
    },
  };
}

interface ControlledSendDelay {
  delay: Delay;
  release(): void;
}

function controlledSendDelay(): ControlledSendDelay {
  let release: (() => void) | null = null;
  const pending = new Promise<void>((resolve) => {
    release = resolve;
  });
  return {
    delay: async (milliseconds) => {
      if (milliseconds === DEFAULT_SETTINGS.send_delay_ms) await pending;
    },
    release() {
      if (!release) throw new Error("send delay resolver was not initialized");
      release();
    },
  };
}

async function beginPendingAutoSend(
  backend: FakeBackend,
  output: OutputTracker,
): Promise<RuntimeHarness> {
  const harness = createRuntime(backend, output.coordinator);
  await waitFor(() => harness.runtime.getState().phase === "ready", "runtime did not initialize");
  await harness.runtime.updateSettings({ output_mode: "steam_input_send" });
  harness.press(r4(true));
  harness.press(r4(false));
  await waitFor(() => backend.calls.includes("record_stop:1"), "session did not stop");
  backend.emitCore(outputEvent(INSTANCE_ID, 1, 1, "待确认发送"));
  await waitFor(() => output.texts.length === 1, "text was not injected before the delay");
  assert.deepEqual(output.keys, []);
  return harness;
}

async function waitFor(predicate: () => boolean, message: string): Promise<void> {
  for (let attempt = 0; attempt < 100; attempt += 1) {
    if (predicate()) return;
    await new Promise((resolve) => setTimeout(resolve, 0));
  }
  throw new Error(message);
}

test("snapshot responses accept objects and reject non-object values", () => {
  const raw = snapshot("ready");
  assert.equal(parseSnapshotResult(raw), raw);
  for (const invalid of [null, undefined, [], 1, "snapshot"]) {
    assert.throws(() => parseSnapshotResult(invalid), /invalid snapshot/);
  }
});

test("snapshot alone updates bindings, cancels recording, and acknowledges auto-send", async () => {
  const backend = new FakeBackend();
  const output = createOutputTracker();
  const harness = createRuntime(backend, output.coordinator);
  await waitFor(() => harness.runtime.getState().phase === "ready", "runtime did not initialize");
  harness.press(r4(true));
  await waitFor(() => backend.calls.includes("record_start:1"), "session did not start");

  backend.settings = {
    ...DEFAULT_SETTINGS,
    controller_primary: "R5",
    output_mode: "steam_input_send",
  };
  backend.emitCore(snapshotEvent(1, "recording", true, null, backend.settings));
  assert.deepEqual(harness.runtime.getState().settings, backend.settings);
  assert.equal(backend.calls.includes("cancel_session:1"), true);
  backend.emitCore(outputEvent(INSTANCE_ID, 2, 1));
  backend.emitCore(snapshotEvent(3, "ready", true, null, backend.settings));

  harness.press(r4(false));
  harness.press(r4(true));
  harness.press({ ...r4(true), button: "R5" });
  harness.press({ ...r4(false), button: "R5" });
  await waitFor(() => backend.calls.includes("record_stop:2"), "new binding did not stop session");
  backend.emitCore(outputEvent(INSTANCE_ID, 4, 2, "新绑定"));
  backend.emitCore(snapshotEvent(5, "ready", true, null, backend.settings));
  await waitFor(() => output.keys.length === 2, "snapshot did not acknowledge auto-send");

  assert.deepEqual(output.texts, ["新绑定"]);
  assert.deepEqual(output.keys, [true, false]);
  assert.equal(harness.runtime.getState().lastOutcome, "Sent");
  harness.runtime.dispose();
});

test("Recording waits for microphone acknowledgement and preserves the pending session owner", async () => {
  const backend = new FakeBackend();
  let resolveStart!: (value: CoreSnapshot) => void;
  backend.deferredStart = new Promise((resolve) => {
    resolveStart = resolve;
  });
  const harness = createRuntime(backend);
  await waitFor(() => harness.runtime.getState().phase === "ready", "runtime did not initialize");

  harness.press(r4(true));
  assert.equal(harness.runtime.visibleStatus(), "Starting");
  await waitFor(() => backend.calls.includes("record_start:1"), "record_start was not called");
  assert.equal(harness.runtime.visibleStatus(), "Starting");
  assert.equal(harness.runtime.getState().phase, "ready");
  harness.press({ ...r4(true), controllerId: 1 });
  harness.press({ ...r4(false), controllerId: 1 });

  resolveStart(snapshot("recording"));
  await waitFor(() => harness.runtime.visibleStatus() === "Recording", "recording was not acknowledged");
  harness.press(r4(false));
  await waitFor(() => backend.calls.includes("record_stop:1"), "owner release did not stop recording");
  assert.deepEqual(
    backend.calls.filter((call) => call.startsWith("record_")),
    ["record_start:1", "record_stop:1"],
  );
  harness.runtime.dispose();
});

test("rapid hold press/release serializes record_start before record_stop", async () => {
  const backend = new FakeBackend();
  let resolveStart: ((value: CoreSnapshot) => void) | null = null;
  backend.deferredStart = new Promise((resolve) => {
    resolveStart = resolve;
  });
  const harness = createRuntime(backend);
  await waitFor(() => harness.runtime.getState().phase === "ready", "runtime did not initialize");

  harness.press(r4(true));
  harness.press(r4(false));
  await waitFor(() => backend.calls.includes("record_start:1"), "record_start was not called");
  assert.equal(backend.calls.includes("record_stop:1"), false);

  if (!resolveStart) throw new Error("missing deferred resolver");
  resolveStart(snapshot("recording"));
  await waitFor(() => backend.calls.includes("record_stop:1"), "record_stop was not called");
  assert.deepEqual(
    backend.calls.filter((call) => call.startsWith("record_")),
    ["record_start:1", "record_stop:1"],
  );
  harness.runtime.dispose();
});

test("old instance, duplicate sequence, and stale session outputs are rejected", async () => {
  const backend = new FakeBackend();
  const output = createOutputTracker();
  const harness = createRuntime(backend, output.coordinator);
  await waitFor(() => harness.runtime.getState().phase === "ready", "runtime did not initialize");
  harness.press(r4(true));
  harness.press(r4(false));
  await waitFor(() => backend.calls.includes("record_stop:1"), "session did not stop");

  backend.emitCore(outputEvent("old-core", 1, 1));
  backend.emitCore(outputEvent(INSTANCE_ID, 0, 1));
  backend.emitCore(outputEvent(INSTANCE_ID, 1, 99));
  await new Promise((resolve) => setTimeout(resolve, 0));
  assert.deepEqual(output.texts, []);
  assert.deepEqual(output.copied, []);
  assert.deepEqual(output.keys, []);
  harness.runtime.dispose();
});

test("bridge failure and disabling invalidate sessions before output", async () => {
  const bridgeBackend = new FakeBackend();
  const bridgeOutput = createOutputTracker();
  const bridgeHarness = createRuntime(bridgeBackend, bridgeOutput.coordinator);
  await waitFor(
    () => bridgeHarness.runtime.getState().phase === "ready",
    "bridge runtime did not initialize",
  );
  bridgeHarness.press(r4(true));
  bridgeHarness.press(r4(false));
  await waitFor(() => bridgeBackend.calls.includes("record_stop:1"), "session did not stop");
  bridgeBackend.emitBridge({
    bridge_instance: "bridge-a",
    status: "failed",
    code: "CORE_EXITED",
    message: "core exited",
  });
  bridgeBackend.emitCore(outputEvent(INSTANCE_ID, 1, 1));
  await new Promise((resolve) => setTimeout(resolve, 0));
  assert.deepEqual(bridgeOutput.texts, []);
  assert.deepEqual(bridgeOutput.keys, []);
  bridgeHarness.runtime.dispose();

  const disableBackend = new FakeBackend();
  const disableOutput = createOutputTracker();
  const disableHarness = createRuntime(disableBackend, disableOutput.coordinator);
  await waitFor(
    () => disableHarness.runtime.getState().phase === "ready",
    "disable runtime did not initialize",
  );
  disableHarness.press(r4(true));
  disableHarness.press(r4(false));
  await waitFor(() => disableBackend.calls.includes("record_stop:1"), "session did not stop");
  const disabling = disableHarness.runtime.setEnabled(false);
  disableBackend.emitCore(outputEvent(INSTANCE_ID, 1, 1));
  await disabling;
  assert.deepEqual(disableOutput.texts, []);
  assert.deepEqual(disableOutput.keys, []);
  assert.equal(disableBackend.calls.includes("cancel_session:1"), false);
  assert.equal(disableBackend.calls.includes("set_enabled:false"), true);
  disableHarness.runtime.dispose();
});

test("record_stop failure invalidates locally and best-effort cancels backend", async () => {
  const backend = new FakeBackend();
  backend.failRecordStop = true;
  const output = createOutputTracker();
  const harness = createRuntime(backend, output.coordinator);
  await waitFor(() => harness.runtime.getState().phase === "ready", "runtime did not initialize");
  harness.press(r4(true));
  harness.press(r4(false));
  await waitFor(
    () => backend.calls.includes("cancel_session:1"),
    "failed record_stop did not cancel backend",
  );
  assert.equal(harness.runtime.getState().phase, "failed");
  backend.emitCore(outputEvent(INSTANCE_ID, 1, 1));
  await new Promise((resolve) => setTimeout(resolve, 0));
  assert.deepEqual(output.texts, []);
  assert.deepEqual(output.keys, []);
  harness.runtime.dispose();
});

test("failed snapshot during auto-send delay permanently suppresses Return", async () => {
  const backend = new FakeBackend();
  const controlled = controlledSendDelay();
  const output = createOutputTracker(controlled.delay);
  const harness = await beginPendingAutoSend(backend, output);

  backend.emitCore(
    snapshotEvent(2, "failed", true, {
      code: "ENGINE_FAILED",
      message: "engine failed during send delay",
      recoverable: true,
    }),
  );
  controlled.release();
  await new Promise((resolve) => setTimeout(resolve, 0));

  assert.deepEqual(output.keys, []);
  assert.notEqual(harness.runtime.getState().lastOutcome, "Sent");
  harness.runtime.dispose();
});

test("Ready snapshot during auto-send delay permits exactly one Return pair", async () => {
  const backend = new FakeBackend();
  const controlled = controlledSendDelay();
  const output = createOutputTracker(controlled.delay);
  const harness = await beginPendingAutoSend(backend, output);

  backend.emitCore(snapshotEvent(2, "ready", true));
  controlled.release();
  await waitFor(() => output.keys.length === 2, "Return was not pressed and released");

  assert.deepEqual(output.keys, [true, false]);
  assert.equal(harness.runtime.getState().lastOutcome, "Sent");
  harness.runtime.dispose();
});

test("error event followed by Ready cannot revive a pending auto-send", async () => {
  const backend = new FakeBackend();
  const controlled = controlledSendDelay();
  const output = createOutputTracker(controlled.delay);
  const harness = await beginPendingAutoSend(backend, output);

  backend.emitCore({
    v: 1,
    kind: "event",
    instance_id: INSTANCE_ID,
    seq: 2,
    name: "error",
    payload: { code: "ENGINE_FAILED", message: "engine failed" },
  });
  backend.emitCore(snapshotEvent(3, "ready", true));
  await waitFor(
    () => backend.calls.includes("cancel_session:1"),
    "error event did not cancel the backend session",
  );
  controlled.release();
  await new Promise((resolve) => setTimeout(resolve, 0));

  assert.deepEqual(output.keys, []);
  harness.runtime.dispose();
});

test("malformed snapshot invalidates core and cancels pending auto-send", async () => {
  const backend = new FakeBackend();
  const controlled = controlledSendDelay();
  const output = createOutputTracker(controlled.delay);
  const harness = await beginPendingAutoSend(backend, output);

  backend.emitCore({
    v: 1,
    kind: "event",
    instance_id: INSTANCE_ID,
    seq: 2,
    name: "snapshot",
    payload: { malformed: true },
  });
  await waitFor(
    () => backend.calls.includes("cancel_session:1"),
    "malformed snapshot did not cancel the backend session",
  );
  backend.emitCore(snapshotEvent(3, "ready", true));
  controlled.release();
  await new Promise((resolve) => setTimeout(resolve, 0));

  assert.deepEqual(output.keys, []);
  assert.equal(harness.runtime.getState().phase, "failed");
  assert.equal(harness.runtime.getState().bridgeCode, "INVALID_BACKEND_EVENT");
  harness.runtime.dispose();
});

test("serialized RPC failure cancels pending auto-send before later Ready", async () => {
  const backend = new FakeBackend();
  const controlled = controlledSendDelay();
  const output = createOutputTracker(controlled.delay);
  const harness = await beginPendingAutoSend(backend, output);

  backend.failUpdateSettings = true;
  await assert.rejects(
    harness.runtime.updateSettings({ send_delay_ms: 300 }),
    /settings update failed/,
  );
  await waitFor(
    () => backend.calls.includes("cancel_session:1"),
    "failed RPC did not cancel the backend session",
  );
  backend.emitCore(snapshotEvent(2, "ready", true));
  controlled.release();
  await new Promise((resolve) => setTimeout(resolve, 0));

  assert.deepEqual(output.keys, []);
  harness.runtime.dispose();
});

test("disabled snapshot and later Ready cannot revive a pending auto-send", async () => {
  const backend = new FakeBackend();
  const controlled = controlledSendDelay();
  const output = createOutputTracker(controlled.delay);
  const harness = await beginPendingAutoSend(backend, output);

  backend.emitCore(snapshotEvent(2, "stopped", false));
  backend.emitCore(snapshotEvent(3, "ready", true));
  controlled.release();
  await new Promise((resolve) => setTimeout(resolve, 0));

  assert.deepEqual(output.keys, []);
  harness.runtime.dispose();
});

test("a matching failed output cancels an already pending auto-send", async () => {
  const backend = new FakeBackend();
  const controlled = controlledSendDelay();
  const output = createOutputTracker(controlled.delay);
  const harness = await beginPendingAutoSend(backend, output);

  backend.emitCore(failedOutputEvent(2, 1));
  assert.equal(harness.runtime.getState().lastOutcome, "Failed");
  controlled.release();
  await new Promise((resolve) => setTimeout(resolve, 0));

  assert.deepEqual(output.keys, []);
  assert.equal(harness.runtime.getState().lastOutcome, "Failed");
  harness.runtime.dispose();
});

test("a later bridge-ready status retries an initialization that raced startup", async () => {
  const backend = new FakeBackend();
  backend.helloFailures = 1;
  const harness = createRuntime(backend);
  await waitFor(
    () => harness.runtime.getState().phase === "failed",
    "startup race did not fail the first initialization",
  );

  backend.emitBridge({
    bridge_instance: "bridge-a",
    status: "ready",
    code: "READY",
  });
  await waitFor(
    () => harness.runtime.getState().phase === "ready",
    "bridge ready did not retry initialization",
  );
  assert.equal(backend.calls.filter((call) => call === "hello").length, 2);
  harness.runtime.dispose();
});

test("dispose continues after throwing controller and unsubscribe cleanup", async () => {
  class TrackingOutput extends OutputCoordinator {
    disposeCalls = 0;

    override dispose(): void {
      this.disposeCalls += 1;
      super.dispose();
    }
  }

  const backend = new FakeBackend();
  backend.throwOnRemove = true;
  const output = new TrackingOutput(
    { sendText: () => {}, setKeyState: () => {} },
    { writeText: async () => true },
    async () => {},
  );
  const harness = createRuntime(backend, output, true);
  await waitFor(() => harness.runtime.getState().phase === "ready", "runtime did not initialize");
  harness.press(r4(true));
  await waitFor(() => backend.calls.includes("record_start:1"), "session did not start");

  harness.runtime.dispose();
  await waitFor(
    () => backend.calls.includes("cancel_session:1"),
    "dispose did not cancel backend session",
  );
  assert.equal(harness.controllerDisposeCalls(), 1);
  assert.equal(backend.coreRemoveCalls, 1);
  assert.equal(backend.bridgeRemoveCalls, 1);
  assert.equal(output.disposeCalls, 1);
});
