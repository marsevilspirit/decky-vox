import type { ControllerButton, Settings } from "../domain/settings";
import type { ControllerButtonEvent } from "../domain/pttMachine";
import type { SteamTextInput } from "./outputCoordinator";

// Back-grip identifiers and the RegisterForControllerInputMessages approach are
// informed by mimed95/decky-voxtype at cbb2201d (BSD-3-Clause). The complete
// upstream notice is shipped with Decky Vox.

interface ControllerInputSubscription {
  unregister(): void;
}

interface SteamInputApi {
  RegisterForControllerInputMessages?: (
    callback: (controllerId: number, button: number, pressed: boolean) => void,
  ) => ControllerInputSubscription;
  ControllerKeyboardSendText?: (text: string) => void;
  ControllerKeyboardSetKeyState?: (key: number, pressed: boolean) => void;
}

export interface ControllerInputRegistration {
  ready: boolean;
  error: string | null;
  dispose(): void;
}

// Values are Valve's gamepad button identifiers observed by Decky plugins.
export const BUTTON_TO_STEAM_ID: Readonly<Record<ControllerButton, number>> = {
  L4: 32,
  R4: 33,
  L5: 44,
  R5: 45,
};

const STEAM_ID_TO_BUTTON = new Map<number, ControllerButton>(
  Object.entries(BUTTON_TO_STEAM_ID).map(([button, id]) => [id, button as ControllerButton]),
);

function inputApi(): SteamInputApi | undefined {
  return (
    globalThis as typeof globalThis & {
      SteamClient?: { Input?: SteamInputApi };
    }
  ).SteamClient?.Input;
}

export function registerControllerInput(
  handler: (event: ControllerButtonEvent) => void,
): ControllerInputRegistration {
  const input = inputApi();
  const register = input?.RegisterForControllerInputMessages;
  if (typeof register !== "function") {
    return {
      ready: false,
      error: "Steam controller input API is unavailable",
      dispose() {},
    };
  }

  try {
    const subscription = register.call(
      input,
      (controllerId, buttonId, pressed) => {
        const button = STEAM_ID_TO_BUTTON.get(buttonId);
        if (button) handler({ controllerId, button, pressed });
      },
    );
    if (!subscription || typeof subscription.unregister !== "function") {
      return {
        ready: false,
        error: "Steam controller input subscription is invalid",
        dispose() {},
      };
    }
    return {
      ready: true,
      error: null,
      dispose: () => subscription.unregister(),
    };
  } catch (error) {
    return {
      ready: false,
      error: `Steam controller input registration failed: ${errorMessage(error)}`,
      dispose() {},
    };
  }
}

export function createSteamTextInput(): SteamTextInput {
  const initialInput = inputApi();
  return {
    sendText:
      typeof initialInput?.ControllerKeyboardSendText === "function"
        ? (text) => {
            const currentInput = inputApi();
            const sendText = currentInput?.ControllerKeyboardSendText;
            if (typeof sendText !== "function") {
              throw new Error("Steam text input API disappeared");
            }
            sendText.call(currentInput, text);
          }
        : undefined,
    setKeyState:
      typeof initialInput?.ControllerKeyboardSetKeyState === "function"
        ? (key, pressed) => {
            const currentInput = inputApi();
            const setKeyState = currentInput?.ControllerKeyboardSetKeyState;
            if (typeof setKeyState !== "function") {
              throw new Error("Steam Return key API disappeared");
            }
            setKeyState.call(currentInput, key, pressed);
          }
        : undefined,
  };
}

export function buttonLabel(button: ControllerButton | null): string {
  if (button === null) return "None (single button)";
  const labels: Record<ControllerButton, string> = {
    R4: "R4 (back grip)",
    L4: "L4 (back grip)",
    R5: "R5 (back grip)",
    L5: "L5 (back grip)",
  };
  return labels[button];
}

export function settingsUseButton(settings: Settings, button: ControllerButton): boolean {
  return (
    settings.controller_primary === button || settings.controller_secondary === button
  );
}

function errorMessage(error: unknown): string {
  return error instanceof Error ? error.message : String(error);
}
