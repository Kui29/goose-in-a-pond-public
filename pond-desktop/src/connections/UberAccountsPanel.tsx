// ─── Each member's own Uber account ─────────────────────────────────────────
// Sign-in happens in the browser on Uber's page; the pond keeps the result for that member only.

import { useCallback, useEffect, useRef, useState } from "react";
import { Car, RefreshCw } from "lucide-react";
import { api } from "../api/PondApiClient";
import { ApiError } from "../api/types";
import { followSignIn, openExternal } from "../api/followSignIn";
import "../styles/connections.css";

interface Member {
  id: string;
  display_name: string;
}

type Action = "read" | "connect" | "disconnect";

const HOST_ONLY =
  "Uber accounts can only be changed on the pond itself. Use the Goose In A Pond app there, or run " +
  "`pond-server dashboard` on the pond and open the link it prints in this browser.";
const OLD_SERVER =
  "This pond's server does not know about Uber accounts yet. Update the pond, then open this screen again.";
const SIGN_IN_OFF =
  "Uber sign-in is turned off on this pond: it needs Jarida's credentials service, which has been " +
  "switched off (POND_CREDENTIALS_URL). Turn it back on to connect anyone.";
const NO_STORE = "This pond has no secret store, so it has nowhere to keep Uber sign-ins.";
const UNAVAILABLE = "The pond could not reach its Uber sign-ins just now. Try again in a moment.";
const RELAY_DOWN =
  "Jarida's credentials service did not answer, so the Uber sign-in could not start. Try again in a few minutes.";
const NOT_A_MEMBER = "That person is no longer in this household.";
const MEMBERS_UNREAD = "Could not read who is in the household. Try again in a moment.";
const BROWSER_FAILED =
  "Could not open your browser for Uber's sign-in page. Check that this computer has a default browser, then try again.";
const UNKNOWN = "Something went wrong talking to the pond. Try again.";

/** A bare machine code, which is never shown to a person. */
const RAW_CODE = /^[a-z][a-z0-9_]*$/;

/** What went wrong and what to do about it, in words; never a raw server code. */
function describeProblem(error: unknown, action: Action): string {
  if (error instanceof ApiError) {
    switch (error.status) {
      case 403:
        return HOST_ONLY;
      case 404:
        return action === "connect" ? NOT_A_MEMBER : OLD_SERVER;
      case 405:
        return OLD_SERVER;
      case 502:
        return RELAY_DOWN;
      case 503:
        if (error.code === "uber_sign_in_off") return SIGN_IN_OFF;
        if (error.code === "no_secret_store") return NO_STORE;
        // An older pond sends no code, but its sentence says which.
        return error.code || RAW_CODE.test(error.message) ? UNAVAILABLE : error.message;
    }
  }
  if (error instanceof Error && error.message && !RAW_CODE.test(error.message)) return error.message;
  return UNKNOWN;
}

export function UberAccountsPanel() {
  /** `null` until the household has been read. */
  const [members, setMembers] = useState<Member[] | null>(null);
  /** `null` when the pond could not say who is connected: each member's status is then unknown. */
  const [connected, setConnected] = useState<Set<string> | null>(null);
  const [loading, setLoading] = useState(true);
  /** The member whose sign-in or disconnect is in progress. */
  const [busyFor, setBusyFor] = useState<string | null>(null);
  /** Why the last read came back incomplete. */
  const [readProblem, setReadProblem] = useState<string | null>(null);
  /** Why the last connect or disconnect failed. */
  const [error, setError] = useState<string | null>(null);
  const flowRef = useRef<{ stop: () => void } | null>(null);

  // Settled apart, so a refusal on the Uber side still shows who is in the household.
  const refresh = useCallback(async () => {
    const [people, accounts] = await Promise.allSettled([api.listProfiles(), api.uberAccounts()]);
    let problem: string | null = null;
    if (people.status === "fulfilled") {
      setMembers(Array.isArray(people.value?.profiles) ? people.value.profiles : []);
    } else {
      problem = MEMBERS_UNREAD;
    }
    if (accounts.status === "fulfilled" && Array.isArray(accounts.value?.connected)) {
      setConnected(new Set(accounts.value.connected));
    } else {
      setConnected(null);
      // A fulfilled reply without a list is an older pond answering with its web page.
      problem ??= accounts.status === "rejected" ? describeProblem(accounts.reason, "read") : OLD_SERVER;
    }
    setReadProblem(problem);
    setLoading(false);
  }, []);

  useEffect(() => {
    void refresh();
    return () => flowRef.current?.stop();
  }, [refresh]);

  async function connect(member: Member) {
    setBusyFor(member.id);
    setError(null);
    try {
      const { auth_url, state } = await api.connectUber(member.id);
      try {
        await openExternal(auth_url);
      } catch {
        setError(BROWSER_FAILED);
        return;
      }
      const flow = followSignIn(state, { getStatus: (s) => api.getOAuthStatus(s) });
      flowRef.current = flow;
      const result = await flow.done;
      if (result.outcome === "failed") {
        setError(result.error || `Uber did not connect for ${member.display_name}.`);
      } else if (result.outcome === "timed_out") {
        setError("Timed out waiting for the Uber sign-in. Please try again.");
      }
      await refresh();
    } catch (e) {
      setError(describeProblem(e, "connect"));
    } finally {
      flowRef.current = null;
      setBusyFor(null);
    }
  }

  async function disconnect(member: Member) {
    setBusyFor(member.id);
    setError(null);
    try {
      await api.disconnectUber(member.id);
      await refresh();
    } catch (e) {
      setError(describeProblem(e, "disconnect"));
    } finally {
      setBusyFor(null);
    }
  }

  const problem = error ?? readProblem;

  return (
    <section className="connections" aria-labelledby="uber-accounts-title">
      <div className="connections-intro">
        <h3 id="uber-accounts-title">Uber</h3>
        <p>
          Each person connects their own Uber account, so rides they ask for are booked and paid on
          it. Nothing is booked until they confirm on their phone.
        </p>
      </div>

      {problem && (
        <p className="connections-detail" role="alert">
          {problem}
        </p>
      )}
      {!loading && connected === null && (
        <div>
          <button
            type="button"
            className="connections-sync"
            disabled={busyFor !== null}
            onClick={() => void refresh()}
          >
            <RefreshCw size={14} aria-hidden="true" />
            Try again
          </button>
        </div>
      )}

      {loading ? (
        <p className="connections-muted">Reading Uber connections…</p>
      ) : members === null ? null : members.length === 0 ? (
        <p className="connections-muted">Add a household member first.</p>
      ) : (
        <ul className="connections-list">
          {members.map((member) => {
            const known = connected !== null;
            const isConnected = connected?.has(member.id) ?? false;
            const busy = busyFor === member.id;
            return (
              <li key={member.id} className="connections-item">
                <span className="connections-icon" aria-hidden="true">
                  <Car size={18} />
                </span>
                <div className="connections-item-body">
                  <div className="connections-item-head">
                    <strong>{member.display_name}</strong>
                    <span
                      className={`connections-pill ${isConnected ? "connections-pill-ok" : "connections-pill-muted"}`}
                    >
                      {!known ? "Status unknown" : isConnected ? "Connected" : "Not connected"}
                    </span>
                  </div>
                  {busy && !isConnected && (
                    <p className="connections-muted">Finish signing in to Uber in your browser…</p>
                  )}
                </div>
                <button
                  type="button"
                  className={isConnected ? "connections-disconnect" : "connections-submit"}
                  disabled={busyFor !== null || !known}
                  onClick={() => void (isConnected ? disconnect(member) : connect(member))}
                >
                  {isConnected ? "Disconnect" : "Connect Uber"}
                </button>
              </li>
            );
          })}
        </ul>
      )}
    </section>
  );
}
