import { describe, it, expect, vi, afterEach } from "vitest";
import { render, screen, fireEvent, waitFor, cleanup } from "@testing-library/react";
import { UberAccountsPanel } from "./UberAccountsPanel";
import { api } from "../api/PondApiClient";
import { openExternal } from "../api/followSignIn";

vi.mock("../api/PondApiClient", () => ({
  api: {
    listProfiles: vi.fn(),
    uberAccounts: vi.fn(),
    connectUber: vi.fn(),
    disconnectUber: vi.fn(),
    getOAuthStatus: vi.fn(),
  },
}));

vi.mock("../api/followSignIn", async (importActual) => ({
  ...(await importActual<typeof import("../api/followSignIn")>()),
  openExternal: vi.fn(),
  // Settle on the first check, so the test does not wait out the polling interval.
  followSignIn: (state: string, { getStatus }: { getStatus: (s: string) => Promise<{ status: string; error?: string }> }) => ({
    stop: () => {},
    done: getStatus(state).then((s) =>
      s.status === "completed" ? { outcome: "completed" } : { outcome: "failed", error: s.error },
    ),
  }),
}));

const m = vi.mocked(api);

function household(connected: string[]) {
  m.listProfiles.mockResolvedValue({
    profiles: [
      { id: "p-liz", display_name: "Liz", avatar_emoji: "*" },
      { id: "p-jerry", display_name: "Jerry", avatar_emoji: "*" },
    ],
  });
  m.uberAccounts.mockResolvedValue({ connected });
}

describe("UberAccountsPanel", () => {
  afterEach(() => {
    cleanup();
    vi.clearAllMocks();
  });

  it("shows each member with their own Uber status", async () => {
    household(["p-liz"]);
    render(<UberAccountsPanel />);
    await screen.findByText("Liz");
    expect(screen.getByText("Connected")).toBeTruthy();
    expect(screen.getByText("Not connected")).toBeTruthy();
    expect(screen.getByRole("button", { name: "Disconnect" })).toBeTruthy();
    expect(screen.getByRole("button", { name: "Connect Uber" })).toBeTruthy();
  });

  it("connects a member: opens Uber's page, follows the sign-in, then shows them connected", async () => {
    household([]);
    m.connectUber.mockResolvedValue({ auth_url: "https://auth.uber.com/oauth/v2/authorize?x", state: "n-1" });
    m.getOAuthStatus.mockResolvedValue({ status: "completed" });
    render(<UberAccountsPanel />);
    const buttons = await screen.findAllByRole("button", { name: "Connect Uber" });

    m.uberAccounts.mockResolvedValue({ connected: ["p-jerry"] });
    fireEvent.click(buttons[1]);

    await waitFor(() => expect(screen.getByText("Connected")).toBeTruthy());
    expect(m.connectUber).toHaveBeenCalledWith("p-jerry");
    expect(vi.mocked(openExternal)).toHaveBeenCalledWith("https://auth.uber.com/oauth/v2/authorize?x");
    expect(m.getOAuthStatus).toHaveBeenCalledWith("n-1");
  });

  it("says why when Uber does not connect", async () => {
    household([]);
    m.connectUber.mockResolvedValue({ auth_url: "https://auth.uber.com/x", state: "n-2" });
    m.getOAuthStatus.mockResolvedValue({ status: "failed", error: "Uber sign-in failed: invalid_grant" });
    render(<UberAccountsPanel />);
    fireEvent.click((await screen.findAllByRole("button", { name: "Connect Uber" }))[0]);
    expect((await screen.findByRole("alert")).textContent).toContain("invalid_grant");
  });

  it("disconnects only the chosen member", async () => {
    household(["p-liz", "p-jerry"]);
    m.disconnectUber.mockResolvedValue(undefined);
    render(<UberAccountsPanel />);
    const buttons = await screen.findAllByRole("button", { name: "Disconnect" });
    m.uberAccounts.mockResolvedValue({ connected: ["p-jerry"] });
    fireEvent.click(buttons[0]);
    await waitFor(() => expect(m.disconnectUber).toHaveBeenCalledWith("p-liz"));
    await waitFor(() => expect(screen.getAllByText("Connected")).toHaveLength(1));
  });

  it("shows the server's reason when Uber connections cannot be read", async () => {
    m.listProfiles.mockResolvedValue({ profiles: [] });
    m.uberAccounts.mockRejectedValue(new Error("host_credential_required"));
    render(<UberAccountsPanel />);
    expect((await screen.findByRole("alert")).textContent).toContain("host_credential_required");
  });
});
