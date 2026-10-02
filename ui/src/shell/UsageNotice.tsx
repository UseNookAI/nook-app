/**
 * The one-time notice about the daily usage report (nook_core::usage), in the hub's lower right
 * corner. The core sends nothing before it has been on screen, so it marks itself seen as soon as
 * it shows. "Turn off" turns the reports off there and then; "What is sent" opens Settings ›
 * General, where the switch and the next report are.
 */
import { useEffect } from "react";
import { usageNoticeSeen, usageSet } from "../api/usage";
import { Button } from "../components/Button";

export function UsageNotice({ onClose, onOpenSettings }: { onClose: () => void; onOpenSettings: () => void }) {
  useEffect(() => {
    usageNoticeSeen().catch(() => {});
  }, []);

  const turnOff = () => {
    usageSet(false)
      .catch(() => {})
      .finally(onClose);
  };

  return (
    <div className="nk-usage-notice" role="status" aria-label="Usage statistics">
      <div className="body1 nk-usage-notice__title">Anonymous usage statistics</div>
      <div className="body2 nk-usage-notice__text">
        Once a day Nook tells us which tools were used, how often and whether they worked, with the app version and your
        graphics card's maker and memory. Never your files, what you type or say, or anything that names you.
      </div>
      <div className="nk-usage-notice__actions">
        <Button variant="ghost" text="What is sent" onClick={onOpenSettings} />
        <Button variant="secondary" text="Turn off" onClick={turnOff} />
        <Button variant="primary" text="OK" onClick={onClose} />
      </div>
    </div>
  );
}
