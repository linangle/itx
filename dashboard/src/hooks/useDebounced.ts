import { useEffect, useState } from "react";

/** `value`, held back until it has stopped changing for `delayMs`.
 *
 * For search boxes whose value keys a request. Typing "warlock" is seven
 * keystrokes and would be seven requests to the hub, six of them for a
 * prefix nobody asked about and any of which can land out of order --
 * the answer to "warl" arriving after the answer to "warlock" leaves the
 * wrong rows on screen. Debouncing collapses them to one.
 *
 * The field itself stays undebounced, so typing never feels laggy: the
 * input renders `value` on every keystroke and only the *fetch* waits.
 *
 * An empty value passes through immediately. Clearing a search is a
 * request to see everything again, and making someone wait a beat for
 * their own board to come back reads as the page having stalled.
 */
export function useDebounced<T>(value: T, delayMs = 300): T {
  const [settled, setSettled] = useState(value);

  useEffect(() => {
    if (value === "" || value === null || value === undefined) {
      setSettled(value);
      return;
    }
    const timer = setTimeout(() => setSettled(value), delayMs);
    return () => clearTimeout(timer);
  }, [value, delayMs]);

  return settled;
}
