/**
 * LanguagePicker.kt: a language pill with a menu of the languages Nook offers. `code` null is
 * "Detect automatically", which `allowAuto` puts first. New here: typing narrows the list, since
 * it is long.
 */
import { useEffect, useRef, useState } from "react";
import type { Language } from "../../api/flows";
import { DropdownMenu } from "../../components/Popover";
import { StyledMenuItem } from "../../components/StyledMenuItem";
import { ToolbarPill } from "../hub/ComposerControls";

/** The label for no language: Whisper tells which one is spoken. */
export const DETECT = "Detect automatically";

export function LanguagePicker({
  label,
  code,
  languages,
  allowAuto,
  onPick,
}: {
  label: string;
  code: string | null;
  languages: Language[];
  allowAuto: boolean;
  onPick: (code: string | null) => void;
}) {
  const [open, setOpen] = useState(false);
  const [filter, setFilter] = useState("");
  const pill = useRef<HTMLButtonElement>(null);
  const field = useRef<HTMLInputElement>(null);
  const name = code == null ? DETECT : (languages.find((l) => l.code === code)?.name ?? code);

  useEffect(() => {
    if (!open) return;
    setFilter("");
    requestAnimationFrame(() => field.current?.focus());
  }, [open]);

  const wanted = filter.trim().toLowerCase();
  const shown = languages.filter((l) => !wanted || l.name.toLowerCase().startsWith(wanted) || l.code === wanted);
  const pick = (c: string | null) => {
    setOpen(false);
    onPick(c);
  };

  return (
    <>
      <span className="fl-lang">
        <span className="caption text-tertiary fl-lang__label">{label}</span>
        <ToolbarPill ref={pill} text={name} expanded={open} onClick={() => setOpen(!open)} maxWidth={220} />
      </span>
      <DropdownMenu anchor={pill} open={open} onClose={() => setOpen(false)} role="menu">
        <input
          ref={field}
          className="fl-lang__filter body2"
          placeholder="Type to find a language"
          value={filter}
          onChange={(e) => setFilter(e.target.value)}
          onKeyDown={(e) => {
            if (e.key === "Enter" && shown.length > 0) pick(shown[0].code);
          }}
        />
        <div className="fl-lang__list">
          {allowAuto && !wanted && (
            <StyledMenuItem
              text={DETECT}
              icon="check"
              // Every row has the tick's room, so the names line up; only the chosen one shows it.
              tint={code == null ? undefined : "transparent"}
              selected={code == null}
              fontWeight={code == null ? 600 : undefined}
              onClick={() => pick(null)}
            />
          )}
          {shown.map((l) => (
            <StyledMenuItem
              key={l.code}
              text={l.name}
              icon="check"
              tint={l.code === code ? undefined : "transparent"}
              selected={l.code === code}
              fontWeight={l.code === code ? 600 : undefined}
              onClick={() => pick(l.code)}
            />
          ))}
          {shown.length === 0 && <div className="caption text-tertiary fl-lang__none">No language by that name.</div>}
        </div>
      </DropdownMenu>
    </>
  );
}
