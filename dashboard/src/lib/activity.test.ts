import { describe, expect, it } from "vitest";
import { activityTiles } from "./activity";
import type { MarketSeriesDto } from "./hub";

/** A response with every series present and nothing interesting in it,
 * so each test can set only the fields it is about. */
function series(over: Partial<MarketSeriesDto> = {}): MarketSeriesDto {
  const zeros = [0, 0, 0, 0];
  return {
    capability: null,
    window_ms: 24 * 3_600_000,
    buckets: 4,
    start_ms: 1_000_000,
    end_ms: 1_000_000 + 24 * 3_600_000,
    posted_series: [...zeros],
    bounty_series: [...zeros],
    settled_series: [...zeros],
    paid_bounty_series: [...zeros],
    agents_series: [...zeros],
    fees_series: [...zeros],
    faucet_series: [...zeros],
    posted: 0,
    bounty: 0,
    settled: 0,
    paid_bounty: 0,
    agents: 0,
    fees: 0,
    faucet_grants: 0,
    faucet_itx: 0,
    open: 0,
    open_bounty: 0,
    first_task_at: null,
    ...over,
  };
}

function tile(dto: MarketSeriesDto, key: string) {
  const found = activityTiles(dto).find((t) => t.key === key);
  if (!found) throw new Error(`no tile ${key}`);
  return found;
}

describe("activityTiles", () => {
  it("reads posted and paid from different series, not from one", () => {
    // The point of the whole section: the hub now carries two timestamps
    // and these two tiles must not collapse back into one number.
    const dto = series({
      bounty_series: [100, 0, 0, 0],
      bounty: 100,
      paid_bounty_series: [0, 0, 0, 60],
      paid_bounty: 60,
    });
    expect(tile(dto, "bounty-posted").value).toBe(100);
    expect(tile(dto, "bounty-posted").series).toEqual([100, 0, 0, 0]);
    expect(tile(dto, "bounty-paid").value).toBe(60);
    expect(tile(dto, "bounty-paid").series).toEqual([0, 0, 0, 60]);
  });

  it("gives a count tile a count unit, so nothing prints a task as itx", () => {
    const dto = series({ posted: 3, agents: 2, faucet_grants: 1, bounty: 500, fees: 2_000 });
    expect(tile(dto, "tasks-posted").unit).toBe("count");
    expect(tile(dto, "tasks-completed").unit).toBe("count");
    expect(tile(dto, "active-agents").unit).toBe("count");
    expect(tile(dto, "faucet-grants").unit).toBe("count");
    expect(tile(dto, "bounty-posted").unit).toBe("itx");
    expect(tile(dto, "chain-fees").unit).toBe("itx");
    expect(tile(dto, "completion-rate").unit).toBe("pct");
  });

  it("withholds a sparkline where a series would be invented rather than absent", () => {
    const dto = series({ open_bounty: 900, posted: 4, settled: 2 });
    // Open bounty is a fact about now; a history would have to be
    // reconstructed from posted minus paid, which starts at zero at the
    // window's left edge whatever the real backlog was.
    expect(tile(dto, "open-bounty").series).toBeNull();
    expect(tile(dto, "open-bounty").value).toBe(900);
    // A per-bucket completion ratio compares two sets that do not
    // correspond, so it is a scalar with a note instead.
    expect(tile(dto, "completion-rate").series).toBeNull();
  });

  it("lets the completion rate exceed 100% rather than clamping a real number", () => {
    // Six tasks finished in a quiet week for posting. That is a true
    // statement about the window, and clamping it to 100 would hide the
    // backlog clearing -- which is the thing worth seeing.
    const dto = series({ posted: 4, settled: 6 });
    expect(tile(dto, "completion-rate").value).toBe(150);
  });

  it("reports no completion rate as zero on an empty board rather than dividing by it", () => {
    expect(tile(series(), "completion-rate").value).toBe(0);
    expect(tile(series(), "average-bounty").value).toBe(0);
  });

  it("averages a bounty per bucket and leaves quiet buckets alone", () => {
    // Bucket 1 has no tasks at all. Charting 0 there would put a trough
    // in the line every quiet hour and read as a collapse.
    const dto = series({
      bounty_series: [200, 0, 900, 0],
      posted_series: [2, 0, 3, 0],
      bounty: 1_100,
      posted: 5,
    });
    expect(tile(dto, "average-bounty").series).toEqual([100, 0, 300, 0]);
    expect(tile(dto, "average-bounty").value).toBe(220);
  });

  it("never offers a change reading for a figure with no good direction", () => {
    const dto = series({
      bounty_series: [10, 10, 20, 20],
      posted_series: [1, 1, 2, 2],
      fees_series: [1, 1, 2, 2],
      posted: 6,
      bounty: 60,
    });
    // An average bounty that rose is not better than one that fell, and
    // fees are a cost -- colouring either green would be a claim.
    expect(tile(dto, "average-bounty").changePct).toBeNull();
    expect(tile(dto, "chain-fees").changePct).toBeNull();
    expect(tile(dto, "bounty-posted").changePct).not.toBeNull();
  });

  /** The copy has to say what the hub actually does.
   *
   * `c40cbc0` fixed the fee arithmetic -- an escrow-funded consensus
   * task pays every winner in one transaction and so costs one fee --
   * and left this note saying "one network fee per payout. A consensus
   * task with three winners costs three." The number on the board became
   * right and the sentence beside it stayed wrong, which is worse than
   * either alone: a reader who checks the explanation against the figure
   * concludes the figure is broken.
   *
   * Nothing connected them, so this does. It cannot verify the
   * arithmetic from here -- that is the hub's own three-winner test --
   * but it can refuse the specific claim that was wrong.
   */
  it("does not tell the reader fees are charged per winner", () => {
    const note = tile(series(), "chain-fees").note.toLowerCase();
    expect(note).toContain("transaction");
    for (const wrong of ["per payout", "winners costs three", "one fee per winner"]) {
      expect(note, `fee note still claims "${wrong}"`).not.toContain(wrong);
    }
  });

  /** A key count is not a headcount, and the note has to say so. */
  it("says the agent count is keys rather than people", () => {
    const note = tile(series(), "active-agents").note.toLowerCase();
    expect(note).toContain("keys");
    expect(note).toMatch(/not a count of people|many agents/);
  });

  it("carries a definition on every tile", () => {
    // The failure this section corrects was a figure whose meaning had
    // to be guessed, so a tile without a note is a regression.
    for (const t of activityTiles(series())) {
      expect(t.note.length, `${t.key} has no note`).toBeGreaterThan(20);
      expect(t.label).not.toBe("");
    }
  });
});
