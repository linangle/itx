import { render, screen, within } from "@testing-library/react";
import { MemoryRouter } from "react-router-dom";
import { describe, expect, it, vi } from "vitest";
import { SiteBar } from "./SiteBar";
import type { AsyncState } from "../hooks/useAsync";
import type { TaskDto } from "../lib/hub";
import * as hub from "../lib/hub";

// The bar itself fetches nothing; the ticker it mounts asks for the
// tape's headlines, which is the one call to quiet down.
vi.mock("../lib/hub", async (importOriginal) => ({
  ...(await importOriginal<typeof hub>()),
  listLatestTasks: vi.fn().mockResolvedValue([]),
}));

// The presentational export, not the live one: this is about the
// masthead's own markup, and `LiveSiteBar` would pull the tape's fetch
// into a test that has nothing to say about it.
const IDLE: AsyncState<{ items: TaskDto[] }> = {
  data: null,
  error: null,
  loading: false,
  stale: false,
};

function renderBar() {
  return render(
    <MemoryRouter initialEntries={["/tasks"]}>
      <SiteBar tasks={IDLE} />
    </MemoryRouter>,
  );
}

/** The masthead was covered only through the prediction page's own test
 * file, which went with the sample sections -- so removing them would
 * have taken the site's one piece of every-page chrome out of the suite
 * along with them. This is that coverage, in the file it should have
 * been in.
 */
describe("SiteBar", () => {
  it("carries the wordmark", () => {
    renderBar();
    expect(screen.getByText("internet traffic exchange")).toBeInTheDocument();
  });

  it("links to onboarding, work and standings", () => {
    renderBar();
    const bar = screen.getByRole("navigation", { name: "Site pages" });
    for (const [name, href] of [
      ["main hub", "/tasks"],
      ["connect an agent", "/connect"],
      ["leaderboard", "/leaderboard"],
    ]) {
      expect(within(bar).getByRole("link", { name })).toHaveAttribute("href", href);
    }
    expect(within(bar).getAllByRole("link")).toHaveLength(3);
  });

  it("no longer offers the deferred sample sections", () => {
    // Asserted rather than assumed: a stale masthead link is a 404 with
    // no route behind it, and it is the one piece of chrome on every
    // page -- so it would be wrong everywhere at once.
    renderBar();
    const bar = screen.getByRole("navigation", { name: "Site pages" });
    expect(within(bar).queryByRole("link", { name: /prediction/i })).not.toBeInTheDocument();
    expect(within(bar).queryByRole("link", { name: /newsroom/i })).not.toBeInTheDocument();
  });
});
