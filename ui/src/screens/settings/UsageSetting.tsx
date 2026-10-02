/**
 * Settings › General › Privacy: the switch for the daily usage report (nook_core::usage), and the
 * next report exactly as it would be sent, so anyone can check there is nothing personal in it.
 */
import { useCallback, useEffect, useState } from "react";
import { messageOf } from "../../api/ipc";
import { usageDescription, usageOverview, usageSentText, usageSet, type UsageOverview } from "../../api/usage";
import { Toggle } from "../../components/Toggle";
import { SettingsAction, SettingsItemRow, SettingsRowDivider } from "./components";

export function UsageSetting() {
  // Null until read: nothing is shown rather than a guess.
  const [overview, setOverview] = useState<UsageOverview | null>(null);
  const [saving, setSaving] = useState(false);
  const [message, setMessage] = useState<string | null>(null);
  const [showReport, setShowReport] = useState(false);

  const load = useCallback(
    () =>
      usageOverview()
        .then(setOverview)
        .catch((e) => setMessage(`Could not read the setting: ${messageOf(e)}`)),
    [],
  );
  useEffect(() => {
    load();
  }, [load]);

  const choose = (wanted: boolean) => {
    setSaving(true);
    usageSet(wanted)
      .then(() => {
        setMessage(null);
        return load();
      })
      .catch((e) => setMessage(`Could not save: ${messageOf(e)}`))
      .finally(() => setSaving(false));
  };

  return (
    <>
      {overview && (
        <>
          <SettingsItemRow title="Usage statistics" description={usageDescription(overview.enabled)}>
            <Toggle checked={overview.enabled} onChange={choose} disabled={saving} label="Usage statistics" />
          </SettingsItemRow>
          <SettingsRowDivider />
          <SettingsItemRow title="What is sent" description={usageSentText(overview)}>
            <SettingsAction text={showReport ? "Hide" : "Show"} onClick={() => setShowReport((s) => !s)} />
          </SettingsItemRow>
          {showReport && <pre className="nk-usage-report">{JSON.stringify(overview.next, null, 2)}</pre>}
        </>
      )}
      {message && <div className="caption nk-usage-error">{message}</div>}
    </>
  );
}
