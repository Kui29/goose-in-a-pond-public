import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { PondApiClient } from "./PondApiClient";

const credential = "a".repeat(43);
let fetchMock: ReturnType<typeof vi.fn>;

beforeEach(() => {
  fetchMock = vi.fn(async () => new Response("{}", { status: 200, headers: { "Content-Type": "application/json" } }));
  vi.stubGlobal("fetch", fetchMock);
  sessionStorage.setItem("giap-host-credential", credential);
});

afterEach(() => {
  vi.unstubAllGlobals();
  sessionStorage.clear();
  localStorage.clear();
});

const client = () => new PondApiClient("http://localhost:4000");

// On a pond with several members the dashboard's own pairing belongs to nobody, so these calls
// pass the role check only on the host credential.
const ownerCalls: Array<[string, string, string, (c: PondApiClient) => Promise<unknown>]> = [
  ["updateSettings", "PUT", "/api/v1/settings", (c) => c.updateSettings({})],
  ["unregisterDevice", "DELETE", "/api/v1/devices/phone-1", (c) => c.unregisterDevice("phone-1")],
  ["createSchedule", "POST", "/api/v1/schedules", (c) => c.createSchedule({ name: "Morning", cron: "30 6 * * *", prompt: "lights on", enabled: true })],
  ["updateSchedule", "PUT", "/api/v1/schedules/s-1", (c) => c.updateSchedule("s-1", { name: "Morning" })],
  ["deleteSchedule", "DELETE", "/api/v1/schedules/s-1", (c) => c.deleteSchedule("s-1")],
  ["pauseSchedule", "POST", "/api/v1/schedules/s-1/pause", (c) => c.pauseSchedule("s-1")],
  ["resumeSchedule", "POST", "/api/v1/schedules/s-1/resume", (c) => c.resumeSchedule("s-1")],
  ["runScheduleNow", "POST", "/api/v1/schedules/s-1/run-now", (c) => c.runScheduleNow("s-1")],
  ["activateModel", "POST", "/api/v1/models/local/qwen/activate", (c) => c.activateModel("local", "qwen", "chat")],
  ["createProfile", "POST", "/api/v1/profiles", (c) => c.createProfile("Grace")],
  ["updateProfilePrefs", "PATCH", "/api/v1/profiles/p-1", (c) => c.updateProfilePrefs("p-1", { language: "sw" })],
];

describe("owner-only household calls", () => {
  it.each(ownerCalls)("%s sends %s %s with the host credential", async (_name, method, path, call) => {
    await call(client());
    const [url, init] = fetchMock.mock.calls[0];
    expect(String(url)).toBe(`http://localhost:4000${path}`);
    expect(init.method).toBe(method);
    expect((init.headers as Record<string, string>)["X-Pond-Host-Credential"]).toBe(credential);
  });

  it("sends no credential header when the dashboard has none", async () => {
    sessionStorage.clear();
    await client().updateSettings({});
    const init = fetchMock.mock.calls[0][1];
    expect((init.headers as Record<string, string>)["X-Pond-Host-Credential"]).toBeUndefined();
  });
});
