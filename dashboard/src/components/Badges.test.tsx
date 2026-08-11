import { render, screen } from "@testing-library/react";
import { MemoryRouter } from "react-router-dom";
import { describe, expect, it } from "vitest";
import { AgentLink } from "./Badges";

const KEY = "02" + "a".repeat(62) + "beef";

function renderLink(props: Parameters<typeof AgentLink>[0]) {
  return render(
    <MemoryRouter>
      <AgentLink {...props} />
    </MemoryRouter>,
  );
}

describe("AgentLink", () => {
  it("puts the name on the first line and the key on the second", () => {
    renderLink({ pubkey: KEY, name: "SwiftWarlock" });

    expect(screen.getByText("SwiftWarlock")).toBeInTheDocument();
    expect(screen.getByText("02aa…beef")).toBeInTheDocument();
    expect(screen.getByRole("link")).toHaveAttribute("href", `/agents/${KEY}`);
  });

  it("promotes the key to the first line when there is no name", () => {
    const { container } = renderLink({ pubkey: KEY, name: null });

    // The pre-naming rendering: 6/4 truncation, and no second line at
    // all -- a leaderboard row for an unnamed agent stays one line high.
    expect(screen.getByText("02aaaa…beef")).toBeInTheDocument();
    expect(container.querySelectorAll(".itx-agent-key")).toHaveLength(0);
  });

  it("shares the second line between the key and the meta fact", () => {
    renderLink({ pubkey: KEY, name: "AmberOtter", meta: "3 done" });

    expect(screen.getByText("AmberOtter")).toBeInTheDocument();
    expect(screen.getByText("02aa…beef · 3 done")).toBeInTheDocument();
  });

  it("shows meta alone on the second line when there is no name", () => {
    renderLink({ pubkey: KEY, name: null, meta: "3 done" });

    // The key is already the headline here, so it must not be repeated
    // underneath itself.
    expect(screen.getByText("02aaaa…beef")).toBeInTheDocument();
    expect(screen.getByText("3 done")).toBeInTheDocument();
  });

  it("keeps the full key recoverable in every state", () => {
    const { unmount } = renderLink({ pubkey: KEY, name: "SwiftWarlock" });
    expect(screen.getByRole("link")).toHaveAttribute("title", KEY);
    unmount();

    renderLink({ pubkey: KEY, name: null });
    expect(screen.getByRole("link")).toHaveAttribute("title", KEY);
  });
});
