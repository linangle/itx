import { render, screen } from "@testing-library/react";
import { Suspense, lazy } from "react";
import { afterEach, describe, expect, it, vi } from "vitest";
import ErrorBoundary, { BrokenPage } from "./ErrorBoundary";

function Throws(): never {
  throw new Error("a field the hub did not send");
}

// React logs a caught render error to the console as well as handing it
// to the boundary; that is expected here and would otherwise be noise.
const quiet = vi.spyOn(console, "error").mockImplementation(() => {});
afterEach(() => quiet.mockClear());

describe("the boundary", () => {
  it("shows the fallback page instead of unmounting everything", () => {
    render(
      <ErrorBoundary fallback={(error) => <BrokenPage error={error} />}>
        <h1>the masthead</h1>
        <Throws />
      </ErrorBoundary>,
    );
    expect(screen.getByRole("heading", { name: "this page broke" })).toBeInTheDocument();
    expect(screen.getByRole("link", { name: "back to the board" })).toHaveAttribute("href", "/");
    expect(screen.getByText("a field the hub did not send")).toBeInTheDocument();
  });

  it("swallows a chunk that failed to load, which Suspense does not", async () => {
    // A lazy component whose import is refused -- the half-upgraded
    // site, or a blocked request for the globe's chunk. Suspense alone
    // rethrows the rejection to the root.
    const Refused = lazy(() => Promise.reject(new Error("Failed to fetch dynamically imported module")));
    render(
      <div>
        <p>the hero copy</p>
        <ErrorBoundary fallback={null}>
          <Suspense fallback={null}>
            <Refused />
          </Suspense>
        </ErrorBoundary>
      </div>,
    );
    // Let the rejected import settle and the boundary catch it.
    await new Promise((resolve) => setTimeout(resolve, 0));
    await screen.findByText("the hero copy");
    expect(screen.queryByText(/Failed to fetch/)).not.toBeInTheDocument();
  });
});
