/**
 * WebAccessSetting.kt: whether Nook Code's worker may search the web and read pages (web_search,
 * read_page), over this computer's own connection. Saved in web.json in the Nook home; the next
 * request uses it. A switch here where the Kotlin row had an On | Off dropdown (components/Toggle).
 */
import { useEffect, useState } from "react";
import { messageOf } from "../../../api/ipc";
import { webAccessEnabled, webAccessSet } from "../../../api/models";
import { Toggle } from "../../../components/Toggle";
import { SettingsItemRow } from "../components";
import { webAccessDescription } from "./workers";
import "./models.css";

export function WebAccessSetting() {
  // Null until read: nothing is shown rather than a guess.
  const [on, setOn] = useState<boolean | null>(null);
  const [saving, setSaving] = useState(false);
  const [message, setMessage] = useState<string | null>(null);

  useEffect(() => {
    let alive = true;
    webAccessEnabled()
      .then((v) => alive && setOn(v))
      .catch((e) => alive && setMessage(`Could not read the setting: ${messageOf(e)}`));
    return () => {
      alive = false;
    };
  }, []);

  const choose = (wanted: boolean) => {
    setSaving(true);
    webAccessSet(wanted)
      .then(() => {
        setMessage(null);
        setOn(wanted);
      })
      .catch((e) => setMessage(`Could not save: ${messageOf(e)}`))
      .finally(() => setSaving(false));
  };

  return (
    <>
      {on != null && (
        <SettingsItemRow title="Web access" description={webAccessDescription(on)}>
          <Toggle checked={on} onChange={choose} disabled={saving} label="Web access" />
        </SettingsItemRow>
      )}
      {message && <div className="caption nk-models__error">{message}</div>}
    </>
  );
}
