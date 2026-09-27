import { describe, expect, it } from "vitest";
import { inlineSpans, markdownBlocks } from "./MiniMarkdown";

describe("summary Markdown", () => {
  it("reads headings, paragraphs and lists", () => {
    const md = [
      "# Tenancy agreement",
      "",
      "A one-year lease",
      "for a flat.",
      "## Key points",
      "- Rent is **€900**.",
      "- Notice is",
      "  three months.",
      "1. First",
      "2. Second",
      "---",
      "Done.",
    ].join("\n");
    expect(markdownBlocks(md)).toEqual([
      { kind: "heading", level: 1, text: "Tenancy agreement" },
      { kind: "paragraph", text: "A one-year lease for a flat." },
      { kind: "heading", level: 2, text: "Key points" },
      { kind: "list", ordered: false, items: ["Rent is **€900**.", "Notice is three months."] },
      { kind: "list", ordered: true, items: ["First", "Second"] },
      { kind: "paragraph", text: "Done." },
    ]);
  });

  it("finds bold, italic and code, and leaves markup as text", () => {
    expect(inlineSpans("Rent **€900**, *due* on `day 1`.")).toEqual([
      { text: "Rent " },
      { text: "€900", bold: true },
      { text: ", " },
      { text: "due", italic: true },
      { text: " on " },
      { text: "day 1", code: true },
      { text: "." },
    ]);
    expect(inlineSpans("<img src=x onerror=alert(1)>")).toEqual([{ text: "<img src=x onerror=alert(1)>" }]);
    expect(inlineSpans("2 * 3 * 4 and snake_case_name")).toEqual([{ text: "2 * 3 * 4 and snake_case_name" }]);
  });
});
