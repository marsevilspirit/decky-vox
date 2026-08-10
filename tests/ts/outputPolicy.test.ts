import assert from "node:assert/strict";
import test from "node:test";

import { decideTranscriptOutput } from "../../src/domain/outputPolicy.ts";

const base = {
  ok: true,
  rawText: "  你好，Steam Deck。  ",
  eventSessionId: 7,
  currentSessionId: 7,
  serviceEnabled: true,
  alreadyConsumed: false,
  sessionStartOutputMode: "steam_input" as const,
  currentOutputMode: "steam_input" as const,
};

test("empty successful transcription performs no output", () => {
  assert.deepEqual(decideTranscriptOutput({ ...base, rawText: " \n\t " }), {
    kind: "ignore",
    reason: "empty_transcript",
  });
});

test("backend NO_SPEECH failure is treated as empty, never as output", () => {
  assert.deepEqual(
    decideTranscriptOutput({ ...base, ok: false, rawText: "", errorCode: "NO_SPEECH" }),
    { kind: "ignore", reason: "no_speech" },
  );
});

test("NO_SPEECH with unexpected non-empty text still cannot output or auto-send", () => {
  assert.deepEqual(
    decideTranscriptOutput({
      ...base,
      ok: false,
      rawText: "这段错误载荷绝不能被输入或发送",
      errorCode: "NO_SPEECH",
      sessionStartOutputMode: "steam_input_send",
      currentOutputMode: "steam_input_send",
    }),
    { kind: "ignore", reason: "no_speech" },
  );
});

test("failed, stale, disabled, and duplicate results perform no output", () => {
  assert.equal(decideTranscriptOutput({ ...base, ok: false }).kind, "ignore");
  assert.equal(decideTranscriptOutput({ ...base, eventSessionId: 6 }).kind, "ignore");
  assert.equal(decideTranscriptOutput({ ...base, serviceEnabled: false }).kind, "ignore");
  assert.equal(decideTranscriptOutput({ ...base, alreadyConsumed: true }).kind, "ignore");
});

test("normal Steam input trims text and never auto-sends", () => {
  assert.deepEqual(decideTranscriptOutput(base), {
    kind: "steam_input",
    text: "你好，Steam Deck。",
    autoSend: false,
  });
});

test("auto-send requires both start-time and current explicit opt-in", () => {
  const bothOptedIn = {
    ...base,
    sessionStartOutputMode: "steam_input_send" as const,
    currentOutputMode: "steam_input_send" as const,
  };
  assert.equal(decideTranscriptOutput(bothOptedIn).kind, "steam_input");
  assert.equal(
    decideTranscriptOutput(bothOptedIn).kind === "steam_input" &&
      decideTranscriptOutput(bothOptedIn).autoSend,
    true,
  );
  assert.equal(
    decideTranscriptOutput({ ...bothOptedIn, currentOutputMode: "steam_input" }).kind ===
      "steam_input" &&
      decideTranscriptOutput({ ...bothOptedIn, currentOutputMode: "steam_input" }).autoSend,
    false,
  );
  assert.equal(
    decideTranscriptOutput({ ...bothOptedIn, sessionStartOutputMode: "steam_input" }).kind ===
      "steam_input" &&
      decideTranscriptOutput({ ...bothOptedIn, sessionStartOutputMode: "steam_input" })
        .autoSend,
    false,
  );
});

test("clipboard mode never asks for native input or Enter", () => {
  assert.deepEqual(
    decideTranscriptOutput({
      ...base,
      sessionStartOutputMode: "steam_input_send",
      currentOutputMode: "clipboard",
    }),
    { kind: "clipboard", text: "你好，Steam Deck。" },
  );
});
