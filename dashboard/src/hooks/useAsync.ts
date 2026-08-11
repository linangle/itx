/* eslint-disable react-hooks/exhaustive-deps --
 * This hook takes its dependency list as a runtime argument, so the lint
 * rule can't statically see that the effect *is* properly gated and warns
 * about the `setState` calls inside it. The guard it's asking for is
 * already here: `deps` is threaded straight through to `useEffect`, and
 * the `cancelled` flag stops any resolved promise from setting state
 * after the effect is torn down. Scoped to this one file, which exists
 * solely to wrap that pattern. */
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
 * here is a single unconditional GET against a hub that has no realtime
 * channel, so there is nothing to cache, invalidate, or deduplicate yet.
 * When v2 adds writes that need to invalidate reads, that's the moment to
 * reach for something bigger -- not before.
 *
 * `refreshMs` is polling, which is the only option while the hub has no
 * realtime channel: a board that claims to be live has to go and ask.
 * Refreshes are *silent* -- they never flip `loading` back on, so the
 * screen updates in place instead of flashing its skeleton every few
 * seconds.
 *
 * A failed refresh deliberately leaves the last good state alone rather
 * than replacing the screen with an error. One dropped poll is not news,
 * and blanking a populated board over it is worse than showing data a
 * few seconds stale. The trade is that a hub which dies mid-session goes
 * unreported until the next mount or navigation; when the hub grows a
 * realtime channel, that's the moment to surface connection state
 * properly rather than inferring it from a poll.
 *
 * A tick that arrives while the previous request is still in flight is
 * *skipped*, not queued. Without that guard a fetch slower than the
 * interval doesn't delay the next poll -- it gets a second fetch started
 * underneath it, each one making the network slower and the next overlap
 * more likely. That is a threshold, not a slowdown: the page is fine
 * until the day the fetch crosses the interval, and then it spirals.
 * Skipping means a slow hub is polled exactly as fast as it can answer,
 * and no faster.
 *
 * Refreshes also pause while the tab is hidden -- rAF loops stop on
 * their own when a tab is backgrounded, but `setInterval` does not, and
 * a board nobody is looking at should not keep pulling the whole task
 * list. Coming back to the tab refreshes immediately rather than waiting
 * out the rest of an interval, so the first thing a returning reader
 * sees is current.
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

    // One callback for both the timer and the visibility change: a tick
    // in a hidden tab does nothing, and becoming visible refreshes right
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
