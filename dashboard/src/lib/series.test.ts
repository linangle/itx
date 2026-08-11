import { describe, expect, it } from "vitest";
import {
  agentEarningsSeries,
  boardTotals,
  chooseWindow,
  countByCreatedAt,
  cumulative,
  periodChangePct,
  summarizeByCapability,
  summarizeByKind,
  summarizeBySector,
} from "./series";
import type { TaskDto } from "./hub";

const NOW = Date.parse("2026-08-09T12:00:00Z");
const HOUR = 3_600_000;

function task(overrides: Partial<TaskDto> & { created_at: string }): TaskDto {
  return {
    id: crypto.randomUUID(),
    description: "t",
    bounty: 1000,
    status: "Open",
    poster: "poster",
    claimant: null,
    failed_attempts: 0,
    min_reputation: 0,
    close_reason: null,
    capabilities: [],
    kind: "hash_match",
    ...overrides,
  } as TaskDto;
}

/** Every test pins `now` explicitly rather than letting the series module
 * read the clock, so none of this can go flaky at a bucket boundary. */
const opts = { buckets: 4, windowMs: 4 * HOUR, now: NOW };

describe("countByCreatedAt", () => {
  it("drops tasks older than the window instead of piling them into bucket 0", () => {
    const series = countByCreatedAt(
      [
        task({ created_at: new Date(NOW - 100 * HOUR).toISOString() }),
        task({ created_at: new Date(NOW - 30 * HOUR).toISOString() }),
      ],
      opts,
    );
    expect(series).toEqual([0, 0, 0, 0]);
  });

  it("places a task in the bucket its timestamp falls in", () => {
    const series = countByCreatedAt(
      [
        task({ created_at: new Date(NOW - 3.5 * HOUR).toISOString() }),
        task({ created_at: new Date(NOW - 0.5 * HOUR).toISOString() }),
        task({ created_at: new Date(NOW - 0.2 * HOUR).toISOString() }),
      ],
      opts,
    );
    expect(series).toEqual([1, 0, 0, 2]);
  });

  it("puts a task created exactly at `now` in the last bucket, not past the end", () => {
    const series = countByCreatedAt([task({ created_at: new Date(NOW).toISOString() })], opts);
    expect(series).toEqual([0, 0, 0, 1]);
  });

  it("ignores an unparseable timestamp rather than producing NaN buckets", () => {
    const series = countByCreatedAt([task({ created_at: "not a date" })], opts);
    expect(series).toEqual([0, 0, 0, 0]);
  });
});

describe("periodChangePct", () => {
  it("compares the later half of the window against the earlier half", () => {
    // earlier half sums to 2, later half to 3 -> +50%
    expect(periodChangePct([1, 1, 2, 1])).toBeCloseTo(50);
  });

  it("returns null when the earlier half is empty, rather than Infinity", () => {
    // Any activity at all is an increase from zero, which is not a
    // percentage -- this must read as "no comparison", not "+∞%".
    expect(periodChangePct([0, 0, 5, 5])).toBeNull();
  });

  it("returns null for a series too short to have two halves", () => {
    expect(periodChangePct([])).toBeNull();
    expect(periodChangePct([7])).toBeNull();
  });

  it("reports a real zero when activity is genuinely unchanged", () => {
    expect(periodChangePct([3, 3])).toBe(0);
  });
});

describe("cumulative", () => {
  it("produces a running total", () => {
    expect(cumulative([1, 0, 2, 3])).toEqual([1, 1, 3, 6]);
  });
});

describe("agentEarningsSeries", () => {
  it("counts only paid tasks claimed by that agent", () => {
    const tasks = [
      task({ created_at: new Date(NOW - 3.5 * HOUR).toISOString(), claimant: "alice", status: "Paid", bounty: 500 }),
      // right agent, not yet paid
      task({ created_at: new Date(NOW - 2.5 * HOUR).toISOString(), claimant: "alice", status: "Claimed", bounty: 900 }),
      // paid, but a different agent
      task({ created_at: new Date(NOW - 1.5 * HOUR).toISOString(), claimant: "bob", status: "Paid", bounty: 900 }),
      task({ created_at: new Date(NOW - 0.5 * HOUR).toISOString(), claimant: "alice", status: "Paid", bounty: 250 }),
    ];
    expect(agentEarningsSeries(tasks, "alice", opts)).toEqual([500, 500, 500, 750]);
  });

  it("is flat for an agent with no visible claimed work", () => {
    // Consensus winners are never exposed by the hub, so an agent who only
    // ever did consensus work has no curve to draw -- flat, not missing.
    const tasks = [task({ created_at: new Date(NOW - HOUR).toISOString(), kind: "consensus" })];
    expect(agentEarningsSeries(tasks, "carol", opts)).toEqual([0, 0, 0, 0]);
  });
});

describe("summarizeByKind", () => {
  it("always returns all three kinds, even when the board is empty", () => {
    const summary = summarizeByKind([], opts);
    expect(summary.map((s) => s.kind)).toEqual(["hash_match", "consensus", "disputable"]);
    expect(summary.every((s) => s.open === 0)).toBe(true);
  });

  it("counts open tasks and open bounty separately from total posted", () => {
    const tasks = [
      task({ created_at: new Date(NOW - HOUR).toISOString(), bounty: 100, status: "Open" }),
      task({ created_at: new Date(NOW - HOUR).toISOString(), bounty: 900, status: "Paid" }),
    ];
    const hashMatch = summarizeByKind(tasks, opts)[0];
    expect(hashMatch.open).toBe(1);
    expect(hashMatch.openBounty).toBe(100);
    expect(hashMatch.posted).toBe(2);
  });
});

describe("summarizeByCapability", () => {
  it("ranks tags by open count and omits untagged tasks entirely", () => {
    const tasks = [
      task({ created_at: new Date(NOW - HOUR).toISOString(), capabilities: ["python"] }),
      task({ created_at: new Date(NOW - HOUR).toISOString(), capabilities: ["python", "ocr"] }),
      task({ created_at: new Date(NOW - HOUR).toISOString(), capabilities: [] }),
    ];
    const summary = summarizeByCapability(tasks, 8, opts);
    expect(summary.map((s) => s.capability)).toEqual(["python", "ocr"]);
    expect(summary[0].open).toBe(2);
  });
});

describe("summarizeBySector", () => {
  const at = (hoursAgo: number) => new Date(NOW - hoursAgo * HOUR).toISOString();

  it("groups markets under their sectors, biggest open bounty first at both levels", () => {
    const summary = summarizeBySector(
      [
        task({ created_at: at(1), capabilities: ["python"], bounty: 100, status: "Open" }),
        task({ created_at: at(1), capabilities: ["rust"], bounty: 900, status: "Open" }),
        task({ created_at: at(1), capabilities: ["ocr"], bounty: 5000, status: "Open" }),
      ],
      opts,
    );
    expect(summary.map((s) => s.name)).toEqual(["data", "coding"]);
    expect(summary[1].markets.map((m) => m.capability)).toEqual(["rust", "python"]);
    expect(summary[1].open).toBe(2);
    expect(summary[1].openBounty).toBe(1000);
  });

  it("files an unknown tag under the other sector", () => {
    const summary = summarizeBySector(
      [task({ created_at: at(1), capabilities: ["haruspicy"] })],
      opts,
    );
    expect(summary.map((s) => s.name)).toEqual(["other"]);
    expect(summary[0].markets.map((m) => m.capability)).toEqual(["haruspicy"]);
  });

  it("leaves untagged tasks out, as summarizeByCapability does", () => {
    expect(summarizeBySector([task({ created_at: at(1) })], opts)).toEqual([]);
  });

  it("counts a task once per sector, not once per tag", () => {
    // Two tags in the same sector: one task, one count. A third tag in
    // another sector counts it there too -- the task genuinely trades in
    // both.
    const twice = task({
      created_at: at(1),
      capabilities: ["python", "rust", "ocr"],
      status: "Open",
      bounty: 100,
    });
    const summary = summarizeBySector([twice], opts);
    const coding = summary.find((s) => s.name === "coding");
    const data = summary.find((s) => s.name === "data");
    expect(coding?.posted).toBe(1);
    expect(coding?.openBounty).toBe(100);
    expect(data?.posted).toBe(1);
  });

  it("counts a task once when it carries the same tag twice", () => {
    // The grouping pass replaced a per-tag filter, which was immune to
    // this; without the guard the bounty would be counted twice.
    const summary = summarizeBySector(
      [
        task({
          created_at: at(1),
          capabilities: ["python", "python"],
          status: "Open",
          bounty: 100,
        }),
      ],
      opts,
    );
    expect(summary[0].openBounty).toBe(100);
    expect(summary[0].markets).toHaveLength(1);
    expect(summary[0].markets[0].openBounty).toBe(100);
  });

  it("withholds a market's change below two active buckets", () => {
    // One payout in one half of the window is not a trend -- the agent
    // tickers had this guard and the markets keep it.
    const summary = summarizeBySector(
      [task({ created_at: at(0.5), capabilities: ["python"], bounty: 100 })],
      opts,
    );
    const python = summary[0].markets[0];
    expect(python.changePct).toBeNull();
    // The cumulative curve still has a shape to draw.
    expect(python.series).toEqual([0, 0, 0, 100]);
  });

  it("reports a market's change once both halves have activity", () => {
    const summary = summarizeBySector(
      [
        task({ created_at: at(3.5), capabilities: ["python"], bounty: 100 }),
        task({ created_at: at(0.5), capabilities: ["python"], bounty: 300 }),
      ],
      opts,
    );
    expect(summary[0].markets[0].changePct).toBe(200);
  });
});

describe("summarizeByCapability", () => {
  it("counts a task once when it carries the same tag twice", () => {
    const summary = summarizeByCapability(
      [
        task({
          created_at: new Date(NOW - HOUR).toISOString(),
          capabilities: ["ocr", "ocr"],
          status: "Open",
          bounty: 250,
        }),
      ],
      8,
      opts,
    );
    expect(summary).toHaveLength(1);
    expect(summary[0].open).toBe(1);
    expect(summary[0].openBounty).toBe(250);
  });
});

describe("chooseWindow", () => {
  function agedHours(hours: number[]) {
    return hours.map((h) => task({ created_at: new Date(NOW - h * HOUR).toISOString() }));
  }

  it("picks the smallest preset that covers the board's age", () => {
    // A board seeded minutes ago charts over an hour, not seven days --
    // otherwise every task lands in the final bucket of a flat line.
    expect(chooseWindow(agedHours([0.2, 0.5]), NOW).label).toBe("1H");
    expect(chooseWindow(agedHours([3, 0.5]), NOW).label).toBe("6H");
    expect(chooseWindow(agedHours([20]), NOW).label).toBe("24H");
    expect(chooseWindow(agedHours([100]), NOW).label).toBe("7D");
    expect(chooseWindow(agedHours([24 * 20]), NOW).label).toBe("30D");
  });

  it("is driven by the oldest task, not the newest", () => {
    expect(chooseWindow(agedHours([0.1, 0.2, 100]), NOW).label).toBe("7D");
  });

  it("falls back to 7D on an empty board", () => {
    expect(chooseWindow([], NOW).label).toBe("7D");
  });

  it("ignores unparseable timestamps rather than collapsing the window", () => {
    expect(chooseWindow([task({ created_at: "nonsense" })], NOW).label).toBe("7D");
  });

  it("clamps a future-dated task instead of picking a negative span", () => {
    // Clock skew between hub and browser must not silently narrow the axis.
    const future = task({ created_at: new Date(NOW + 5 * HOUR).toISOString() });
    expect(chooseWindow([future], NOW).label).toBe("1H");
  });

  it("caps at the widest preset for a very old board, rather than the default", () => {
    // A board older than every preset should show as much history as we
    // have -- falling back to the 7D default would show *less* history
    // for an older board, which is the wrong direction.
    expect(chooseWindow(agedHours([24 * 4000]), NOW).label).toBe("90D");
  });
});

describe("boardTotals", () => {
  it("separates value on offer from value already settled", () => {
    const tasks = [
      task({ created_at: new Date(NOW - HOUR).toISOString(), bounty: 100, status: "Open" }),
      task({ created_at: new Date(NOW - HOUR).toISOString(), bounty: 700, status: "Paid" }),
      task({ created_at: new Date(NOW - HOUR).toISOString(), bounty: 500, status: "Claimed" }),
    ];
    const totals = boardTotals(tasks, opts);
    expect(totals.openTasks).toBe(1);
    expect(totals.openBounty).toBe(100);
    expect(totals.paidTasks).toBe(1);
    expect(totals.paidBounty).toBe(700);
  });
});
