import type { ControllerButton, PttMode } from "./settings";

export interface PttBindings {
  mode: PttMode;
  primary: ControllerButton;
  secondary: ControllerButton | null;
}

export interface ControllerButtonEvent {
  controllerId: number;
  button: ControllerButton;
  pressed: boolean;
}

export type PttAction =
  | { type: "start"; controllerId: number }
  | { type: "stop"; controllerId: number };

function sameBindings(left: PttBindings, right: PttBindings): boolean {
  return (
    left.mode === right.mode &&
    left.primary === right.primary &&
    left.secondary === right.secondary
  );
}

/** Pure, controller-scoped PTT state machine. */
export class PttMachine {
  private bindings: PttBindings;
  private readonly pressedByController = new Map<number, Set<ControllerButton>>();
  private ownerControllerId: number | null = null;
  private recordingIntent = false;

  constructor(bindings: PttBindings) {
    this.bindings = { ...bindings };
  }

  configure(bindings: PttBindings): boolean {
    if (sameBindings(this.bindings, bindings)) return false;
    const hadRecordingIntent = this.recordingIntent;
    this.bindings = { ...bindings };
    this.reset();
    return hadRecordingIntent;
  }

  reset(): void {
    this.pressedByController.clear();
    this.ownerControllerId = null;
    this.recordingIntent = false;
  }

  isRecordingIntentActive(): boolean {
    return this.recordingIntent;
  }

  handle(event: ControllerButtonEvent, canStart: boolean): PttAction | null {
    if (!this.isBound(event.button)) return null;

    const pressed = this.pressedFor(event.controllerId);
    const wasActive = this.bindingIsActive(pressed);
    if (event.pressed) {
      if (pressed.has(event.button)) return null;
      pressed.add(event.button);
    } else {
      if (!pressed.has(event.button)) return null;
      pressed.delete(event.button);
    }
    const isActive = this.bindingIsActive(pressed);

    if (this.bindings.mode === "toggle") {
      if (!wasActive && isActive && event.pressed) {
        if (this.recordingIntent) {
          if (event.controllerId !== this.ownerControllerId) return null;
          this.recordingIntent = false;
          const owner = this.ownerControllerId;
          this.ownerControllerId = null;
          return owner === null ? null : { type: "stop", controllerId: owner };
        }
        if (!canStart || this.ownerControllerId !== null) return null;
        this.ownerControllerId = event.controllerId;
        this.recordingIntent = true;
        return { type: "start", controllerId: event.controllerId };
      }
      return null;
    }

    if (!this.recordingIntent) {
      if (!canStart || wasActive || !isActive || !event.pressed) return null;
      this.ownerControllerId = event.controllerId;
      this.recordingIntent = true;
      return { type: "start", controllerId: event.controllerId };
    }

    if (
      event.controllerId === this.ownerControllerId &&
      !event.pressed &&
      this.isBound(event.button)
    ) {
      const owner = this.ownerControllerId;
      this.ownerControllerId = null;
      this.recordingIntent = false;
      return owner === null ? null : { type: "stop", controllerId: owner };
    }
    return null;
  }

  private pressedFor(controllerId: number): Set<ControllerButton> {
    let pressed = this.pressedByController.get(controllerId);
    if (!pressed) {
      pressed = new Set<ControllerButton>();
      this.pressedByController.set(controllerId, pressed);
    }
    return pressed;
  }

  private isBound(button: ControllerButton): boolean {
    return button === this.bindings.primary || button === this.bindings.secondary;
  }

  private bindingIsActive(pressed: ReadonlySet<ControllerButton>): boolean {
    return (
      pressed.has(this.bindings.primary) &&
      (this.bindings.secondary === null || pressed.has(this.bindings.secondary))
    );
  }
}
