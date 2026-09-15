import { Component, type ErrorInfo, type ReactNode } from "react";

/** The one thing React will not do for you: keep the rest of the page
 * when one component throws in render. Without a boundary an error
 * anywhere unmounts the entire root -- a blank white document with no
 * masthead, no message and no way back -- and this app has been there
 * twice: a tape headline for a status the union did not know, and an
 * activity tile reading fields an older hub did not send. Each was
 * fixed where it happened, and each time the note beside the fix said
 * the same thing, that the amplifier was the missing boundary.
 *
 * Two of them, then. One at the root (`main.tsx`) whose fallback is a
 * page that says what happened and offers the board; and one around
 * anything decorative that can fail on its own, like the landing hero's
 * lazily-loaded globe, whose fallback is nothing -- `Suspense` catches a
 * chunk that is *loading*, not a chunk whose request was refused, and a
 * rejected import throws to the nearest boundary like any other error.
 *
 * A class, because React only gives boundaries to classes. */
export default class ErrorBoundary extends Component<
  { fallback: ReactNode | ((error: Error) => ReactNode); children: ReactNode },
  { error: Error | null }
> {
  state: { error: Error | null } = { error: null };

  static getDerivedStateFromError(error: Error) {
    return { error };
  }

  componentDidCatch(error: Error, info: ErrorInfo) {
    // The console, not the hub: nothing here reports back, and a
    // visitor's broken page is not the hub's business.
    console.error("render failed", error, info.componentStack);
  }

  render() {
    const { error } = this.state;
    if (error === null) return this.props.children;
    const { fallback } = this.props;
    return typeof fallback === "function" ? fallback(error) : fallback;
  }
}

/** The root fallback: the page is gone, so say so in the site's voice
 * and offer the one link that always works. A plain anchor rather than
 * a router `Link`, because the router may be part of what broke. */
export function BrokenPage({ error }: { error: Error }) {
  return (
    <main className="itx-connect">
      <h1>this page broke</h1>
      <p>
        something on it failed while drawing. the board itself is fine:{" "}
        <a href="/">back to the board</a>, or reload this page.
      </p>
      <p>
        what it said: <code>{error.message || String(error)}</code>
      </p>
    </main>
  );
}
