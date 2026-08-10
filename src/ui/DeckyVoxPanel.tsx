import {
  ButtonItem,
  DropdownItem,
  PanelSection,
  PanelSectionRow,
  ToggleField,
} from "@decky/ui";
import { Fragment, useEffect, useState } from "react";

import {
  CONTROLLER_BUTTONS,
  MODEL_OPTIONS,
  OUTPUT_MODES,
  PTT_MODES,
} from "../domain/settings";
import type {
  ControllerButton,
  ModelName,
  OutputMode,
  PttMode,
} from "../domain/settings";
import { buttonLabel } from "../runtime/controllerInput";
import type { DeckyVoxRuntime, RuntimeState } from "../runtime/deckyVoxRuntime";

interface DeckyVoxPanelProps {
  runtime: DeckyVoxRuntime;
}

const MODEL_LABELS: Record<ModelName, string> = {
  tiny: "Tiny (multilingual)",
  base: "Base (multilingual)",
  small: "Small (multilingual, recommended)",
  medium: "Medium (multilingual)",
};

const OUTPUT_LABELS: Record<OutputMode, string> = {
  steam_input: "Steam input (safe default)",
  steam_input_send: "Steam input + auto-send",
  clipboard: "Clipboard only",
};

const PTT_LABELS: Record<PttMode, string> = {
  hold: "Hold to talk",
  toggle: "Press to start / press again to stop",
};

const DELAY_OPTIONS = [100, 250, 500, 750, 1000, 1500, 2000, 5000] as const;

const STATUS_COLORS: Record<string, string> = {
  Connecting: "#6c757d",
  Stopped: "#6c757d",
  "Setup required": "#ffc107",
  Ready: "#28a745",
  Recording: "#dc3545",
  Transcribing: "#ffc107",
  Failed: "#dc3545",
};

function useRuntimeState(runtime: DeckyVoxRuntime): RuntimeState {
  const [state, setState] = useState<RuntimeState>(runtime.getState());
  useEffect(() => runtime.subscribe(() => setState(runtime.getState())), [runtime]);
  return state;
}

export function DeckyVoxPanel({ runtime }: DeckyVoxPanelProps) {
  const state = useRuntimeState(runtime);
  const [showSettings, setShowSettings] = useState(false);
  const [confirmAutoSend, setConfirmAutoSend] = useState(false);
  const visibleStatus = runtime.visibleStatus();
  const statusColor = STATUS_COLORS[visibleStatus] ?? STATUS_COLORS.Failed;

  const run = (operation: Promise<void>) => {
    void operation.catch(() => {
      // Runtime state carries the user-visible backend error.
    });
  };

  const chooseOutputMode = (mode: OutputMode) => {
    if (mode === "steam_input_send" && state.settings.output_mode !== mode) {
      setConfirmAutoSend(true);
      return;
    }
    setConfirmAutoSend(false);
    run(runtime.updateSettings({ output_mode: mode }));
  };

  return (
    <Fragment>
      <style>{`
        @keyframes decky-vox-pulse {
          0%, 100% { opacity: 1; }
          50% { opacity: 0.35; }
        }
      `}</style>

      <PanelSection title="使用前必读">
        <PanelSectionRow>
          <div style={{ color: "#ffc107", fontSize: "13px", lineHeight: 1.4 }}>
            开始录音前，请先让目标聊天输入框获得焦点。Decky Vox v1 不会自动寻找、点击或选择聊天窗口。
          </div>
        </PanelSectionRow>
      </PanelSection>

      <PanelSection title="Status">
        <PanelSectionRow>
          <div style={{ display: "flex", alignItems: "center", gap: "8px" }}>
            <span
              style={{
                display: "inline-block",
                width: "10px",
                height: "10px",
                borderRadius: "50%",
                backgroundColor: statusColor,
                animation:
                  visibleStatus === "Recording"
                    ? "decky-vox-pulse 1.2s ease-in-out infinite"
                    : "none",
              }}
            />
            <span>{visibleStatus}</span>
            {state.engineBackend && <span style={{ opacity: 0.7 }}>({state.engineBackend})</span>}
          </div>
        </PanelSectionRow>
        <PanelSectionRow>
          <ToggleField
            label="Enable Decky Vox"
            checked={state.enabled}
            disabled={!state.initialized}
            onChange={(enabled) => run(runtime.setEnabled(enabled))}
          />
        </PanelSectionRow>
        {state.lastOutcome && (
          <PanelSectionRow>
            <div style={{ fontSize: "13px" }}>Last result: {state.lastOutcome}</div>
          </PanelSectionRow>
        )}
        {state.controllerError && (
          <PanelSectionRow>
            <div style={{ color: "#dc3545", fontSize: "12px" }}>
              Controller: {state.controllerError}
            </div>
          </PanelSectionRow>
        )}
        {state.error && (
          <PanelSectionRow>
            <div style={{ color: "#dc3545", fontSize: "12px", overflowWrap: "anywhere" }}>
              {state.bridgeCode ? `${state.bridgeCode}: ` : ""}
              {state.error}
            </div>
          </PanelSectionRow>
        )}
      </PanelSection>

      {!state.modelInstalled && (
        <PanelSection title="Setup required">
          <PanelSectionRow>
            <div style={{ color: "#ffc107", fontSize: "13px", lineHeight: 1.4 }}>
              首次使用需主动下载 {MODEL_LABELS[state.settings.model]} 模型。下载完成后，日常转写完全在本地运行。
            </div>
          </PanelSectionRow>
          {state.modelDownloading && (
            <PanelSectionRow>
              <div style={{ fontSize: "13px" }}>
                Downloading model
                {state.modelProgressPercent === null
                  ? "…"
                  : `: ${state.modelProgressPercent.toFixed(0)}%`}
              </div>
            </PanelSectionRow>
          )}
          <PanelSectionRow>
            {state.modelDownloading ? (
              <ButtonItem layout="below" onClick={() => run(runtime.cancelModelInstall())}>
                Cancel model download
              </ButtonItem>
            ) : (
              <ButtonItem layout="below" onClick={() => run(runtime.installModel())}>
                Download {state.settings.model} model
              </ButtonItem>
            )}
          </PanelSectionRow>
        </PanelSection>
      )}

      <PanelSection>
        <PanelSectionRow>
          <ButtonItem layout="below" onClick={() => setShowSettings((shown) => !shown)}>
            {showSettings ? "Hide Settings" : "Show Settings"}
          </ButtonItem>
        </PanelSectionRow>
      </PanelSection>

      {showSettings && (
        <PanelSection title="Settings">
          <PanelSectionRow>
            <DropdownItem
              label="Model"
              description="Multilingual; language detection is fixed to auto in v1."
              rgOptions={MODEL_OPTIONS.map((model) => ({
                data: model,
                label: MODEL_LABELS[model],
              }))}
              selectedOption={state.settings.model}
              onChange={(option) =>
                run(runtime.updateSettings({ model: option.data as ModelName }))
              }
            />
          </PanelSectionRow>
          <PanelSectionRow>
            <ToggleField
              label="Vulkan GPU acceleration"
              description="Recommended. The backend may report a CPU fallback if Vulkan cannot start."
              checked={state.settings.gpu_enabled}
              onChange={(gpu_enabled) => run(runtime.updateSettings({ gpu_enabled }))}
            />
          </PanelSectionRow>
          <PanelSectionRow>
            <DropdownItem
              label="PTT mode"
              rgOptions={PTT_MODES.map((mode) => ({ data: mode, label: PTT_LABELS[mode] }))}
              selectedOption={state.settings.ptt_mode}
              onChange={(option) =>
                run(runtime.updateSettings({ ptt_mode: option.data as PttMode }))
              }
            />
          </PanelSectionRow>
          <PanelSectionRow>
            <DropdownItem
              label="Primary button"
              rgOptions={CONTROLLER_BUTTONS.map((button) => ({
                data: button,
                label: buttonLabel(button),
              }))}
              selectedOption={state.settings.controller_primary}
              onChange={(option) =>
                run(
                  runtime.updateSettings({
                    controller_primary: option.data as ControllerButton,
                  }),
                )
              }
            />
          </PanelSectionRow>
          <PanelSectionRow>
            <DropdownItem
              label="Optional chord button"
              rgOptions={[
                { data: "none", label: buttonLabel(null) },
                ...CONTROLLER_BUTTONS.map((button) => ({
                  data: button,
                  label: buttonLabel(button),
                })),
              ]}
              selectedOption={state.settings.controller_secondary ?? "none"}
              onChange={(option) =>
                run(
                  runtime.updateSettings({
                    controller_secondary:
                      option.data === "none" ? null : (option.data as ControllerButton),
                  }),
                )
              }
            />
          </PanelSectionRow>
          <PanelSectionRow>
            <DropdownItem
              label="Output mode"
              rgOptions={OUTPUT_MODES.map((mode) => ({
                data: mode,
                label: OUTPUT_LABELS[mode],
              }))}
              selectedOption={state.settings.output_mode}
              onChange={(option) => chooseOutputMode(option.data as OutputMode)}
            />
          </PanelSectionRow>

          {confirmAutoSend && (
            <Fragment>
              <PanelSectionRow>
                <div style={{ color: "#dc3545", fontSize: "13px", lineHeight: 1.4 }}>
                  自动发送有误发风险：如果焦点不在预期聊天框，文字和 Enter 可能进入其他位置。只有理解风险后才启用。
                </div>
              </PanelSectionRow>
              <PanelSectionRow>
                <ButtonItem
                  layout="below"
                  onClick={() => {
                    setConfirmAutoSend(false);
                    run(runtime.updateSettings({ output_mode: "steam_input_send" }));
                  }}
                >
                  I understand — enable auto-send
                </ButtonItem>
              </PanelSectionRow>
              <PanelSectionRow>
                <ButtonItem layout="below" onClick={() => setConfirmAutoSend(false)}>
                  Cancel
                </ButtonItem>
              </PanelSectionRow>
            </Fragment>
          )}

          {state.settings.output_mode === "steam_input_send" && (
            <PanelSectionRow>
              <div style={{ color: "#dc3545", fontSize: "12px", lineHeight: 1.4 }}>
                Auto-send enabled: a wrong focus can send text to an unintended target.
              </div>
            </PanelSectionRow>
          )}

          <PanelSectionRow>
            <DropdownItem
              label="Auto-send delay"
              description="Delay before Return; only used in the explicitly enabled auto-send mode."
              rgOptions={DELAY_OPTIONS.map((delay) => ({
                data: delay,
                label: `${delay} ms${delay === 250 ? " (default)" : ""}`,
              }))}
              selectedOption={state.settings.send_delay_ms}
              onChange={(option) =>
                run(runtime.updateSettings({ send_delay_ms: option.data as number }))
              }
            />
          </PanelSectionRow>
          <PanelSectionRow>
            <ToggleField
              label="Auto-start on boot"
              checked={state.settings.auto_start}
              onChange={(auto_start) => run(runtime.updateSettings({ auto_start }))}
            />
          </PanelSectionRow>
        </PanelSection>
      )}
    </Fragment>
  );
}
