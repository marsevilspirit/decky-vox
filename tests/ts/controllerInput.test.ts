import assert from "node:assert/strict";
import test from "node:test";

import { createSteamTextInput } from "../../src/runtime/controllerInput.ts";
import { OutputCoordinator } from "../../src/runtime/outputCoordinator.ts";

interface MutableSteamInput {
  ControllerKeyboardSendText?: (text: string) => void;
  ControllerKeyboardSetKeyState?: (key: number, pressed: boolean) => void;
}

interface MutableSteamGlobal {
  SteamClient?: { Input?: MutableSteamInput };
}

const steamGlobal = globalThis as typeof globalThis & MutableSteamGlobal;

test("disappearing SendText falls back to clipboard and never presses Return", async () => {
  const previous = steamGlobal.SteamClient;
  const input: MutableSteamInput = {
    ControllerKeyboardSendText() {},
    ControllerKeyboardSetKeyState() {},
  };
  steamGlobal.SteamClient = { Input: input };
  try {
    const steam = createSteamTextInput();
    delete input.ControllerKeyboardSendText;
    const copied: string[] = [];
    const keyCalls: boolean[] = [];
    input.ControllerKeyboardSetKeyState = (_key, pressed) => keyCalls.push(pressed);
    const coordinator = new OutputCoordinator(
      steam,
      {
        writeText: async (text) => {
          copied.push(text);
          return true;
        },
      },
      async () => {},
    );

    const result = await coordinator.execute(
      { kind: "steam_input", text: "只能复制", autoSend: true },
      250,
      () => true,
    );
    assert.deepEqual(copied, ["只能复制"]);
    assert.deepEqual(keyCalls, []);
    assert.equal(result.outcome, "Copied to clipboard");
  } finally {
    steamGlobal.SteamClient = previous;
  }
});

test("disappearing SetKeyState cannot report Sent or duplicate injected text", async () => {
  const previous = steamGlobal.SteamClient;
  const injected: string[] = [];
  const input: MutableSteamInput = {
    ControllerKeyboardSendText(text) {
      injected.push(text);
    },
    ControllerKeyboardSetKeyState() {},
  };
  steamGlobal.SteamClient = { Input: input };
  try {
    const steam = createSteamTextInput();
    delete input.ControllerKeyboardSetKeyState;
    let copied = false;
    const coordinator = new OutputCoordinator(
      steam,
      {
        writeText: async () => {
          copied = true;
          return true;
        },
      },
      async () => {},
    );

    const result = await coordinator.execute(
      { kind: "steam_input", text: "已经注入", autoSend: true },
      250,
      () => true,
    );
    assert.deepEqual(injected, ["已经注入"]);
    assert.equal(copied, false);
    assert.equal(result.outcome, "Input completed; auto-send failed");
    assert.notEqual(result.outcome, "Sent");
  } finally {
    steamGlobal.SteamClient = previous;
  }
});
