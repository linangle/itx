import { render } from "@testing-library/react";
import { describe, expect, it } from "vitest";
import TaskProgress from "./TaskProgress";
import type { TaskDto } from "../lib/hub";

const KEY = "02" + "a".repeat(62) + "beef";

/** A task as the hub would serve it, with a kind this build has never
 * heard of -- hence the cast through `unknown`: the point is a value the
 * type does not admit. */
function task(overrides: Record<string, unknown>): TaskDto {
  return {
    id: "t-1",
    description: "a task",
    bounty: 1_000,
    status: "Claimed",
    poster: KEY,
    claimant: KEY,
    failed_attempts: 0,
    min_reputation: 0,
    close_reason: null,
    capabilities: [],
    kind: "hash_match",
    created_at: "2026-09-15T00:00:00Z",
    bounty_confirmed: 0,
    bounty_pending: 1_000,
    settled_at: null,
    ...overrides,
  } as unknown as TaskDto;
}

describe("a task kind this build does not know", () => {
  it("renders a generic sequence rather than throwing", () => {
    // `TaskStatus` drifted from the hub once (Submitted, PayoutFailed) and
    // the tape and the badges were hardened for it; this stepper indexed
    // a table by kind with nothing behind it, so a fourth kind blanked
    // every task page.
    const { container } = render(<TaskProgress task={task({ kind: "auction" })} />);
    const labels = Array.from(container.querySelectorAll(".itx-step-label")).map((el) => el.textContent);
    expect(labels).toEqual(["posted", "in progress", "paid"]);
    expect(container.querySelector(".itx-step.current")?.textContent).toBe("in progress");
  });

  it("still derails a closed one", () => {
    const { container } = render(<TaskProgress task={task({ kind: "auction", status: "Closed" })} />);
    expect(container.querySelector(".itx-step.derailed")?.textContent).toBe("closed");
  });
});
