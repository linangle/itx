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

/** Runs an async function on mount and whenever `deps` change.
 *
 * Deliberately tiny rather than a data-fetching library: every screen
 * here is a single unconditional GET against a hub that has no realtime
 * channel, so there is nothing to cache, invalidate, or deduplicate yet.
 * When v2 adds writes that need to invalidate reads, that's the moment to
 * reach for something bigger -- not before.
 *
 * The `cancelled` flag prevents a slow response from setting state on an
 * unmounted component, and prevents an earlier request from overwriting a
 * later one when `deps` change mid-flight.
 */
export function useAsync<T>(fn: () => Promise<T>, deps: unknown[]): AsyncState<T> {
  const [state, setState] = useState<AsyncState<T>>({
    data: null,
    error: null,
    loading: true,
  });

  useEffect(() => {
    let cancelled = false;
    setState((previous) => ({ ...previous, loading: true, error: null }));

    fn()
      .then((data) => {
        if (!cancelled) setState({ data, error: null, loading: false });
      })
      .catch((error: unknown) => {
        if (!cancelled) {
          setState({
            data: null,
            error: error instanceof Error ? error : new Error(String(error)),
            loading: false,
          });
        }
      });

    return () => {
      cancelled = true;
    };
  }, deps);

  return state;
}
