/**
 * The Code page's disk in a plain browser: a small made-up project in a few languages, kept in
 * memory, with the same ordering, limits and messages as nook_core::ide. The page's own state
 * (ide.json) survives a reload through localStorage.
 */
import { mock } from "../ipc";
import { DEFAULT_PREFS, type FileNode, type IdePrefs, type LoadedFile } from "../ide";

const ROOT = "C:\\Users\\you\\code\\nook-demo";
const PREFS_KEY = "nook-mock-ide-prefs";

interface MockFile {
  text: string;
  time: number;
  lineEnding: string;
  charset: string;
  binary?: boolean;
}
type Entry = { dir: true } | MockFile;

const fs = new Map<string, Entry>();

function put(rel: string, text: string, lineEnding = "LF") {
  const path = rel ? `${ROOT}\\${rel}` : ROOT;
  fs.set(path, { text, time: Date.now() - 3_600_000, lineEnding, charset: "UTF-8" });
}
function dir(rel: string) {
  fs.set(rel ? `${ROOT}\\${rel}` : ROOT, { dir: true });
}

dir("");
for (const d of ["src", "src\\components", "server", "scripts", "docs", "assets", ".git"]) dir(d);
put(
  "README.md",
  `# nook-demo

A small project to try the Code page with.

- \`src/\`: the web front end (TypeScript and React)
- \`server/\`: a Python API and a Go handler
- \`scripts/\`: the build

Run \`npm run dev\` and open http://localhost:5173.
`,
);
put(
  "package.json",
  `{
  "name": "nook-demo",
  "version": "0.1.0",
  "private": true,
  "scripts": {
    "dev": "vite",
    "build": "tsc -b && vite build"
  },
  "dependencies": {
    "react": "^19.0.0"
  }
}
`,
);
put(".gitignore", "node_modules/\ndist/\n*.log\n");
put(
  "Cargo.toml",
  `[package]
name = "nook-demo-core"
version = "0.1.0"
edition = "2021"

[dependencies]
serde = { version = "1", features = ["derive"] }
`,
);
put(
  "src\\main.ts",
  `import { createRoot } from "react-dom/client";
import { App } from "./App";
import "./styles.css";

/** Mounts the app, or says why it could not. */
function start(): void {
  const root = document.getElementById("root");
  if (!root) {
    console.error("No #root element");
    return;
  }
  createRoot(root).render(App({ title: "Nook demo", count: 3 }));
}

start();
`,
);
put(
  "src\\App.tsx",
  `import { useState } from "react";
import { Counter } from "./components/Counter";

export interface AppProps {
  title: string;
  count: number;
}

// The page: a title and a counter that starts at \`count\`.
export function App({ title, count }: AppProps) {
  const [value, setValue] = useState(count);
  return (
    <main className="app">
      <h1>{title}</h1>
      <Counter value={value} onChange={setValue} />
      {value > 10 && <p className="note">That is a lot of clicks.</p>}
    </main>
  );
}
`,
);
put(
  "src\\components\\Counter.tsx",
  `export function Counter({ value, onChange }: { value: number; onChange: (v: number) => void }) {
  return (
    <div className="counter">
      <button onClick={() => onChange(value - 1)}>-</button>
      <span>{value}</span>
      <button onClick={() => onChange(value + 1)}>+</button>
    </div>
  );
}
`,
);
put(
  "src\\styles.css",
  `:root {
  --accent: #446d49;
}

.app {
  font-family: system-ui, sans-serif;
  max-width: 640px;
  margin: 48px auto;
}

.counter button {
  width: 32px;
  height: 32px;
  border-radius: 8px;
}
`,
);
put(
  "src\\lib.rs",
  `use serde::{Deserialize, Serialize};

/// A to-do item as the API returns it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Todo {
    pub id: u64,
    pub title: String,
    pub done: bool,
}

impl Todo {
    pub fn new(id: u64, title: impl Into<String>) -> Self {
        Todo { id, title: title.into(), done: false }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_new_todo_is_open() {
        assert!(!Todo::new(1, "write tests").done);
    }
}
`,
);
put(
  "server\\app.py",
  `"""A tiny API for the demo's to-do list."""
from dataclasses import dataclass, field
from http import HTTPStatus

MAX_TODOS = 100


@dataclass
class Todo:
    id: int
    title: str
    done: bool = False
    tags: list[str] = field(default_factory=list)


def add(todos: list[Todo], title: str) -> tuple[Todo, HTTPStatus]:
    if len(todos) >= MAX_TODOS:
        raise ValueError(f"no more than {MAX_TODOS} to-dos")
    todo = Todo(id=len(todos) + 1, title=title.strip())
    todos.append(todo)
    return todo, HTTPStatus.CREATED
`,
  "CRLF",
);
put(
  "server\\handler.go",
  `package server

import (
	"encoding/json"
	"net/http"
)

// Health answers the load balancer's checks.
func Health(w http.ResponseWriter, r *http.Request) {
	w.Header().Set("Content-Type", "application/json")
	_ = json.NewEncoder(w).Encode(map[string]string{"status": "ok"})
}
`,
);
put(
  "scripts\\build.ps1",
  `# Builds the web app and the Rust core.
$ErrorActionPreference = "Stop"
Push-Location (Split-Path $PSScriptRoot)
try {
    npm run build
    cargo build --release
} finally {
    Pop-Location
}
`,
  "CRLF",
);
fs.set(`${ROOT}\\assets\\logo.png`, { text: "", time: Date.now(), lineEnding: "LF", charset: "UTF-8", binary: true });

const isDir = (p: string) => (fs.get(p) as { dir?: true } | undefined)?.dir === true;
const nameOf = (p: string) => p.slice(p.lastIndexOf("\\") + 1) || p;
const parentOf = (p: string) => p.slice(0, p.lastIndexOf("\\"));
const node = (p: string): FileNode => ({ path: p, name: nameOf(p), isDirectory: isDir(p) });

function checkedName(name: string): string {
  const n = name.trim();
  if (!n || n === "." || n === ".." || /[/\\:]/.test(n)) throw new Error(`"${name}" is not a file name.`);
  return n;
}

function list(dirPath: string): FileNode[] {
  if (!isDir(dirPath)) return [];
  return [...fs.keys()]
    .filter((p) => parentOf(p) === dirPath && p !== dirPath && nameOf(p) !== ".git")
    .map(node)
    .sort(
      (a, b) =>
        Number(b.isDirectory) - Number(a.isDirectory) ||
        (a.name.toLowerCase() < b.name.toLowerCase() ? -1 : a.name.toLowerCase() > b.name.toLowerCase() ? 1 : 0) ||
        (a.name < b.name ? -1 : a.name > b.name ? 1 : 0),
    );
}

function loadPrefs(): IdePrefs {
  try {
    const saved = localStorage.getItem(PREFS_KEY);
    if (saved) return { ...DEFAULT_PREFS, ...(JSON.parse(saved) as Partial<IdePrefs>) };
  } catch {
    // No storage here: start from the defaults.
  }
  return { ...DEFAULT_PREFS };
}

export function registerIdeMocks(): void {
  mock("ide_load_prefs", () => {
    const prefs = loadPrefs();
    return prefs.folder && !isDir(prefs.folder) ? { ...prefs, folder: null } : prefs;
  });
  mock("ide_save_prefs", ({ prefs }) => {
    try {
      localStorage.setItem(PREFS_KEY, JSON.stringify(prefs));
    } catch {
      // Not remembered in this browser.
    }
  });
  mock("ide_mock_choose_folder", () => ROOT);
  mock("ide_resolve_folder", ({ path }) => String(path).replace(/[\\/]+$/, ""));
  mock("ide_branch", ({ dir }) => (String(dir).startsWith(ROOT) ? "main" : null));
  mock("ide_list_dir", ({ dir }) => list(String(dir)));
  mock("ide_read_file", ({ path }): LoadedFile => {
    const p = String(path);
    const f = fs.get(p);
    if (!f || "dir" in f) throw new Error(`Could not open ${nameOf(p)}: The system cannot find the file specified. (os error 2)`);
    if (f.binary) throw new Error(`${nameOf(p)} is not a text file.`);
    return { text: f.text, lineEnding: f.lineEnding, charset: f.charset, time: f.time };
  });
  mock("ide_write_file", ({ path, text, lineEnding, charset }) => {
    const p = String(path);
    const time = Date.now();
    fs.set(p, { text: String(text), time, lineEnding: String(lineEnding), charset: String(charset) });
    return time;
  });
  mock("ide_file_times", ({ paths }) =>
    (paths as string[]).map((p) => {
      const f = fs.get(p);
      return f && !("dir" in f) ? f.time : null;
    }),
  );
  mock("ide_create_file", ({ dir: d, name }) => {
    const path = `${d}\\${checkedName(String(name))}`;
    if (fs.has(path)) throw new Error(`${name} is already there.`);
    fs.set(path, { text: "", time: Date.now(), lineEnding: "CRLF", charset: "UTF-8" });
    return path;
  });
  mock("ide_create_folder", ({ dir: d, name }) => {
    const path = `${d}\\${checkedName(String(name))}`;
    if (fs.has(path)) throw new Error(`${name} is already there.`);
    fs.set(path, { dir: true });
    return path;
  });
  mock("ide_rename", ({ path, name }) => {
    const from = String(path);
    const to = `${parentOf(from)}\\${checkedName(String(name))}`;
    if (to === from) return node(from);
    if (fs.has(to) && to.toLowerCase() !== from.toLowerCase()) throw new Error(`${name} is already there.`);
    for (const [p, e] of [...fs.entries()]) {
      if (p === from || p.startsWith(from + "\\")) {
        fs.delete(p);
        fs.set(to + p.slice(from.length), e);
      }
    }
    return node(to);
  });
  mock("ide_delete", ({ path }) => {
    const target = String(path);
    for (const p of [...fs.keys()]) if (p === target || p.startsWith(target + "\\")) fs.delete(p);
  });
}
