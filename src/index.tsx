import { definePlugin, toaster } from "@decky/api";
import { staticClasses } from "@decky/ui";
import { FaMicrophone } from "react-icons/fa";

import { createBackendClient } from "./api/backend";
import { createSteamTextInput } from "./runtime/controllerInput";
import { DeckyVoxRuntime } from "./runtime/deckyVoxRuntime";
import {
  OutputCoordinator,
  createBrowserClipboardWriter,
} from "./runtime/outputCoordinator";
import { DeckyVoxPanel } from "./ui/DeckyVoxPanel";

export default definePlugin(() => {
  const output = new OutputCoordinator(
    createSteamTextInput(),
    createBrowserClipboardWriter(),
  );
  const runtime = new DeckyVoxRuntime(createBackendClient(), output);

  let previous = runtime.getState();
  const removeToastListener = runtime.subscribe(() => {
    const current = runtime.getState();
    if (current.phase === "recording" && previous.phase !== "recording") {
      toaster.toast({
        title: "Decky Vox",
        body: "Recording — release the PTT button when finished",
        duration: 1200,
      });
    }
    if (current.lastOutcome && current.lastOutcome !== previous.lastOutcome) {
      toaster.toast({
        title: "Decky Vox",
        body: current.lastOutcome,
        duration: 1800,
      });
    }
    previous = current;
  });

  // Runtime ownership is plugin-wide, so controller and output listeners keep
  // working while the quick-access panel's React tree is closed. This lifecycle
  // pattern is informed by mimed95/decky-voxtype (BSD-3-Clause).
  runtime.start();

  return {
    name: "Decky Vox",
    titleView: <div className={staticClasses.Title}>Decky Vox</div>,
    content: <DeckyVoxPanel runtime={runtime} />,
    icon: <FaMicrophone />,
    onDismount() {
      removeToastListener();
      runtime.dispose();
    },
  };
});
