import { describe, it, expect, vi, beforeEach, afterEach } from "vitest";
import { render, screen, fireEvent, cleanup, waitFor } from "@testing-library/react";

vi.mock("../../api/PondApiClient", () => ({
  api: {
    getSettings: vi.fn(),
    getOnboardingStatus: vi.fn(),
  },
}));

import { api } from "../../api/PondApiClient";
import { ApiError } from "../../api/types";
import { OnboardingWizard } from "./OnboardingWizard";
import { STEPS, beStepToFeIndex } from "./onboarding.constants";

const mockApi = api as unknown as {
  getSettings: ReturnType<typeof vi.fn>;
  getOnboardingStatus: ReturnType<typeof vi.fn>;
};

beforeEach(() => {
  mockApi.getSettings.mockResolvedValue({});
  mockApi.getOnboardingStatus.mockResolvedValue({ onboarded: false, current_step: "Extensions" });
});

afterEach(() => {
  cleanup();
  vi.clearAllMocks();
});

async function atTheLastStep() {
  await waitFor(() => expect(screen.getByText("You're all set!")).toBeTruthy());
}

describe("OnboardingWizard", () => {
  it("stays open and says what is missing when the pond refuses to finish", async () => {
    const onComplete = vi.fn().mockRejectedValue(
      new ApiError(400, "Onboarding is incomplete", undefined, { missing_fields: ["user_name"] }),
    );
    render(<OnboardingWizard onComplete={onComplete} />);
    await atTheLastStep();

    fireEvent.click(screen.getByText("Open dashboard"));

    await waitFor(() => expect(screen.getByRole("alert").textContent).toContain("your name"));
    expect(onComplete).toHaveBeenCalledTimes(1);
    expect(screen.getByText("You're all set!")).toBeTruthy();
  });

  it("finishes without complaint when the pond accepts", async () => {
    const onComplete = vi.fn().mockResolvedValue(undefined);
    render(<OnboardingWizard onComplete={onComplete} />);
    await atTheLastStep();

    fireEvent.click(screen.getByText("Open dashboard"));

    await waitFor(() => expect(onComplete).toHaveBeenCalledTimes(1));
    expect(screen.queryByRole("alert")).toBeNull();
  });

  it("resumes every backend step at a step the wizard has", () => {
    for (const step of [
      "Welcome", "Basics", "Accessibility", "Location", "Personality",
      "GooseIdentity", "WakeWord", "Model", "Extensions", "Completed",
    ]) {
      expect(beStepToFeIndex(step), step).toBeLessThan(STEPS.length);
    }
  });
});
