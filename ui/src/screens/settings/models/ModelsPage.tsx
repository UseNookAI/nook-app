/**
 * Models (AiModelsSettingsView.kt): the curated library, everything with GGUF files on Hugging
 * Face, and which installed model serves each kind of work. With the header's search field
 * (ModelsSearchField) and the state the header shares with the page (ModelsPageState).
 */
import { useCallback, useState } from "react";
import { SearchField } from "../../../components/TextField";
import { SettingsSubNav } from "../components";
import { HubBrowserView } from "./HubBrowserView";
import { KINDS, type Kind } from "./library";
import { LibraryView } from "./LibraryView";
import type { ModelsData } from "./useModelsData";
import { WorkerModelsView } from "./WorkerModelsView";
import "./models.css";

export const MODELS_PAGES = ["Library", "Browse", "Workers"] as const;
export type ModelsPageName = (typeof MODELS_PAGES)[number];

/**
 * What the Settings header shares with the Models page (ModelsPageState): the open view, the text
 * in the search field (which lives in the header), the Enter count Browse searches on, and the
 * kind filter. `forCode` is Models opened from Code's model menu: it starts on the models Code can
 * use, and choosing one closes Settings.
 */
export interface ModelsPageState {
  forCode: boolean;
  page: ModelsPageName;
  query: string;
  submit: number;
  kind: Kind;
  setPage: (page: ModelsPageName) => void;
  setQuery: (query: string) => void;
  /** Enter in the field: Browse searches at once. */
  bumpSubmit: () => void;
  setKind: (kind: Kind) => void;
}

export function useModelsPageState(forCode: boolean): ModelsPageState {
  const [page, setPage] = useState<ModelsPageName>("Library");
  const [query, setQuery] = useState("");
  const [submit, setSubmit] = useState(0);
  const [kind, setKind] = useState<Kind>(forCode ? "Code" : "All");
  const bumpSubmit = useCallback(() => setSubmit((n) => n + 1), []);
  return { forCode, page, query, submit, kind, setPage, setQuery, bumpSubmit, setKind };
}

/** The search field in the Settings header. Library filters as you type; Browse searches Hugging Face. */
export function ModelsSearchField({ state }: { state: ModelsPageState }) {
  if (state.page === "Workers") return null;
  return (
    <SearchField
      className="nk-models-search"
      width={320}
      value={state.query}
      onChange={state.setQuery}
      onEnter={state.bumpSubmit}
      placeholder={state.page === "Library" ? "Filter the library" : "Search Hugging Face"}
      aria-label={state.page === "Library" ? "Filter the library" : "Search Hugging Face"}
    />
  );
}

export function ModelsPage({ state, data, onPicked }: { state: ModelsPageState; data: ModelsData; onPicked: () => void }) {
  return (
    <div className="nk-models">
      <div className="nk-models__nav">
        <SettingsSubNav
          options={MODELS_PAGES}
          selectedIndex={MODELS_PAGES.indexOf(state.page)}
          onOptionSelected={(i) => {
            state.setPage(MODELS_PAGES[i]);
            state.setQuery("");
          }}
        />
        <span className="nk-models__nav-spacer" />
        {state.page === "Library" && (
          <SettingsSubNav options={KINDS} selectedIndex={KINDS.indexOf(state.kind)} onOptionSelected={(i) => state.setKind(KINDS[i])} />
        )}
      </div>
      {state.page === "Library" && <LibraryView data={data} query={state.query} kind={state.kind} forCode={state.forCode} onPicked={onPicked} />}
      {state.page === "Browse" && (
        <HubBrowserView
          query={state.query}
          submit={state.submit}
          downloads={data.runtimeDownloads}
          onQuickSearch={(q) => {
            state.setQuery(q);
            state.bumpSubmit();
          }}
        />
      )}
      {state.page === "Workers" && <WorkerModelsView catalog={data.catalog} installed={data.installed} />}
    </div>
  );
}
