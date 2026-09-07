import { render, screen } from "@testing-library/react";
import { afterEach, describe, expect, it } from "vitest";
import NewsTicker from "./NewsTicker";
import type { TaskDto, TaskStatus } from "../lib/hub";
import type { AsyncState } from "../hooks/useAsync";

/** Every status the hub can put on a task, copied from `TaskStatus` in
 * `hub/src/board.rs`. Listed literally rather than derived, because a
 * type has no runtime members and deriving it from the union under test
 * would let the two drift together in exactly the way that caused the
 * bug this file exists for. */
const EVERY_STATUS: TaskStatus[] = [
  "Open",
  "Claimed",
  "AwaitingDispute",
  "Disputed",
  "Verified",
  "Submitted",
  "Paid",
  "PayoutFailed",
  "Closed",
];

const KEY = "02" + "a".repeat(62) + "beef";

function task(status: TaskStatus, overrides: Partial<TaskDto> = {}): TaskDto {
  return {
    id: `t-${status}`,
    description: "a task",
    bounty: 1_000,
    status,
    poster: KEY,
    claimant: KEY,
    failed_attempts: 0,
    min_reputation: 0,
    close_reason: null,
    capabilities: [],
    created_at: "2026-09-07T12:00:00Z",
    kind: "hash_match",
    ...overrides,
  } as TaskDto;
}

function state(items: TaskDto[]): AsyncState<{ items: TaskDto[] }> {
  return { data: { items }, error: null, loading: false };
}

afterEach(() => sessionStorage.clear());

describe("NewsTicker", () => {
  /** The regression, and it was not a cosmetic one. `TaskStatus` was
   * missing `Submitted` and `PayoutFailed` while the hub was already
   * serving both, so `headline` fell off the end of its switch and
   * returned `undefined`; the duration calculation then read `.length`
   * from it and threw. There is no error boundary in this app and this
   * component renders inside `SiteBar`, which is on every screen, so a
   * single settling task blanked the entire site until it aged out of
   * the newest fourteen.
   *
   * Every status, not just the two: what failed here was the assumption
   * that the union was complete, so the test that replaces it has to
   * cover the whole set. */
  it.each(EVERY_STATUS)("renders a headline for a %s task", (status) => {
    const { container } = render(<NewsTicker tasks={state([task(status)])} />);

    const items = container.querySelectorAll(".itx-news-item");
    expect(items.length).toBeGreaterThan(0);
    for (const item of items) {
      expect(item.textContent).toBeTruthy();
      expect(item.textContent).not.toContain("undefined");
    }
  });

  it("phrases a settling task as money in flight, not as money settled", () => {
    render(<NewsTicker tasks={state([task("Submitted")])} />);

    // "settling", not "settled": the payout is on the wire and the hub
    // has not seen it on chain yet, which is the distinction the whole
    // Submitted status exists to make.
    expect(screen.getAllByText(/^settling [\d.]+ itx →/i).length).toBeGreaterThan(0);
  });

  it("says a failed payout is still owed", () => {
    render(<NewsTicker tasks={state([task("PayoutFailed")])} />);

    expect(screen.getAllByText(/still owed/i).length).toBeGreaterThan(0);
  });

  /** The belt to the union's braces. A hub newer than this build can
   * serve a status `TaskStatus` has never heard of, and when it does the
   * tape must go dull rather than take the site with it. */
  it("survives a status this build has never heard of", () => {
    const fromTheFuture = task("Archived" as TaskStatus);

    const { container } = render(<NewsTicker tasks={state([fromTheFuture])} />);

    const items = container.querySelectorAll(".itx-news-item");
    expect(items.length).toBeGreaterThan(0);
    for (const item of items) {
      expect(item.textContent).toMatch(/itx/i);
      expect(item.textContent).not.toContain("undefined");
    }
  });
});
