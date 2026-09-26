/** Ports the UI half of IdeSupportTest.kt: highlighting by file name and the explorer's rows. */
import { EditorSelection, EditorState } from "@codemirror/state";
import { describe, expect, it } from "vitest";
import type { FileNode } from "../../api/ide";
import { baseName } from "../../components/paths";
import {
  isUnder,
  languageFor,
  nameProblem,
  parentDir,
  relativeName,
  selectedLines,
  selectedText,
  treeRows,
} from "./IdeSupport";

describe("IdeSupport", () => {
  it("highlighting follows the file name", () => {
    expect(languageFor("Foo.kt")).toMatchObject({ name: "Kotlin", mode: "Kotlin" });
    expect(languageFor("build.gradle.KTS").name).toBe("Kotlin");
    expect(languageFor("build.gradle").name).toBe("Groovy");
    expect(languageFor("app.tsx")).toMatchObject({ name: "TypeScript", mode: "TSX" });
    expect(languageFor("Makefile")).toMatchObject({ name: "Makefile", tabs: true });
    expect(languageFor("main.go").tabs).toBe(true);
    expect(languageFor("main.rs").tabs).toBe(false);
    expect(languageFor("Dockerfile").name).toBe("Dockerfile");
    expect(languageFor(".editorconfig").name).toBe("INI");
    expect(languageFor("LICENSE")).toMatchObject({ name: "Plain text", mode: null });
    expect(languageFor("notes.unknownext").name).toBe("Plain text");
  });

  it("rows follow the open folders", () => {
    const root = "root";
    const src = "root\\src";
    const deep = "root\\src\\deep";
    const n = (path: string, isDirectory: boolean): FileNode => ({ path, name: baseName(path), isDirectory });
    const children = new Map<string, FileNode[]>([
      [root, [n(src, true), n("root\\README.md", false)]],
      [src, [n(deep, true), n("root\\src\\Main.kt", false)]],
      [deep, [n("root\\src\\deep\\x.kt", false)]],
    ]);
    const shape = (open: string[]) => treeRows(root, new Set(open), children).map((r) => [r.node.name, r.depth]);
    expect(shape([])).toEqual([
      ["src", 0],
      ["README.md", 0],
    ]);
    expect(shape([src])).toEqual([
      ["src", 0],
      ["deep", 1],
      ["Main.kt", 1],
      ["README.md", 0],
    ]);
    // A folder open in memory but under a closed one stays hidden.
    expect(shape([deep]).map((r) => r[0])).toEqual(["src", "README.md"]);
  });

  it("paths inside the folder read plainly", () => {
    const folder = "C:\\work\\app";
    expect(relativeName(folder, "C:\\work\\app\\src\\main.ts")).toBe("src/main.ts");
    expect(relativeName(folder, "C:\\elsewhere\\x.ts")).toBe("C:/elsewhere/x.ts");
    expect(isUnder("C:\\work\\app\\a", folder)).toBe(true);
    expect(isUnder("C:\\work\\apple", folder)).toBe(false);
    expect(isUnder("C:\\x", "C:\\")).toBe(true);
    expect(parentDir("C:\\work\\app\\a.ts")).toBe("C:\\work\\app");
    expect(parentDir("C:\\a.ts")).toBe("C:\\");
  });

  it("names are checked before a change", () => {
    expect(nameProblem("main.rs")).toBeNull();
    for (const bad of ["", " ", ".", "..", "a/b", "a\\b", "c:x"]) expect(nameProblem(bad)).toBe(`"${bad}" is not a file name.`);
  });

  it("the selection says its lines", () => {
    const doc = "one\ntwo\nthree\n";
    const at = (anchor: number, head: number) => EditorState.create({ doc, selection: EditorSelection.single(anchor, head) });
    expect(selectedLines(at(0, 0))).toBeNull();
    expect(selectedLines(at(1, 6))).toEqual([1, 2]);
    // A selection that ends at the start of a line does not hold that line.
    expect(selectedLines(at(0, 8))).toEqual([1, 2]);
    expect(selectedLines(at(4, 4 + 3))).toEqual([2, 2]);
    expect(selectedText(at(0, 8))).toBe("one\ntwo");
    expect(selectedText(at(3, 4))).toBeNull();
  });
});
