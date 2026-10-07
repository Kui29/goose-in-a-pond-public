// ─── Each member's own Uber account ─────────────────────────────────────────
// Sign-in happens in the browser on Uber's page; the pond keeps the result for that member only.

import { useCallback, useEffect, useRef, useState } from "react";
import { Car } from "lucide-react";
import { api } from "../api/PondApiClient";
import { followSignIn, openExternal } from "../api/followSignIn";
import "../styles/connections.css";

interface Member {
  id: string;
  display_name: string;
}

export function UberAccountsPanel() {
  const [members, setMembers] = useState<Member[]>([]);
  const [connected, setConnected] = useState<Set<string>>(new Set());
  const [loading, setLoading] = useState(true);
  /** The member whose sign-in or disconnect is in progress. */
  const [busyFor, setBusyFor] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const flowRef = useRef<{ stop: () => void } | null>(null);

  const refresh = useCallback(async () => {
    try {
      const [{ profiles }, { connected: ids }] = await Promise.all([
        api.listProfiles(),
        api.uberAccounts(),
      ]);
      setMembers(profiles);
      setConnected(new Set(ids));
    } catch (e) {
      setError(e instanceof Error ? e.message : "Could not read Uber connections.");
    } finally {
      setLoading(false);
    }
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
      void openExternal(auth_url);
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
      setError(e instanceof Error ? e.message : String(e));
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
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      setBusyFor(null);
    }
  }

  return (
    <section className="connections" aria-labelledby="uber-accounts-title">
      <div className="connections-intro">
        <h3 id="uber-accounts-title">Uber</h3>
        <p>
          Each person connects their own Uber account, so rides they ask for are booked and paid on
          it. Nothing is booked until they confirm on their phone.
        </p>
      </div>

      {error && (
        <p className="connections-detail" role="alert">
          {error}
        </p>
      )}

      {loading ? (
        <p className="connections-muted">Reading Uber connections…</p>
      ) : members.length === 0 ? (
        <p className="connections-muted">Add a household member first.</p>
      ) : (
        <ul className="connections-list">
          {members.map((member) => {
            const isConnected = connected.has(member.id);
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
                      {isConnected ? "Connected" : "Not connected"}
                    </span>
                  </div>
                  {busy && !isConnected && (
                    <p className="connections-muted">Finish signing in to Uber in your browser…</p>
                  )}
                </div>
                <button
                  type="button"
                  className={isConnected ? "connections-disconnect" : "connections-submit"}
                  disabled={busyFor !== null}
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
