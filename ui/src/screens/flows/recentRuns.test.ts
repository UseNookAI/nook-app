import { describe, expect, it } from "vitest";
import type { Job } from "../../api/convert";
import type { Run } from "../../api/flows";
import { recentRuns } from "./NookletsSidebar";

const run = (id: string, flow: string, createdAt: number, status: Run["status"], inputName = `${id}.mp3`) =>
  ({ id, flow, createdAt, status, inputName }) as Run;
const job = (id: string, at: number, status: Job["status"], names: string[]) =>
  ({ id, at, status, to: "pdf", toName: "PDF", items: names.map((name) => ({ input: name, name, status, outputs: [], error: null })) }) as Job;

describe("recentRuns", () => {
  it("lists flow runs and conversions together, newest first, each under its Nooklet", () => {
    const list = recentRuns(
      [run("a", "transcribe", 10, "DONE"), run("b", "translate-audio", 30, "RUNNING"), run("c", "summarize", 20, "FAILED", "contract.pdf")],
      [job("j", 25, "DONE", ["one.docx", "two.docx", "three.docx"])],
    );
    expect(list.map((r) => [r.id, r.kind])).toEqual([
      ["b", "TRANSLATE"],
      ["j", "CONVERT"],
      ["c", "SUMMARIZE"],
      ["a", "TRANSCRIBE"],
    ]);
    expect(list[0].going).toBe(true);
    expect(list[1].name).toBe("one.docx and 2 more");
    expect(list[2]).toMatchObject({ name: "contract.pdf", failed: true, going: false });
  });

  it("leaves out flows it does not know and keeps to the limit", () => {
    const runs = Array.from({ length: 20 }, (_, i) => run(`r${i}`, "read-aloud", i, "DONE"));
    const list = recentRuns([...runs, run("x", "something-new", 999, "DONE")], [], 5);
    expect(list.map((r) => r.id)).toEqual(["r19", "r18", "r17", "r16", "r15"]);
  });
});
