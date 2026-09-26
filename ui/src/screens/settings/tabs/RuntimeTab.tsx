/**
 * The Runtime page of Settings (RuntimeSettingsView.kt; the view is in ../runtime). It fills the
 * page area under the "Runtime" title, full width, and scrolls itself. The worker's web access
 * (WebAccessSetting.kt) sits under Models › Workers, where the 0.4.2 app had it.
 */
import { RuntimeSettingsView } from "../runtime/RuntimeSettingsView";

export interface RuntimeTabProps {
  /** The part of the deep link after "runtime/" (none is used by the 0.4.2 app); undefined otherwise. */
  initialSection?: string;
}

export function RuntimeTab(_props: RuntimeTabProps) {
  return <RuntimeSettingsView />;
}
