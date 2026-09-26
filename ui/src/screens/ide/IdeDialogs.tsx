/**
 * The Code page's questions (IdeDialogs.kt): a name for a new file or folder or a rename, a yes
 * for a delete, and what to do with unsaved edits. Each is a small sheet over a scrim with its
 * buttons at the right; the quieter choices are ghost buttons, the answer is the primary one.
 */
import { useEffect, useRef, useState } from "react";
import { Button } from "../../components/Button";
import { Dialog } from "../../components/Dialog";

/** Asks for a name: a new file's or folder's, or the new name of one. Enter confirms. */
export function NameDialog({
  title,
  initial,
  confirm,
  onDismiss,
  onConfirm,
}: {
  title: string;
  initial: string;
  confirm: string;
  onDismiss: () => void;
  onConfirm: (name: string) => void;
}) {
  const [value, setValue] = useState(initial);
  const input = useRef<HTMLInputElement>(null);
  useEffect(() => {
    const el = input.current;
    if (!el) return;
    el.focus();
    // The name is selected up to its extension, so typing replaces it and keeps the ".kt".
    const dot = initial.lastIndexOf(".");
    el.setSelectionRange(0, dot > 0 ? dot : initial.length);
  }, [initial]);
  const ok = value.trim().length > 0;
  return (
    <Dialog
      title={title}
      onDismiss={onDismiss}
      actions={
        <>
          <Button variant="ghost" text="Cancel" onClick={onDismiss} />
          <Button variant="primary" text={confirm} disabled={!ok} onClick={() => onConfirm(value.trim())} />
        </>
      }
    >
      <input
        ref={input}
        className="ide-name-input"
        value={value}
        spellCheck={false}
        onChange={(e) => setValue(e.target.value)}
        onKeyDown={(e) => {
          if (e.key === "Enter" && ok) {
            e.preventDefault();
            onConfirm(value.trim());
          }
        }}
      />
    </Dialog>
  );
}

/** A yes-or-no question with a sentence of explanation. */
export function ConfirmDialog({
  title,
  body,
  confirm,
  onDismiss,
  onConfirm,
}: {
  title: string;
  body: string;
  confirm: string;
  onDismiss: () => void;
  onConfirm: () => void;
}) {
  return (
    <Dialog
      title={title}
      onDismiss={onDismiss}
      actions={
        <>
          <Button variant="ghost" text="Cancel" onClick={onDismiss} />
          <Button variant="primary" text={confirm} onClick={onConfirm} />
        </>
      }
    >
      {body}
    </Dialog>
  );
}

/** Unsaved edits are about to be lost: save them, drop them, or stay. */
export function UnsavedDialog({
  names,
  onDismiss,
  onDiscard,
  onSave,
}: {
  names: string[];
  onDismiss: () => void;
  onDiscard: () => void;
  onSave: () => void;
}) {
  const one = names.length === 1;
  return (
    <Dialog
      title={one ? `Save changes to ${names[0]}?` : `Save changes to ${names.length} files?`}
      onDismiss={onDismiss}
      actions={
        <>
          <Button variant="ghost" text="Don't save" onClick={onDiscard} />
          <Button variant="ghost" text="Cancel" onClick={onDismiss} />
          <Button variant="primary" text="Save" onClick={onSave} />
        </>
      }
    >
      {one ? "The edits are lost if you don't save them." : `${names.join(", ")}. The edits are lost if you don't save them.`}
    </Dialog>
  );
}
