import { describe, expect, it } from "vitest";
import type { TaskDto } from "./hub";
import { DEFAULT_SORT, parseSortDirection, parseSortKey, sortTasks } from "./taskSort";

const HOUR = 3_600_000;
const NOW = Date.UTC(2026, 0, 10);

function task(overrides: Partial<TaskDto> = {}): TaskDto {
  return {
    id: "t",
    description: "a task",
    bounty: 100,
    status: "Open",
    poster: "02" + "a".repeat(64),
    claimant: null,
    failed_attempts: 0,
    min_reputation: 0,
    close_reason: null,
    capabilities: [],
    created_at: new Date(NOW).toISOString(),
    kind: "hash_match",
    ...overrides,
  } as TaskDto;
}

const ids = (tasks: TaskDto[]) => tasks.map((t) => t.id);

describe("parseSortKey", () => {
  it("falls back to the default for a value the table doesn't have", () => {
    expect(parseSortKey("bounty")).toBe("bounty");
    // A link someone kept from an older build, or typed by hand. It
    // shows the board in default order rather than erroring.
    expect(parseSortKey("colour")).toBe(DEFAULT_SORT);
    expect(parseSortKey(null)).toBe(DEFAULT_SORT);
  });

  it("only accepts desc as a direction", () => {
    expect(parseSortDirection("desc")).toBe("desc");
    expect(parseSortDirection("asc")).toBe("asc");
    expect(parseSortDirection("sideways")).toBe("asc");
  });
});

describe("sortTasks", () => {
  it("puts the newest first by default, which is what age ascending means", () => {
    const tasks = [
      task({ id: "old", created_at: new Date(NOW - 5 * HOUR).toISOString() }),
      task({ id: "new", created_at: new Date(NOW).toISOString() }),
      task({ id: "mid", created_at: new Date(NOW - HOUR).toISOString() }),
    ];
    expect(ids(sortTasks(tasks, "age", "asc"))).toEqual(["new", "mid", "old"]);
    expect(ids(sortTasks(tasks, "age", "desc"))).toEqual(["old", "mid", "new"]);
  });

  it("sorts bounty numerically, not as text", () => {
    const tasks = [task({ id: "a", bounty: 9 }), task({ id: "b", bounty: 100 })];
    // "100" < "9" as strings, which is the bug this guards.
    expect(ids(sortTasks(tasks, "bounty", "asc"))).toEqual(["a", "b"]);
  });

  it("sorts status by lifecycle, not alphabetically", () => {
    const tasks = [
      task({ id: "paid", status: "Paid" }),
      task({ id: "open", status: "Open" }),
      task({ id: "claimed", status: "Claimed" }),
    ];
    // Alphabetically this would be Claimed, Open, Paid -- an ordering
    // that describes the spelling rather than the board.
    expect(ids(sortTasks(tasks, "status", "asc"))).toEqual(["open", "claimed", "paid"]);
  });

  it("keeps untagged tasks at the bottom in both directions", () => {
    const tasks = [
      task({ id: "none", capabilities: [] }),
      task({ id: "ocr", capabilities: ["ocr"] }),
      task({ id: "rust", capabilities: ["rust"] }),
    ];
    // A reverse sort on a sparse column would otherwise open with a
    // screenful of em dashes.
    expect(ids(sortTasks(tasks, "sector", "asc")).at(-1)).toBe("none");
    expect(ids(sortTasks(tasks, "sector", "desc")).at(-1)).toBe("none");
  });

  it("breaks ties by recency so rows don't swap places between polls", () => {
    const tasks = [
      task({ id: "b", bounty: 50, created_at: new Date(NOW - HOUR).toISOString() }),
      task({ id: "a", bounty: 50, created_at: new Date(NOW).toISOString() }),
    ];
    expect(ids(sortTasks(tasks, "bounty", "asc"))).toEqual(["a", "b"]);
    expect(ids(sortTasks([...tasks].reverse(), "bounty", "asc"))).toEqual(["a", "b"]);
  });

  it("leaves the array it was given alone", () => {
    const tasks = [task({ id: "a", bounty: 9 }), task({ id: "b", bounty: 1 })];
    sortTasks(tasks, "bounty", "asc");
    expect(ids(tasks)).toEqual(["a", "b"]);
  });

  it("orders verification by the label a reader sees", () => {
    const tasks = [
      task({ id: "hash", kind: "hash_match" }),
      task({ id: "consensus", kind: "consensus" }),
    ];
    // Automatic check < Majority vote. By protocol name it would be
    // consensus < hash_match, i.e. the opposite of what the column says.
    expect(ids(sortTasks(tasks, "kind", "asc"))).toEqual(["hash", "consensus"]);
  });
});
