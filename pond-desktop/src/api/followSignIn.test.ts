import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { followSignIn } from "./followSignIn";
import type { OAuthFlowStatus } from "./types";

describe("followSignIn", () => {
  beforeEach(() => vi.useFakeTimers());
  afterEach(() => vi.useRealTimers());

  function statuses(...seq: Array<OAuthFlowStatus | Error>) {
    const calls: string[] = [];
    const getStatus = vi.fn(async (state: string) => {
      calls.push(state);
      const next = seq.length > 1 ? seq.shift()! : seq[0];
      if (next instanceof Error) throw next;
      return next;
    });
    return { getStatus, calls };
  }

  it("waits through pending and unknown, then reports completion", async () => {
    const { getStatus, calls } = statuses({ status: "pending" }, { status: "unknown" }, { status: "completed" });
    const flow = followSignIn("nonce-1", { getStatus, intervalMs: 10 });
    await vi.advanceTimersByTimeAsync(35);
    await expect(flow.done).resolves.toEqual({ outcome: "completed" });
    expect(new Set(calls)).toEqual(new Set(["nonce-1"]));
  });

  it("passes the server's reason on a failure", async () => {
    const { getStatus } = statuses({ status: "failed", error: "invalid_grant" });
    const flow = followSignIn("n", { getStatus, intervalMs: 10 });
    await vi.advanceTimersByTimeAsync(15);
    await expect(flow.done).resolves.toEqual({ outcome: "failed", error: "invalid_grant" });
  });

  it("keeps waiting through a failed check and gives up at the timeout", async () => {
    let clock = 0;
    const { getStatus } = statuses(new Error("offline"), { status: "pending" });
    const flow = followSignIn("n", { getStatus, intervalMs: 10, timeoutMs: 100, now: () => clock });
    clock = 50;
    await vi.advanceTimersByTimeAsync(20);
    clock = 150;
    await vi.advanceTimersByTimeAsync(10);
    await expect(flow.done).resolves.toEqual({ outcome: "timed_out" });
  });

  it("stops checking once stopped", async () => {
    const { getStatus } = statuses({ status: "pending" });
    const flow = followSignIn("n", { getStatus, intervalMs: 10 });
    flow.stop();
    await expect(flow.done).resolves.toEqual({ outcome: "stopped" });
    const before = getStatus.mock.calls.length;
    await vi.advanceTimersByTimeAsync(50);
    expect(getStatus.mock.calls.length).toBe(before);
  });
});
