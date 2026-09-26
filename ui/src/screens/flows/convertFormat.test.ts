import { describe, expect, it } from "vitest";
import type { FileInfo, Job, Offer, Target } from "../../api/convert";
import { filesText, goText, groupTargets, jobStatusText, jobTitle, needsText, pickTarget } from "./convertFormat";

const target = (id: string, kind: Target["kind"], by = "Pandoc"): Target => ({ id, name: id.toUpperCase(), kind, by, needs: [], missing: null });
const file = (name: string, kind: FileInfo["kind"]): FileInfo => ({ path: `C:\\d\\${name}`, name, format: null, formatName: null, kind });
const offer = (targets: Target[]): Offer => ({ files: [], targets, combine: false, note: null });

describe("groupTargets", () => {
  it("groups in the picker's order and leaves empty groups out", () => {
    const groups = groupTargets([target("md", "TEXT"), target("pdf", "PDF"), target("docx", "DOCUMENT"), target("txt", "TEXT")]);
    expect(groups.map((g) => g.label)).toEqual(["PDF", "Documents", "Text"]);
    expect(groups[2].targets.map((t) => t.id)).toEqual(["md", "txt"]);
  });
});

describe("pickTarget", () => {
  const o = offer([target("pdf", "PDF"), target("md", "TEXT")]);
  it("keeps the chosen target while it is offered", () => {
    expect(pickTarget(o, "md", "pdf")).toBe("md");
  });
  it("falls back to the one asked for", () => {
    expect(pickTarget(o, "xlsx", "pdf")).toBe("pdf");
    expect(pickTarget(o, null, "pdf")).toBe("pdf");
  });
  it("chooses nothing when neither is offered", () => {
    expect(pickTarget(o, "xlsx", "png")).toBeNull();
    expect(pickTarget(null, "pdf", null)).toBeNull();
  });
});

describe("wording", () => {
  it("names the downloads", () => {
    expect(needsText([{ engine: "PANDOC", what: "the document engine (Pandoc)", bytes: 1 }])).toBe(
      "One-time download: the document engine (Pandoc). After that it converts without the internet.",
    );
    expect(
      needsText([
        { engine: "PANDOC", what: "Pandoc", bytes: 1 },
        { engine: "PDF", what: "PDFium", bytes: 1 },
      ]),
    ).toContain("Pandoc and PDFium.");
  });

  it("says what becomes what", () => {
    expect(filesText([file("a.docx", "DOCUMENT")])).toBe("a.docx");
    expect(filesText([file("a.png", "IMAGE"), file("b.jpg", "IMAGE")])).toBe("2 pictures");
    expect(filesText([file("a.png", "IMAGE"), file("b.md", "TEXT")])).toBe("2 files");
    const pdf = target("pdf", "PDF", "PDFium");
    expect(goText([file("a.png", "IMAGE"), file("b.png", "IMAGE")], pdf, true)).toBe("2 pictures into one PDF · by PDFium");
    expect(goText([file("a.png", "IMAGE"), file("b.png", "IMAGE")], pdf, false)).toBe("2 pictures into PDF · by PDFium");
    expect(goText([file("a.png", "IMAGE")], null, false)).toBe("Pick what they should become.");
  });

  it("tells a job's progress", () => {
    const job: Job = {
      id: "cv_1",
      to: "pdf",
      toName: "PDF",
      at: 0,
      status: "CONVERTING",
      items: [
        { input: "a", name: "a.docx", status: "DONE", outputs: ["a.pdf"], error: null },
        { input: "b", name: "b.docx", status: "CONVERTING", outputs: [], error: null },
      ],
    };
    expect(jobTitle(job)).toBe("2 files into PDF");
    expect(jobStatusText(job)).toBe("Converting · 1 of 2");
    expect(jobStatusText({ ...job, status: "DONE" })).toBe("2 converted");
    expect(jobStatusText({ ...job, status: "STOPPED" })).toBe("Stopped · 1 of 2 converted");
    expect(jobTitle({ ...job, items: [job.items[0]] })).toBe("a.docx into PDF");
  });
});
