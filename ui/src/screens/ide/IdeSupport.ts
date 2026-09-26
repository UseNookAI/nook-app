/**
 * The Code page's pure helpers (IdeSupport.kt): the explorer's rows, a file's language from its
 * name, paths within the open folder, and the selected lines. No DOM, so the tests run in node.
 * A path's last part and a folder's name are components/paths' baseName and repoName.
 */
import type { EditorState } from "@codemirror/state";
import type { FileNode } from "../../api/ide";

/** One row of the explorer: an entry and how deep it sits under the open folder. */
export interface TreeRow {
  node: FileNode;
  depth: number;
}

/** The rows the explorer shows: the root's entries, and under each open folder its own, in order. */
export function treeRows(root: string, expanded: ReadonlySet<string>, children: ReadonlyMap<string, FileNode[]>): TreeRow[] {
  const out: TreeRow[] = [];
  const walk = (dir: string, depth: number) => {
    for (const n of children.get(dir) ?? []) {
      out.push({ node: n, depth });
      if (n.isDirectory && expanded.has(n.path)) walk(n.path, depth + 1);
    }
  };
  walk(root, 0);
  return out;
}

// ---------------------------------------------------------------------------- languages

/** A file's language: the status bar's name, the CodeMirror mode that colours it, real tabs or spaces. */
export interface Language {
  /** As the status bar says it ("Kotlin", "Plain text"). */
  name: string;
  /** The @codemirror/language-data name to load, or null for none. */
  mode: string | null;
  /** Makefiles and Go want real tabs; everything else gets four spaces. */
  tabs: boolean;
}

const lang = (name: string, mode: string | null, tabs = false): Language => ({ name, mode, tabs });

export const PLAIN_TEXT: Language = lang("Plain text", null);

/** Whole file names that decide the language on their own. */
const BY_NAME: Record<string, Language> = {
  makefile: lang("Makefile", null, true),
  gnumakefile: lang("Makefile", null, true),
  dockerfile: lang("Dockerfile", "Dockerfile"),
  containerfile: lang("Dockerfile", "Dockerfile"),
  hosts: lang("Hosts", null),
  ".htaccess": lang(".htaccess", null),
};

/** Extensions, as the original's table; where CodeMirror has a closer mode (JSX, TOML) it is used. */
const BY_EXTENSION: Record<string, Language> = {};
const table: [string, Language][] = [
  ["kt kts", lang("Kotlin", "Kotlin")],
  ["java", lang("Java", "Java")],
  ["groovy gradle", lang("Groovy", "Groovy")],
  ["scala sc", lang("Scala", "Scala")],
  ["py pyw pyi", lang("Python", "Python")],
  ["js mjs cjs", lang("JavaScript", "JavaScript")],
  ["jsx", lang("JavaScript", "JSX")],
  ["ts mts cts", lang("TypeScript", "TypeScript")],
  ["tsx", lang("TypeScript", "TSX")],
  ["json jsonl webmanifest", lang("JSON", "JSON")],
  ["jsonc json5", lang("JSON with comments", "JSON")],
  ["yml yaml", lang("YAML", "YAML")],
  ["xml xsd xsl xslt svg plist csproj fsproj props targets iml pom manifest", lang("XML", "XML")],
  ["html htm xhtml", lang("HTML", "HTML")],
  ["jsp", lang("JSP", "HTML")],
  ["css", lang("CSS", "CSS")],
  ["less", lang("Less", "LESS")],
  ["md markdown", lang("Markdown", "Markdown")],
  ["sql", lang("SQL", "SQL")],
  ["c h", lang("C", "C")],
  ["cpp cc cxx hpp hh hxx ino", lang("C++", "C++")],
  ["cs", lang("C#", "C#")],
  ["go", lang("Go", "Go", true)],
  ["rs", lang("Rust", "Rust")],
  ["rb rake gemspec", lang("Ruby", "Ruby")],
  ["php", lang("PHP", "PHP")],
  ["sh bash zsh", lang("Shell", "Shell")],
  ["ps1 psm1 psd1", lang("PowerShell", "PowerShell")],
  ["bat cmd", lang("Batch", null)],
  ["ini cfg conf editorconfig gitconfig", lang("INI", "Properties files")],
  ["properties env", lang("Properties", "Properties files")],
  ["toml", lang("Properties", "TOML")],
  ["lua", lang("Lua", "Lua")],
  ["pl pm", lang("Perl", "Perl")],
  ["dart", lang("Dart", "Dart")],
  ["proto", lang("Protocol Buffers", "ProtoBuf")],
  ["csv", lang("CSV", null)],
  ["tex latex bib", lang("LaTeX", "LaTeX")],
  ["clj", lang("Clojure", "Clojure")],
  ["cljs", lang("Clojure", "ClojureScript")],
  ["edn", lang("Clojure", "edn")],
  ["lisp el", lang("Lisp", "Common Lisp")],
  ["scm", lang("Lisp", "Scheme")],
  ["tcl", lang("Tcl", "Tcl")],
  ["vb", lang("Visual Basic", "VB.NET")],
  ["vbs", lang("Visual Basic", "VBScript")],
  ["hbs handlebars", lang("Handlebars", "HTML")],
  ["d", lang("D", "D")],
  ["f f90 for", lang("Fortran", "Fortran")],
  ["asm s", lang("Assembly", "Gas")],
  ["dtd", lang("DTD", "DTD")],
  ["nsi nsh", lang("NSIS", "NSIS")],
  ["vhd vhdl", lang("VHDL", "VHDL")],
  ["sas", lang("SAS", "SAS")],
  ["pas dpr", lang("Delphi", "Pascal")],
  ["as", lang("ActionScript", "JavaScript")],
  ["mxml", lang("MXML", "XML")],
];
for (const [exts, l] of table) for (const e of exts.split(" ")) BY_EXTENSION[e] = l;

/** The editor's language for a file, from its name; plain text when there is none for it. */
export function languageFor(fileName: string): Language {
  const lower = fileName.toLowerCase();
  const named = BY_NAME[lower];
  if (named) return named;
  const dot = lower.lastIndexOf(".");
  const ext = dot < 0 ? "" : lower.slice(dot + 1);
  return BY_EXTENSION[ext] ?? PLAIN_TEXT;
}

// ---------------------------------------------------------------------------- paths

/** The folder a path sits in ("C:\\" for "C:\\a.txt"), or null at the top. */
export function parentDir(path: string): string | null {
  const i = Math.max(path.lastIndexOf("\\"), path.lastIndexOf("/"));
  if (i < 0) return null;
  const parent = path.slice(0, i);
  if (parent === "" || parent.endsWith(":")) return parent + path[i];
  return parent;
}

/** Whether `path` is `base` or lies inside it (a component-wise startsWith, like Path.startsWith). */
export function isUnder(path: string, base: string): boolean {
  if (path === base) return true;
  if (/[\\/]$/.test(base)) return path.startsWith(base);
  return path.startsWith(base + "\\") || path.startsWith(base + "/");
}

/** A path within the folder, with the folder's slashes made plain ("src/main.ts"). */
export function relativeName(folder: string | null, path: string): string {
  let rel = path;
  if (folder && path !== folder && isUnder(path, folder)) rel = path.slice(folder.length).replace(/^[\\/]+/, "");
  return rel.replace(/\\/g, "/");
}

/** Why a new name cannot be used (empty, a dot name, a separator or colon in it), or null. */
export function nameProblem(name: string): string | null {
  const n = name.trim();
  if (!n || n === "." || n === ".." || /[/\\:]/.test(n)) return `"${name}" is not a file name.`;
  return null;
}

// ---------------------------------------------------------------------------- the selection

/** The selected lines, first and last (1-based), or null when nothing is selected. */
export function selectedLines(state: EditorState): [number, number] | null {
  const sel = state.selection.main;
  if (sel.empty) return null;
  const from = state.doc.lineAt(sel.from).number;
  const end = state.doc.lineAt(sel.to);
  let to = end.number;
  // A selection that ends at the start of a line does not hold that line.
  if (to > from && end.from === sel.to) to--;
  return [from, to];
}

/** The selected text for a request: at most 4000 characters, trimmed at the end; null when blank. */
export function selectedText(state: EditorState): string | null {
  const sel = state.selection.main;
  if (sel.empty) return null;
  const text = state.doc.sliceString(sel.from, sel.to);
  if (!text.trim()) return null;
  return text.slice(0, 4000).trimEnd();
}
