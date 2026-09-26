/**
 * Browser-only stand-ins for the Rust commands, so `npm run dev` shows every screen without
 * Tauri. Each area registers its own file here; none of this runs inside the app.
 */
import { inTauri } from "../ipc";
import { registerAppMocks } from "./app";
import { registerCodeMocks } from "./code";
import { registerFlowsMocks } from "./flows";
import { registerIdeMocks } from "./ide";
import { registerModelsMocks } from "./models";
import { registerPdfMocks } from "./pdf";
import { registerRuntimeMocks } from "./runtime";
import { registerUpdateMocks } from "./update";
import { registerVideoMocks } from "./video";

export function installMocks(): void {
  if (inTauri) return;
  registerAppMocks();
  registerUpdateMocks();
  registerRuntimeMocks();
  registerCodeMocks();
  registerIdeMocks();
  registerVideoMocks();
  registerFlowsMocks();
  registerPdfMocks();
  registerModelsMocks();
}
