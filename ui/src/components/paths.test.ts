import { describe, expect, it } from "vitest";
import { baseName, repoName } from "./paths";

describe("paths", () => {
  it("a path's last part", () => {
    expect(baseName("C:\\work\\app")).toBe("app");
    expect(baseName("C:\\work\\app\\")).toBe("app");
    expect(baseName("C:\\work/app.ts")).toBe("app.ts");
    expect(baseName("/home/me/app")).toBe("app");
    expect(baseName("C:\\")).toBe("C:\\");
    expect(baseName("app")).toBe("app");
  });

  it("a folder's name, as Path.fileName gives it", () => {
    expect(repoName("C:\\Users\\you\\Projects\\nook-site")).toBe("nook-site");
    expect(repoName("C:\\src\\nook\\")).toBe("nook");
    // A drive root has no file name: the path itself shows.
    expect(repoName("C:\\")).toBe("C:\\");
    expect(repoName("D:")).toBe("D:");
    expect(repoName("")).toBe("");
    expect(repoName("   ")).toBe("");
    expect(repoName(null)).toBe("");
    expect(repoName(undefined)).toBe("");
  });
});
