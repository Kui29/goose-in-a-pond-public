/** The Accounts screen's Uber panel, in a real browser: each member's status, connect, disconnect. */
import { test, expect } from "@playwright/test";
import type { Page } from "@playwright/test";
import { mockAllApiRoutes } from "./helpers/api-mocks";
import { navigateTo } from "./helpers/nav";

async function setup(page: Page, connected: string[]) {
  await mockAllApiRoutes(page);
  const state = { connected: [...connected], connectBody: null as unknown, deleted: [] as string[] };

  await page.route("**/api/v1/context/sources**", (r) => r.fulfill({ json: { sources: [] } }));
  await page.route("**/api/v1/profiles", (r) =>
    r.fulfill({
      json: {
        profiles: [
          { id: "p-liz", display_name: "Liz", avatar_emoji: "*" },
          { id: "p-jerry", display_name: "Jerry", avatar_emoji: "*" },
        ],
      },
    }),
  );
  await page.route("**/api/v1/uber/accounts", (r) => r.fulfill({ json: { connected: state.connected } }));
  await page.route("**/api/v1/uber/accounts/connect", (r) => {
    state.connectBody = r.request().postDataJSON();
    state.connected.push("p-jerry");
    return r.fulfill({ json: { auth_url: "https://auth.uber.com/oauth/v2/authorize?x", state: "n-1" } });
  });
  await page.route("**/api/v1/oauth/status/n-1", (r) => r.fulfill({ json: { status: "completed" } }));
  await page.route("**/api/v1/uber/accounts/p-*", (r) => {
    state.deleted.push(r.request().url().split("/").pop()!);
    state.connected = state.connected.filter((id) => !r.request().url().endsWith(id));
    return r.fulfill({ status: 204, body: "" });
  });

  await page.addInitScript(() => {
    localStorage.setItem("giap-section", "hub");
    localStorage.setItem("giap-force-hub", "1");
    localStorage.setItem("goosehub_route", "home");
  });
  await page.setViewportSize({ width: 1440, height: 900 });
  await page.goto("/");
  await page.waitForSelector(".ghub", { timeout: 10_000 });
  await navigateTo(page, "Settings");
  await page.waitForSelector(".set", { timeout: 5_000 });
  await page.getByRole("button", { name: /^Accounts$/ }).first().click();
  return state;
}

test.describe("Hub — Accounts: Uber", () => {
  test("shows each member's own Uber status", async ({ page }) => {
    await setup(page, ["p-liz"]);
    const panel = page.locator("section", { has: page.getByRole("heading", { name: "Uber" }) });
    await expect(panel.getByText("Liz")).toBeVisible({ timeout: 5_000 });
    await expect(panel.getByText("Connected", { exact: true })).toBeVisible();
    await expect(panel.getByText("Not connected")).toBeVisible();
  });

  test("connecting a member opens Uber and then shows them connected", async ({ page }) => {
    const state = await setup(page, []);
    const panel = page.locator("section", { has: page.getByRole("heading", { name: "Uber" }) });
    const opened = page.waitForEvent("popup");
    await panel.getByRole("button", { name: "Connect Uber" }).nth(1).click();
    expect((await opened).url()).toContain("auth.uber.com");
    await expect(panel.getByText("Connected", { exact: true })).toBeVisible({ timeout: 10_000 });
    expect(state.connectBody).toEqual({ profile_id: "p-jerry" });
  });

  test("disconnecting forgets only that member", async ({ page }) => {
    const state = await setup(page, ["p-liz", "p-jerry"]);
    const panel = page.locator("section", { has: page.getByRole("heading", { name: "Uber" }) });
    await panel.getByRole("button", { name: "Disconnect" }).first().click();
    await expect(panel.getByRole("button", { name: "Disconnect" })).toHaveCount(1, { timeout: 5_000 });
    expect(state.deleted).toEqual(["p-liz"]);
  });
});
