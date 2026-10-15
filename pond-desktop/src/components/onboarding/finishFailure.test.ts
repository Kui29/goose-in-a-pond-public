import { describe, it, expect } from "vitest";
import { ApiError } from "../../api/types";
import { finishFailure } from "./finishFailure";

describe("finishFailure", () => {
  it("names what the pond says is missing", () => {
    const refusal = new ApiError(400, "Onboarding is incomplete", undefined, {
      missing_fields: ["user_name", "timezone"],
    });
    expect(finishFailure(refusal)).toBe(
      "Setup can't finish yet: your name, your time zone is missing. Go back and fill it in.",
    );
  });

  it("passes an unknown field through by its name", () => {
    const refusal = new ApiError(400, "x", undefined, { missing_fields: ["pond_colour"] });
    expect(finishFailure(refusal)).toContain("pond_colour");
  });

  it("falls back when the refusal names nothing", () => {
    expect(finishFailure(new TypeError("Failed to fetch"))).toBe(
      "Setup couldn't be finished. Check that the pond is running and try again.",
    );
    expect(finishFailure(new ApiError(500, "boom"))).toContain("couldn't be finished");
  });
});
