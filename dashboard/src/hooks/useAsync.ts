/* eslint-disable react-hooks/exhaustive-deps --
 * This hook takes its dependency list as a runtime argument, so the lint
 * rule can't statically see that the effect *is* properly gated. The
 * guard it asks for is already here: `deps` is threaded straight through
 * to `useEffect`, and the `cancelled` flag stops any resolved promise
 * from setting state after teardown. */
import { useEffect, useState } from "react";

export interface AsyncState<T> {
  data: T | null;
  error: Error | null;
  loading: boolean;
}

/** Runs an async function on mount and whenever `deps` change, and
 * optionally re-runs it on an interval.
 *
 * Deliberately tiny rather than a data-fetching library: every screen
 * here is a single unconditional GET against a hub with no realtime
 * channel, so there is nothing to cache, invalidate, or deduplicate yet.
 *
 * Refreshes are *silent* -- they never flip `loading` back on, so the
 * screen updates in place instead of flashing its skeleton. A failed
 * refresh leaves the last good state alone rather than replacing a
 * populated board with an error over one dropped poll; the trade is that
 * a hub which dies mid-session goes unreported until the next mount.
 *
 * A tick that arrives while the previous request is still in flight is
 * *skipped*, not queued. Without that guard a fetch slower than the
 * interval gets a second fetch started underneath it, each one making the
 * next overlap more likely -- a threshold, not a slowdown.
 *
 * Refreshes pause while the tab is hidden (`setInterval` does not stop on
 * its own the way rAF does), and coming back refreshes immediately rather
 * than waiting out the rest of an interval.
 *
 * The `cancelled` flag prevents a slow response from setting state on an
 * unmounted component, and prevents an earlier request from overwriting a
 * later one when `deps` change mid-flight.
 */
export function useAsync<T>(
  fn: () => Promise<T>,
  deps: unknown[],
  refreshMs?: number,
): AsyncState<T> {
  const [state, setState] = useState<AsyncState<T>>({
    data: null,
    error: null,
    loading: true,
  });

  useEffect(() => {
    let cancelled = false;
    let inFlight = false;

    const run = (silent: boolean) => {
      if (inFlight) return;
      inFlight = true;
      if (!silent) setState((previous) => ({ ...previous, loading: true, error: null }));

      fn()
        .then((data) => {
          if (!cancelled) setState({ data, error: null, loading: false });
        })
        .catch((error: unknown) => {
          if (cancelled || silent) return;
          setState({
            data: null,
            error: error instanceof Error ? error : new Error(String(error)),
            loading: false,
          });
        })
        .finally(() => {
          inFlight = false;
        });
    };

    run(false);
    if (!refreshMs) {
      return () => {
        cancelled = true;
      };
    }

    // One callback for both the timer and the visibility change: a tick in
    // a hidden tab does nothing, and becoming visible refreshes right
    // away. (`visibilitychange` also fires on the way *to* hidden, where
    // the `document.hidden` check makes it a no-op.)
    const refresh = () => {
      if (!document.hidden) run(true);
    };
    const timer = setInterval(refresh, refreshMs);
    document.addEventListener("visibilitychange", refresh);
    return () => {
      cancelled = true;
      clearInterval(timer);
      document.removeEventListener("visibilitychange", refresh);
    };
  }, [...deps, refreshMs]);

  return state;
}
