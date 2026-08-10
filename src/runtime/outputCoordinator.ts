import type { OutputDecision } from "../domain/outputPolicy";

export const RETURN_HID_KEY = 40;
export const RETURN_HOLD_MS = 30;

export interface SteamTextInput {
  sendText?: (text: string) => void;
  setKeyState?: (key: number, pressed: boolean) => void;
}

export interface ClipboardWriter {
  writeText(text: string): Promise<boolean>;
}

export type OutputOutcome =
  | "Input completed"
  | "Input completed; auto-send failed"
  | "Sent"
  | "Copied to clipboard"
  | "No speech recognized"
  | "Failed";

export interface OutputResult {
  outcome: OutputOutcome;
  error?: string;
}

export type Delay = (milliseconds: number) => Promise<void>;

const defaultDelay: Delay = (milliseconds) =>
  new Promise((resolve) => globalThis.setTimeout(resolve, milliseconds));

export class OutputCoordinator {
  private disposed = false;
  private returnPressed = false;
  private readonly steam: SteamTextInput;
  private readonly clipboard: ClipboardWriter;
  private readonly delay: Delay;

  constructor(
    steam: SteamTextInput,
    clipboard: ClipboardWriter,
    delay: Delay = defaultDelay,
  ) {
    this.steam = steam;
    this.clipboard = clipboard;
    this.delay = delay;
  }

  async execute(
    decision: Exclude<OutputDecision, { kind: "ignore" }>,
    sendDelayMs: number,
    maySendReturn: () => boolean,
  ): Promise<OutputResult> {
    if (this.disposed) return { outcome: "Failed", error: "Output coordinator is stopped" };

    if (decision.kind === "clipboard") {
      return this.copy(decision.text, "Clipboard output failed");
    }

    if (typeof this.steam.sendText !== "function") {
      return this.copy(decision.text, "Steam text input is unavailable and clipboard failed");
    }
    try {
      this.steam.sendText(decision.text);
    } catch (error) {
      return this.copy(
        decision.text,
        `Steam text input failed and clipboard failed: ${errorMessage(error)}`,
      );
    }

    if (!decision.autoSend) return { outcome: "Input completed" };
    if (typeof this.steam.setKeyState !== "function") {
      return {
        outcome: "Input completed; auto-send failed",
        error: "Steam Return key API is unavailable",
      };
    }

    await this.delay(sendDelayMs);
    if (this.disposed || !maySendReturn()) return { outcome: "Input completed" };

    let keyError: string | undefined;
    try {
      // Mark first so even an API that throws after changing native state is
      // followed by a release attempt.
      this.returnPressed = true;
      this.steam.setKeyState(RETURN_HID_KEY, true);
      await this.delay(RETURN_HOLD_MS);
    } catch (error) {
      keyError = errorMessage(error);
    } finally {
      const releaseError = this.tryReleaseReturn(2);
      keyError ??= releaseError;
    }

    return keyError
      ? { outcome: "Input completed; auto-send failed", error: keyError }
      : { outcome: "Sent" };
  }

  /** Best-effort key release used by disable, cancellation, and plugin unload. */
  releaseReturn(): void {
    this.tryReleaseReturn(2);
  }

  dispose(): void {
    this.disposed = true;
    this.releaseReturn();
  }

  private async copy(text: string, failureMessage: string): Promise<OutputResult> {
    try {
      if (await this.clipboard.writeText(text)) return { outcome: "Copied to clipboard" };
    } catch (error) {
      return { outcome: "Failed", error: `${failureMessage}: ${errorMessage(error)}` };
    }
    return { outcome: "Failed", error: failureMessage };
  }

  private tryReleaseReturn(attempts: number): string | undefined {
    if (!this.returnPressed || typeof this.steam.setKeyState !== "function") return undefined;
    let lastError: string | undefined;
    for (let attempt = 0; attempt < attempts; attempt += 1) {
      try {
        this.steam.setKeyState(RETURN_HID_KEY, false);
        this.returnPressed = false;
        return undefined;
      } catch (error) {
        lastError = errorMessage(error);
      }
    }
    // Keep the latch set so disable/dispose can retry later.
    return lastError ?? "Steam Return release failed";
  }
}

export function createBrowserClipboardWriter(): ClipboardWriter {
  return {
    async writeText(text: string): Promise<boolean> {
      if (typeof document !== "undefined" && document.body) {
        const input = document.createElement("textarea");
        input.value = text;
        input.setAttribute("readonly", "true");
        input.style.position = "fixed";
        input.style.left = "-10000px";
        input.style.top = "-10000px";
        document.body.appendChild(input);
        try {
          input.focus();
          input.select();
          try {
            if (document.execCommand("copy")) return true;
          } catch {
            // Fall through to the async Clipboard API.
          }
        } finally {
          document.body.removeChild(input);
        }
      }

      try {
        if (typeof navigator !== "undefined" && navigator.clipboard?.writeText) {
          await navigator.clipboard.writeText(text);
          return true;
        }
      } catch {
        // Report a single accurate failure to the caller.
      }
      return false;
    },
  };
}

function errorMessage(error: unknown): string {
  return error instanceof Error ? error.message : String(error);
}
