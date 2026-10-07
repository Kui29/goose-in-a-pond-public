// Following one browser sign-in to its end, and opening its page outside the app.

import { invoke, isDesktopShell } from "../shell";
import type { OAuthFlowStatus } from "./types";

/** Generous: the person may have to log in and pick an account. */
export const SIGN_IN_TIMEOUT_MS = 5 * 60 * 1000;
const POLL_INTERVAL_MS = 2000;

export type SignInOutcome =
  | { outcome: "completed" }
  | { outcome: "failed"; error?: string }
  | { outcome: "timed_out" }
  | { outcome: "stopped" };

export interface FollowOptions {
  getStatus: (state: string) => Promise<OAuthFlowStatus>;
  timeoutMs?: number;
  intervalMs?: number;
  now?: () => number;
}

/**
 * Follow THIS sign-in by its `state` nonce. Watching the stored token instead would report success
 * on a re-authorisation before the person has signed in, since the old token is still there.
 */
export function followSignIn(
  state: string,
  { getStatus, timeoutMs = SIGN_IN_TIMEOUT_MS, intervalMs = POLL_INTERVAL_MS, now = Date.now }: FollowOptions,
): { done: Promise<SignInOutcome>; stop: () => void } {
  let timer: ReturnType<typeof setInterval> | null = null;
  let settle: (o: SignInOutcome) => void = () => {};
  const done = new Promise<SignInOutcome>((resolve) => {
    settle = (o) => {
      if (timer !== null) clearInterval(timer);
      timer = null;
      resolve(o);
    };
  });
  const startedAt = now();
  timer = setInterval(async () => {
    try {
      const { status, error } = await getStatus(state);
      if (status === "completed") return settle({ outcome: "completed" });
      if (status === "failed") return settle({ outcome: "failed", error });
    } catch {
      // A transient check failure: keep waiting.
    }
    // "pending" and "unknown" both mean keep waiting: a server restarted mid-flow says "unknown".
    if (now() - startedAt > timeoutMs) settle({ outcome: "timed_out" });
  }, intervalMs);
  return { done, stop: () => settle({ outcome: "stopped" }) };
}

/** Open a URL in the real browser. In the shell it must go via the main process:
 *  `window.open` on an app:// page opens another in-app window. */
export async function openExternal(url: string) {
  if (isDesktopShell()) {
    await invoke("open_external", { url });
    return;
  }
  window.open(url, "_blank", "noopener,noreferrer");
}
