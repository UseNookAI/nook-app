/** CodeComposer.kt: where the person writes the next request. */
import { useCallback, useLayoutEffect, useRef, useState } from "react";
import { Icon } from "../../components/Icon";
import { repoName } from "../../components/paths";
import { DropdownMenu } from "../../components/Popover";
import { StyledMenuItem } from "../../components/StyledMenuItem";
import { PromptActionIconButton, ToolbarPill } from "../hub/ComposerControls";
import { SpeechButton } from "./CodeModels";
import { ContextMeter, type ContextReading } from "./ContextMeter";
import { chooseFolder } from "./FolderPicker";
import "./code.css";

/** The prompt box grows with its text up to this height, then scrolls (HeightPrompt). */
const HEIGHT_PROMPT = 160;

export interface CodeComposerProps {
  text: string;
  onTextChange: (text: string) => void;
  placeholder: string;
  repository: string | null;
  repoLocked: boolean;
  repositories: string[];
  onRepository: (path: string) => void;
  workerName: string | null;
  running: boolean;
  onSend: () => void;
  onStop: () => void;
  repositoryReady?: boolean;
  onNotice?: (message: string) => void;
  context?: ContextReading | null;
  /** False on the Code page's Nook panel, whose header already names the folder. */
  showRepository?: boolean;
}

/**
 * Where the person writes the next request: the text, the folder (chosen once per session),
 * how full the worker's context is ([context], in a session), voice input, and send or stop.
 */
export function CodeComposer({
  text,
  onTextChange,
  placeholder,
  repository,
  repoLocked,
  repositories,
  onRepository,
  workerName,
  running,
  onSend,
  onStop,
  repositoryReady = true,
  onNotice = () => undefined,
  context = null,
  showRepository = true,
}: CodeComposerProps) {
  const canSend = text.trim() !== "" && repository != null && repositoryReady && workerName != null && !running;
  const box = useRef<HTMLTextAreaElement>(null);

  // Grows with the text, then scrolls.
  useLayoutEffect(() => {
    const el = box.current;
    if (!el) return;
    el.style.height = "auto";
    el.style.height = `${Math.min(el.scrollHeight, HEIGHT_PROMPT)}px`;
  }, [text]);

  return (
    <div className="nk-composer nc-composer">
      <textarea
        ref={box}
        className="nc-composer__input body1"
        value={text}
        rows={1}
        placeholder={placeholder}
        onChange={(e) => onTextChange(e.target.value)}
        onKeyDown={(e) => {
          if (e.key !== "Enter" || e.nativeEvent.isComposing) return;
          // Shift+Enter is a new line; Enter sends when it can and is swallowed when it cannot.
          if (e.shiftKey) return;
          e.preventDefault();
          if (canSend) onSend();
        }}
      />
      <div className="nc-composer__row">
        {showRepository && <RepositoryPicker repository={repository} locked={repoLocked} repositories={repositories} onRepository={onRepository} />}
        <span className="nc-flex-spacer" />
        {context != null && <ContextMeter reading={context} />}
        <SpeechButton
          enabled={!running}
          onText={(said) => {
            const joined = text.trim() === "" ? said : text.trimEnd() + " " + said;
            onTextChange(joined);
            // The caret goes to the end, after what was said.
            requestAnimationFrame(() => {
              const el = box.current;
              if (el) {
                el.focus();
                el.setSelectionRange(joined.length, joined.length);
              }
            });
          }}
          onError={onNotice}
        />
        <PromptActionIconButton isGenerating={running} isEnabled={canSend || running} onSend={onSend} onCancel={onStop} />
      </div>
    </div>
  );
}

/**
 * The folder pill: the recent folders and Choose a folder…, or locked to the session's folder.
 * Choose a folder… shows the system's folder picker and hands on what was picked, unless
 * [onChoose] takes it over (the Code page asks through its own picker).
 */
export function RepositoryPicker({
  repository,
  locked,
  repositories,
  onRepository,
  onChoose,
}: {
  repository: string | null;
  locked: boolean;
  repositories: string[];
  onRepository: (path: string) => void;
  /** Called with the folder the picker should start in, in place of the built-in picker. */
  onChoose?: (start: string | null) => void;
}) {
  const [open, setOpen] = useState(false);
  const anchor = useRef<HTMLButtonElement>(null);
  const close = useCallback(() => setOpen(false), []);
  return (
    <>
      <ToolbarPill
        ref={anchor}
        text={repository ? repoName(repository) : "Choose a folder"}
        icon="folder-open"
        onClick={() => {
          if (!locked) setOpen(!open);
        }}
        expanded={open}
        locked={locked}
        emphasised={repository == null}
        maxWidth={240}
        title={repository ?? undefined}
      />
      <DropdownMenu anchor={anchor} open={open} onClose={close} role="menu">
        {repositories.slice(0, 8).map((r) => (
          <RepoRow
            key={r}
            path={r}
            selected={r === repository}
            onClick={() => {
              setOpen(false);
              onRepository(r);
            }}
          />
        ))}
        <StyledMenuItem
          text="Choose a folder…"
          icon="folder-plus"
          onClick={() => {
            setOpen(false);
            const start = repository ?? repositories[0] ?? null;
            if (onChoose) {
              onChoose(start);
              return;
            }
            chooseFolder(start)
              .then((picked) => {
                if (picked) onRepository(picked);
              })
              .catch(() => undefined);
          }}
        />
      </DropdownMenu>
    </>
  );
}

/** A repository in the menu: its folder name, and the whole path on one line under it. */
function RepoRow({ path, selected, onClick }: { path: string; selected: boolean; onClick: () => void }) {
  return (
    <button type="button" role="menuitem" className="nc-repo-row" onClick={onClick}>
      <Icon name="folder-empty" size={18} color="var(--text-secondary)" />
      <span className="nc-repo-row__text">
        <span className="body2 nc-ellipsis" style={{ fontWeight: selected ? 600 : 500 }}>
          {repoName(path)}
        </span>
        <span className="caption text-tertiary nc-ellipsis">{path}</span>
      </span>
    </button>
  );
}
