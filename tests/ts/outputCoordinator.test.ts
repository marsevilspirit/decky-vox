import assert from "node:assert/strict";
import test from "node:test";

import {
  OutputCoordinator,
  RETURN_HID_KEY,
  RETURN_HOLD_MS,
} from "../../src/runtime/outputCoordinator.ts";

test("steam input mode injects once and never presses Return", async () => {
  const texts: string[] = [];
  const keys: Array<[number, boolean]> = [];
  const coordinator = new OutputCoordinator(
    { sendText: (text) => texts.push(text), setKeyState: (key, state) => keys.push([key, state]) },
    { writeText: async () => true },
    async () => {},
  );

  const result = await coordinator.execute(
    { kind: "steam_input", text: "你好", autoSend: false },
    250,
    () => true,
  );
  assert.deepEqual(texts, ["你好"]);
  assert.deepEqual(keys, []);
  assert.equal(result.outcome, "Input completed");
});

test("explicit auto-send waits, presses Return 40, and always releases", async () => {
  const actions: string[] = [];
  const coordinator = new OutputCoordinator(
    {
      sendText: (text) => actions.push(`text:${text}`),
      setKeyState: (key, pressed) => actions.push(`key:${key}:${pressed}`),
    },
    { writeText: async () => false },
    async (ms) => {
      actions.push(`wait:${ms}`);
    },
  );

  const result = await coordinator.execute(
    { kind: "steam_input", text: "发送", autoSend: true },
    250,
    () => true,
  );
  assert.deepEqual(actions, [
    "text:发送",
    "wait:250",
    `key:${RETURN_HID_KEY}:true`,
    `wait:${RETURN_HOLD_MS}`,
    `key:${RETURN_HID_KEY}:false`,
  ]);
  assert.equal(result.outcome, "Sent");
});

test("native injection failure falls back to clipboard and never presses Return", async () => {
  const keys: boolean[] = [];
  const copied: string[] = [];
  const coordinator = new OutputCoordinator(
    {
      sendText: () => {
        throw new Error("no focused input");
      },
      setKeyState: (_key, pressed) => keys.push(pressed),
    },
    {
      writeText: async (text) => {
        copied.push(text);
        return true;
      },
    },
    async () => {},
  );

  const result = await coordinator.execute(
    { kind: "steam_input", text: "回退", autoSend: true },
    250,
    () => true,
  );
  assert.deepEqual(copied, ["回退"]);
  assert.deepEqual(keys, []);
  assert.equal(result.outcome, "Copied to clipboard");
});

test("cancellation during auto-send delay prevents Return", async () => {
  let allowed = true;
  const keys: boolean[] = [];
  const coordinator = new OutputCoordinator(
    { sendText: () => {}, setKeyState: (_key, pressed) => keys.push(pressed) },
    { writeText: async () => false },
    async () => {
      allowed = false;
    },
  );
  const result = await coordinator.execute(
    { kind: "steam_input", text: "不发送", autoSend: true },
    250,
    () => allowed,
  );
  assert.deepEqual(keys, []);
  assert.equal(result.outcome, "Input completed");
});

test("Return press failure still attempts release and never copies duplicate text", async () => {
  const keyStates: boolean[] = [];
  let copied = false;
  const coordinator = new OutputCoordinator(
    {
      sendText: () => {},
      setKeyState: (_key, pressed) => {
        keyStates.push(pressed);
        if (pressed) throw new Error("press failed");
      },
    },
    {
      writeText: async () => {
        copied = true;
        return true;
      },
    },
    async () => {},
  );
  const result = await coordinator.execute(
    { kind: "steam_input", text: "已经输入", autoSend: true },
    250,
    () => true,
  );
  assert.deepEqual(keyStates, [true, false]);
  assert.equal(copied, false);
  assert.equal(result.outcome, "Input completed; auto-send failed");
});

test("Return release is retried when the native API throws", async () => {
  const states: boolean[] = [];
  let failedRelease = false;
  const coordinator = new OutputCoordinator(
    {
      sendText: () => {},
      setKeyState: (_key, pressed) => {
        states.push(pressed);
        if (!pressed && !failedRelease) {
          failedRelease = true;
          throw new Error("transient release error");
        }
      },
    },
    { writeText: async () => false },
    async () => {},
  );
  const result = await coordinator.execute(
    { kind: "steam_input", text: "release", autoSend: true },
    250,
    () => true,
  );
  assert.deepEqual(states, [true, false, false]);
  assert.equal(result.outcome, "Sent");
});
