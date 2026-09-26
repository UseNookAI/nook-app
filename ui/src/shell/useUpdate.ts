/**
 * The update state as the window uses it (the update half of SettingsViewModel.kt): the core's
 * [UpdateStatus] kept current from the "update" topic, and whether the update dialog shows.
 *
 * The dialog shows by itself when an offer arrives that has not been answered with "Later"
 * (the view model's `snapshotFlow { isUpdateAvailable }`), and on demand from the title strip's
 * "Update available" and Settings' "Update to ..." buttons.
 */
import { useCallback, useEffect, useMemo, useState } from "react";
import { messageOf } from "../api/ipc";
import {
  onUpdate,
  updateCancel,
  updateCheck,
  updateSetChannel,
  updateSnooze,
  updateStart,
  updateStatus,
  type UpdateChannel,
  type UpdateStatus,
} from "../api/update";

export interface UpdateController {
  /** Null until the first status arrives. */
  status: UpdateStatus | null;
  popupVisible: boolean;
  setPopupVisible: (visible: boolean) => void;
  checkForUpdates: () => void;
  startUpdate: () => void;
  cancelUpdate: () => void;
  snoozeUpdate: () => void;
  chooseUpdateChannel: (channel: UpdateChannel) => void;
}

const warn = (what: string) => (e: unknown) => console.warn(`${what}: ${messageOf(e)}`);

export function useUpdate(): UpdateController {
  const [status, setStatus] = useState<UpdateStatus | null>(null);
  const [popupVisible, setPopupVisible] = useState(false);

  useEffect(() => {
    let alive = true;
    const off = onUpdate((s) => alive && setStatus(s));
    updateStatus()
      .then((s) => alive && setStatus((prev) => prev ?? s))
      .catch(warn("Update status unavailable"));
    return () => {
      alive = false;
      off();
    };
  }, []);

  // Auto-show the popup when the background check finds an update.
  const offered = !!status?.isUpdateAvailable && !status.snoozed;
  useEffect(() => {
    if (offered) setPopupVisible(true);
  }, [offered]);

  const checkForUpdates = useCallback(() => void updateCheck().catch(warn("Update check failed")), []);
  const startUpdate = useCallback(() => void updateStart().catch(warn("Update failed")), []);
  const cancelUpdate = useCallback(() => void updateCancel().catch(warn("Update cancel failed")), []);
  const snoozeUpdate = useCallback(() => {
    setPopupVisible(false);
    updateSnooze().catch(warn("Update snooze failed"));
  }, []);
  const chooseUpdateChannel = useCallback((channel: UpdateChannel) => {
    // shown at once; the core drops the other channel's offer and checks again
    setStatus((s) => (s ? { ...s, channel } : s));
    updateSetChannel(channel).catch(warn("Update channel not saved"));
  }, []);

  return useMemo(
    () => ({
      status,
      popupVisible,
      setPopupVisible,
      checkForUpdates,
      startUpdate,
      cancelUpdate,
      snoozeUpdate,
      chooseUpdateChannel,
    }),
    [status, popupVisible, checkForUpdates, startUpdate, cancelUpdate, snoozeUpdate, chooseUpdateChannel],
  );
}
