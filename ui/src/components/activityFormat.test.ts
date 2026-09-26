import { describe, expect, it } from "vitest";
import { ago, compact, modelLabel } from "./activityFormat";

describe("activityFormat", () => {
  it("counts compactly", () => {
    expect(compact(1204)).toBe("1,204");
    expect(compact(9999)).toBe("9,999");
    expect(compact(48_700)).toBe("48k");
    expect(compact(1_240_000)).toBe("1.2M");
  });

  it("says how long ago", () => {
    const now = new Date(2026, 8, 25, 12, 0, 0);
    expect(ago(new Date(2026, 8, 25, 11, 59, 30), now)).toBe("just now");
    expect(ago(new Date(2026, 8, 25, 11, 15, 0), now)).toBe("45 min ago");
    expect(ago(new Date(2026, 8, 25, 7, 0, 0), now)).toBe("5 h ago");
    expect(ago(new Date(2026, 8, 22, 12, 0, 0), now)).toBe("3 d ago");
    expect(ago(new Date(2026, 8, 25, 12, 5, 0), now)).toBe("just now");
  });

  it("names models plainly", () => {
    expect(modelLabel("qwen3-8b-q4km")).toBe("Qwen3 8B");
    expect(modelLabel("qwen2.5-coder-14b-instruct-q4_k_m")).toBe("Qwen2.5 Coder 14B Instruct");
    expect(modelLabel("mistral-v0.3-7b-f16")).toBe("Mistral v0.3 7B");
    expect(modelLabel("nomic-embed-text-v1.5")).toBe("nomic-embed");
    expect(modelLabel("whisper-small-q5")).toBe("Whisper small");
    expect(modelLabel("whisper.base")).toBe("Whisper base");
    expect(modelLabel("z-image-turbo-q8")).toBe("Z-Image Turbo");
    expect(modelLabel("sdxl-turbo")).toBe("SDXL Turbo");
    expect(modelLabel("sd_turbo")).toBe("SD Turbo");
    expect(modelLabel(null)).toBe("—");
    expect(modelLabel("  ")).toBe("—");
  });
});
