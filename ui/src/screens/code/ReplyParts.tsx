/**
 * ReplyParts.kt: how a reply is shown. The summary is plain selectable text (the original renders
 * it as is, no markdown), each changed file is a code block, and a toolbar sits under the reply.
 */
import { useMemo, useState, type ReactNode } from "react";
import { ToolbarIcon } from "../../components/Button";
import { FoldToggle } from "./CodeControls";
import type { DiffFile } from "./Diffs";
import "./code.css";

/** Text that reads like a message and can be selected. */
export function SelectableText({ children, className }: { children: ReactNode; className?: string }) {
  return <div className={className ? `nc-selectable selectable ${className}` : "nc-selectable selectable"}>{children}</div>;
}

/** Copies text to the clipboard; a failure is quiet, as Compose's clipboard was. */
export function copyText(text: string): void {
  navigator.clipboard?.writeText(text).catch(() => undefined);
}

/** More lines than this and a code block opens folded to its first [FOLDED_LINES]. */
const FOLD_OVER = 36;
const FOLDED_LINES = 24;

/**
 * One file of a reply, the way a conversation shows code: its name and language on top, the code
 * below. A new file shows in full; an edited one shows its changed lines, added in green and
 * removed in red.
 */
export function CodeBlock({ file }: { file: DiffFile }) {
  const lines = useMemo(() => {
    const body = file.lines.filter((l) => !l.startsWith("\\ No newline"));
    return file.isNew ? body.filter((l) => l.startsWith("+")).map((l) => l.substring(1)) : body;
  }, [file]);
  const [all, setAll] = useState(false);
  const long = lines.length > FOLD_OVER;
  const shown = long && !all ? lines.slice(0, FOLDED_LINES) : lines;
  return (
    <div className="nc-code-block">
      <div className="nc-code-block__head">
        {/* Name and language take the room; the copy icon keeps the right edge. */}
        <div className="nc-code-block__names">
          <span className="nc-code-block__path caption">{file.path}</span>
          <span className="caption text-tertiary nc-nowrap">
            {file.isNew ? languageOf(file.path) : file.isDeleted ? "deleted" : `+${file.added} −${file.removed}`}
          </span>
        </div>
        <ToolbarIcon icon="copy" hint="Copy" onClick={() => copyText(lines.join("\n"))} />
      </div>
      {file.isDeleted ? (
        <div className="body2 text-secondary nc-code-block__deleted">This file is deleted.</div>
      ) : (
        <>
          <pre className="nc-code-block__code selectable">
            {shown.map((line, i) => (
              <span key={i} className={file.isNew ? undefined : lineClass(line)}>
                {line}
                {i < shown.length - 1 ? "\n" : ""}
              </span>
            ))}
          </pre>
          {long && (
            <div className="nc-code-block__fold">
              <FoldToggle text={all ? "Show less" : `Show all ${lines.length} lines`} open={all} onClick={() => setAll(!all)} />
            </div>
          )}
        </>
      )}
    </div>
  );
}

function lineClass(line: string): string | undefined {
  if (line.startsWith("@@")) return "nc-diff-hunk";
  if (line.startsWith("+")) return "nc-diff-added";
  if (line.startsWith("-")) return "nc-diff-removed";
  return undefined;
}

/** A code block's language, from its file name. */
export function languageOf(path: string): string {
  const dot = path.lastIndexOf(".");
  const ext = dot >= 0 ? path.substring(dot + 1).toLowerCase() : "";
  switch (ext) {
    case "sol":
      return "Solidity";
    case "py":
      return "Python";
    case "kt":
    case "kts":
      return "Kotlin";
    case "java":
      return "Java";
    case "js":
    case "mjs":
    case "cjs":
      return "JavaScript";
    case "ts":
      return "TypeScript";
    case "tsx":
      return "TSX";
    case "jsx":
      return "JSX";
    case "rs":
      return "Rust";
    case "go":
      return "Go";
    case "rb":
      return "Ruby";
    case "cs":
      return "C#";
    case "c":
    case "h":
      return "C";
    case "cpp":
    case "cc":
    case "hpp":
      return "C++";
    case "swift":
      return "Swift";
    case "html":
    case "htm":
      return "HTML";
    case "css":
      return "CSS";
    case "json":
      return "JSON";
    case "yml":
    case "yaml":
      return "YAML";
    case "md":
      return "Markdown";
    case "sh":
      return "Shell";
    case "ps1":
      return "PowerShell";
    case "sql":
      return "SQL";
    case "toml":
      return "TOML";
    case "xml":
      return "XML";
    case "gradle":
      return "Gradle";
    case "":
      return "Text";
    default:
      return path.substring(dot + 1).toUpperCase();
  }
}

/**
 * Under a reply: copy it, and, on the reply whose change is not in the person's files yet, undo,
 * discard and apply.
 */
export function ReplyToolbar({
  onCopy,
  stat,
  pending,
  canUndo,
  onUndo,
  onDiscard,
  onApply,
}: {
  onCopy: () => void;
  stat: string | null;
  pending: boolean;
  canUndo: boolean;
  onUndo: () => void;
  onDiscard: () => void;
  onApply: () => void;
}) {
  return (
    <div className="nc-reply-toolbar">
      <ToolbarIcon icon="copy" hint="Copy the reply" onClick={onCopy} />
      {/* Cut short before the buttons are, on the Code page's narrow panel. */}
      {stat != null && <span className="caption text-tertiary nc-reply-toolbar__stat">{stat}</span>}
      <span className="nc-flex-spacer" />
      {pending && (
        <>
          {canUndo && <ToolbarIcon icon="redo" hint="Undo this request" onClick={onUndo} />}
          <ToolbarIcon icon="trash" hint="Discard the change" onClick={onDiscard} />
          <button type="button" className="nc-apply" onClick={onApply}>
            Apply to your files
          </button>
        </>
      )}
    </div>
  );
}
