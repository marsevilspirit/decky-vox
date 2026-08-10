import assert from "node:assert/strict";
import test from "node:test";

import { PttMachine } from "../../src/domain/pttMachine.ts";

test("hold single button deduplicates down/up and stops on release", () => {
  const machine = new PttMachine({ mode: "hold", primary: "R4", secondary: null });

  assert.deepEqual(
    machine.handle({ controllerId: 0, button: "R4", pressed: true }, true),
    { type: "start", controllerId: 0 },
  );
  assert.equal(machine.handle({ controllerId: 0, button: "R4", pressed: true }, true), null);
  assert.deepEqual(
    machine.handle({ controllerId: 0, button: "R4", pressed: false }, false),
    { type: "stop", controllerId: 0 },
  );
  assert.equal(machine.handle({ controllerId: 0, button: "R4", pressed: false }, false), null);
});

test("hold chord starts once and releasing either member stops once", () => {
  for (const released of ["R4", "L4"] as const) {
    const machine = new PttMachine({ mode: "hold", primary: "R4", secondary: "L4" });
    assert.equal(machine.handle({ controllerId: 2, button: "R4", pressed: true }, true), null);
    assert.deepEqual(
      machine.handle({ controllerId: 2, button: "L4", pressed: true }, true),
      { type: "start", controllerId: 2 },
    );
    assert.equal(machine.handle({ controllerId: 2, button: "L4", pressed: true }, true), null);
    assert.deepEqual(
      machine.handle({ controllerId: 2, button: released, pressed: false }, false),
      { type: "stop", controllerId: 2 },
    );
    assert.equal(machine.handle({ controllerId: 2, button: released, pressed: false }, false), null);
  }
});

test("toggle release only rearms; the next complete press stops", () => {
  const machine = new PttMachine({ mode: "toggle", primary: "R4", secondary: "L4" });
  assert.equal(machine.handle({ controllerId: 0, button: "R4", pressed: true }, true), null);
  assert.deepEqual(
    machine.handle({ controllerId: 0, button: "L4", pressed: true }, true),
    { type: "start", controllerId: 0 },
  );
  assert.equal(machine.handle({ controllerId: 0, button: "R4", pressed: false }, false), null);
  assert.equal(machine.handle({ controllerId: 0, button: "L4", pressed: false }, false), null);
  assert.equal(machine.handle({ controllerId: 0, button: "R4", pressed: true }, false), null);
  assert.deepEqual(
    machine.handle({ controllerId: 0, button: "L4", pressed: true }, false),
    { type: "stop", controllerId: 0 },
  );
});

test("buttons from separate controllers cannot form or stop a session", () => {
  const machine = new PttMachine({ mode: "hold", primary: "R4", secondary: "L4" });
  assert.equal(machine.handle({ controllerId: 0, button: "R4", pressed: true }, true), null);
  assert.equal(machine.handle({ controllerId: 1, button: "L4", pressed: true }, true), null);
  assert.deepEqual(
    machine.handle({ controllerId: 0, button: "L4", pressed: true }, true),
    { type: "start", controllerId: 0 },
  );
  assert.equal(machine.handle({ controllerId: 1, button: "L4", pressed: false }, false), null);
  assert.deepEqual(
    machine.handle({ controllerId: 0, button: "R4", pressed: false }, false),
    { type: "stop", controllerId: 0 },
  );
});

test("changing bindings clears held state and recording intent", () => {
  const machine = new PttMachine({ mode: "hold", primary: "R4", secondary: null });
  machine.handle({ controllerId: 0, button: "R4", pressed: true }, true);
  assert.equal(
    machine.configure({ mode: "hold", primary: "R5", secondary: null }),
    true,
  );
  assert.equal(machine.isRecordingIntentActive(), false);
  assert.equal(machine.handle({ controllerId: 0, button: "R4", pressed: false }, false), null);
});
