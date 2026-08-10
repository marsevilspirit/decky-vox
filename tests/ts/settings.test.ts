import assert from "node:assert/strict";
import test from "node:test";

import {
  DEFAULT_SETTINGS,
  normalizeSettings,
} from "../../src/domain/settings.ts";

test("settings normalization supplies the safe defaults", () => {
  assert.deepEqual(normalizeSettings(undefined), DEFAULT_SETTINGS);
});

test("settings normalization rejects unsafe or mistyped values", () => {
  const settings = normalizeSettings({
    schema_version: 99,
    model: "small.en",
    language: "zh",
    gpu_enabled: 1,
    ptt_mode: "anything",
    controller_primary: "invalid",
    controller_secondary: "R4",
    output_mode: "send_without_confirmation",
    send_delay_ms: "0",
    auto_start: "yes",
    unknown: "discard me",
  });

  assert.deepEqual(settings, DEFAULT_SETTINGS);
  assert.equal("unknown" in settings, false);
});

test("settings normalization clamps integer send delay and removes duplicate chord", () => {
  assert.equal(normalizeSettings({ send_delay_ms: 0 }).send_delay_ms, 100);
  assert.equal(normalizeSettings({ send_delay_ms: 9999 }).send_delay_ms, 5000);
  assert.equal(normalizeSettings({ send_delay_ms: 250.5 }).send_delay_ms, 250);
  assert.equal(
    normalizeSettings({ controller_primary: "R5", controller_secondary: "R5" })
      .controller_secondary,
    null,
  );
});

test("invalid output mode can never opt into auto-send", () => {
  const settings = normalizeSettings({ output_mode: "steam_input_send_typo" });
  assert.equal(settings.output_mode, "steam_input");
});
