import type { OutputMode } from "./settings";

export interface TranscriptCandidate {
  ok: boolean;
  rawText: string;
  eventSessionId: number;
  currentSessionId: number | null;
  serviceEnabled: boolean;
  alreadyConsumed: boolean;
  sessionStartOutputMode: OutputMode | null;
  currentOutputMode: OutputMode;
  errorCode?: string | null;
}

export type IgnoredTranscriptReason =
  | "stale_session"
  | "service_disabled"
  | "already_consumed"
  | "no_speech"
  | "transcription_failed"
  | "empty_transcript";

export type OutputDecision =
  | { kind: "ignore"; reason: IgnoredTranscriptReason }
  | { kind: "clipboard"; text: string }
  | { kind: "steam_input"; text: string; autoSend: boolean };

export function decideTranscriptOutput(candidate: TranscriptCandidate): OutputDecision {
  if (
    candidate.currentSessionId === null ||
    candidate.eventSessionId !== candidate.currentSessionId
  ) {
    return { kind: "ignore", reason: "stale_session" };
  }
  if (!candidate.serviceEnabled) {
    return { kind: "ignore", reason: "service_disabled" };
  }
  if (candidate.alreadyConsumed) {
    return { kind: "ignore", reason: "already_consumed" };
  }
  if (!candidate.ok) {
    return {
      kind: "ignore",
      reason: candidate.errorCode === "NO_SPEECH" ? "no_speech" : "transcription_failed",
    };
  }

  const text = candidate.rawText.trim();
  if (text.length === 0) {
    return { kind: "ignore", reason: "empty_transcript" };
  }
  if (candidate.currentOutputMode === "clipboard") {
    return { kind: "clipboard", text };
  }
  return {
    kind: "steam_input",
    text,
    autoSend:
      candidate.sessionStartOutputMode === "steam_input_send" &&
      candidate.currentOutputMode === "steam_input_send",
  };
}
