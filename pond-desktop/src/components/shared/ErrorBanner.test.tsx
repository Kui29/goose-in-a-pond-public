import { describe, it, expect, afterEach } from "vitest";
import { render, screen, cleanup } from "@testing-library/react";
import { ErrorBanner } from "./ErrorBanner";

afterEach(cleanup);

describe("ErrorBanner", () => {
  it("says setup is unfinished rather than that permission is missing", () => {
    render(<ErrorBanner error="onboarding_required" />);
    expect(screen.getByText(/Setup isn't finished/)).toBeTruthy();
    expect(screen.queryByText(/permission/)).toBeNull();
  });

  it("still reads any other 403 as a permission problem", () => {
    render(<ErrorBanner error="Forbidden" />);
    expect(screen.getByText(/permission/)).toBeTruthy();
  });
});
