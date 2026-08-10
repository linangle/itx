import { describe, expect, it } from "vitest";
import {
  directionOf,
  formatCompactItx,
  formatCountdown,
  formatItx,
  formatItxExact,
  formatPct,
  formatRelative,
  truncatePubkey,
  UNITS_PER_ITX,
} from "./format";

describe("formatItx", () => {
  it("converts base units to ITX", () => {
    expect(formatItx(UNITS_PER_ITX)).toBe("1.00");
    // The faucet grants 50_000_000 base units -- half an ITX.
    expect(formatItx(50_000_000)).toBe("0.50");
  });

  it("keeps at least two decimals so columns stay aligned", () => {
    expect(formatItx(0)).toBe("0.00");
    expect(formatItx(2 * UNITS_PER_ITX)).toBe("2.00");
  });

  it("keeps the precision of a small amount rather than rounding it away", () => {
    // HUB_TRANSACTION_FEE is 1_000 base units. Rounding this to "0.00"
    // would make every fee in the UI look like nothing at all.
    expect(formatItx(1_000)).toBe("0.00001");
  });

  it("caps a long amount at four decimals so columns stay scannable", () => {
    expect(formatItx(250_745_433)).toBe("2.5075");
  });

  it("groups thousands", () => {
    expect(formatItx(1234 * UNITS_PER_ITX)).toBe("1,234.00");
  });
});

describe("formatItxExact", () => {
  it("keeps every base unit, for detail views", () => {
    expect(formatItxExact(250_745_433)).toBe("2.50745433");
    expect(formatItxExact(UNITS_PER_ITX)).toBe("1.00");
  });
});

describe("formatCompactItx", () => {
  it("abbreviates large amounts", () => {
    expect(formatCompactItx(1500 * UNITS_PER_ITX)).toBe("1.5K");
  });

  it("stays exact below the compaction threshold", () => {
    expect(formatCompactItx(12 * UNITS_PER_ITX)).toBe("12.00");
  });
});

describe("formatPct", () => {
  it("signs positive values and leaves negatives their own sign", () => {
    expect(formatPct(1.3)).toBe("+1.30%");
    expect(formatPct(-1.65)).toBe("-1.65%");
  });

  it("renders a genuine zero rather than a dash", () => {
    expect(formatPct(0)).toBe("0.00%");
  });

  it("renders null as an em dash -- 'no comparison' is not 'no change'", () => {
    expect(formatPct(null)).toBe("—");
    expect(formatPct(Infinity)).toBe("—");
  });
});

describe("directionOf", () => {
  it("treats both zero and unknown as flat, so neither reads as good news", () => {
    expect(directionOf(0)).toBe("flat");
    expect(directionOf(null)).toBe("flat");
    expect(directionOf(3)).toBe("up");
    expect(directionOf(-3)).toBe("down");
  });
});

describe("truncatePubkey", () => {
  it("shows both ends of a 66-character key", () => {
    const pubkey = "02".concat("a".repeat(64));
    expect(truncatePubkey(pubkey)).toBe("02aaaa…aaaa");
  });

  it("leaves a short string alone rather than making it longer", () => {
    expect(truncatePubkey("abc")).toBe("abc");
  });
});

describe("formatCountdown", () => {
  const now = Date.parse("2026-08-09T12:00:00Z");

  it("distinguishes time remaining from time elapsed", () => {
    // The whole point of this helper over formatRelative: "in 4h" and
    // "4h ago" mean opposite things for a deadline.
    expect(formatCountdown("2026-08-09T16:00:00Z", now)).toEqual({
      text: "in 4h",
      expired: false,
    });
    expect(formatCountdown("2026-08-09T08:00:00Z", now)).toEqual({
      text: "4h ago",
      expired: true,
    });
  });

  it("reads naturally at the boundary instead of saying 'in just now'", () => {
    expect(formatCountdown("2026-08-09T12:00:30Z", now).text).toBe("any moment");
    expect(formatCountdown("2026-08-09T11:59:30Z", now).text).toBe("just expired");
  });

  it("degrades to a dash on an unparseable value", () => {
    expect(formatCountdown("nonsense", now)).toEqual({ text: "—", expired: false });
  });
});

describe("formatRelative", () => {
  const now = Date.parse("2026-08-09T12:00:00Z");

  it("uses coarse units past each threshold", () => {
    expect(formatRelative("2026-08-09T11:58:00Z", now)).toBe("2m");
    expect(formatRelative("2026-08-09T08:00:00Z", now)).toBe("4h");
    expect(formatRelative("2026-08-07T12:00:00Z", now)).toBe("2d");
  });

  it("collapses anything under a minute", () => {
    expect(formatRelative("2026-08-09T11:59:30Z", now)).toBe("just now");
  });

  it("handles a future timestamp the same way -- deadlines are also relative", () => {
    expect(formatRelative("2026-08-09T16:00:00Z", now)).toBe("4h");
  });

  it("degrades to a dash on an unparseable value", () => {
    expect(formatRelative("nonsense", now)).toBe("—");
  });
});
