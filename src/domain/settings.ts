export const SETTINGS_SCHEMA_VERSION = 1 as const;

export const MODEL_OPTIONS = ["tiny", "base", "small", "medium"] as const;
export type ModelName = (typeof MODEL_OPTIONS)[number];

export const PTT_MODES = ["hold", "toggle"] as const;
export type PttMode = (typeof PTT_MODES)[number];

export const OUTPUT_MODES = [
  "steam_input",
  "steam_input_send",
  "clipboard",
] as const;
export type OutputMode = (typeof OUTPUT_MODES)[number];

export const CONTROLLER_BUTTONS = [
  "R4",
  "L4",
  "R5",
  "L5",
] as const;
export type ControllerButton = (typeof CONTROLLER_BUTTONS)[number];

export interface Settings {
  schema_version: typeof SETTINGS_SCHEMA_VERSION;
  model: ModelName;
  language: "auto";
  gpu_enabled: boolean;
  ptt_mode: PttMode;
  controller_primary: ControllerButton;
  controller_secondary: ControllerButton | null;
  output_mode: OutputMode;
  send_delay_ms: number;
  auto_start: boolean;
}

export const DEFAULT_SETTINGS: Readonly<Settings> = Object.freeze({
  schema_version: SETTINGS_SCHEMA_VERSION,
  model: "small",
  language: "auto",
  gpu_enabled: true,
  ptt_mode: "hold",
  controller_primary: "R4",
  controller_secondary: null,
  output_mode: "steam_input",
  send_delay_ms: 250,
  auto_start: true,
});

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

function enumValue<T extends string>(
  value: unknown,
  allowed: readonly T[],
  fallback: T,
): T {
  return typeof value === "string" && allowed.includes(value as T)
    ? (value as T)
    : fallback;
}

function strictBoolean(value: unknown, fallback: boolean): boolean {
  return typeof value === "boolean" ? value : fallback;
}

function normalizeDelay(value: unknown): number {
  if (typeof value !== "number" || !Number.isFinite(value) || !Number.isInteger(value)) {
    return DEFAULT_SETTINGS.send_delay_ms;
  }
  return Math.min(5000, Math.max(100, value));
}

/**
 * Fail-closed frontend normalization. Rust remains the persisted source of
 * truth; this protects the runtime while a malformed or older snapshot is on
 * the wire.
 */
export function normalizeSettings(input: unknown): Settings {
  const raw = isRecord(input) ? input : {};
  const primary = enumValue(
    raw.controller_primary,
    CONTROLLER_BUTTONS,
    DEFAULT_SETTINGS.controller_primary,
  );
  const secondaryCandidate =
    typeof raw.controller_secondary === "string" &&
    CONTROLLER_BUTTONS.includes(raw.controller_secondary as ControllerButton)
      ? (raw.controller_secondary as ControllerButton)
      : null;
  const secondary =
    secondaryCandidate && secondaryCandidate !== primary ? secondaryCandidate : null;

  return {
    schema_version: SETTINGS_SCHEMA_VERSION,
    model: enumValue(raw.model, MODEL_OPTIONS, DEFAULT_SETTINGS.model),
    language: "auto",
    gpu_enabled: strictBoolean(raw.gpu_enabled, DEFAULT_SETTINGS.gpu_enabled),
    ptt_mode: enumValue(raw.ptt_mode, PTT_MODES, DEFAULT_SETTINGS.ptt_mode),
    controller_primary: primary,
    controller_secondary: secondary,
    output_mode: enumValue(raw.output_mode, OUTPUT_MODES, DEFAULT_SETTINGS.output_mode),
    send_delay_ms: normalizeDelay(raw.send_delay_ms),
    auto_start: strictBoolean(raw.auto_start, DEFAULT_SETTINGS.auto_start),
  };
}

export function bindingsEqual(left: Settings, right: Settings): boolean {
  return (
    left.ptt_mode === right.ptt_mode &&
    left.controller_primary === right.controller_primary &&
    left.controller_secondary === right.controller_secondary
  );
}
