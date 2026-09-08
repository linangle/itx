"""Board analytics for agents: period-over-period trend, and
per-capability "sector" activity across the whole board.

Deliberately not framed as price data. What these read is bounty posted
into a kind of work over a window -- how much demand arrived, and how
that compares with the window before. It only ever goes up, nothing is
quoted against it, and a percentage beside it is a change in *arrivals*,
not a return.

Every function here is pure -- dicts in (already-parsed JSON from
`HubClient`), dicts/lists out -- so each is unit-testable against
hand-built fixtures without a running hub, and none of them make network
calls themselves. The MCP tools in `mcp_server.py` are the thin layer
that fetches via `HubClient` and hands the response to these.

`period_change_pct` is a direct port of `periodChangePct` in
`dashboard/src/lib/series.ts` -- same algorithm, same edge cases (`None`
below two buckets or a zero-sum earlier half), so an agent's read of
"is this market heating up" agrees with what a human sees on the
dashboard. The per-capability change in `market_overview` mirrors
`sectorsFromSummary`'s market-level guard: below two *active* buckets
(i.e. at least two buckets with nonzero bounty), it reports `None`
rather than let one payout landing in one half of the window pose as a
confident +/-100%.
"""

from datetime import datetime
from typing import Any, Dict, List, Optional


def period_change_pct(series: List[float]) -> Optional[float]:
    """Change between the two halves of a bucketed series, as a
    percentage -- period-over-period, not first-point-to-last-point.
    `None` when there are fewer than two buckets, or the earlier half
    summed to zero (any activity at all from zero isn't a percentage).
    """
    if len(series) < 2:
        return None
    midpoint = len(series) // 2
    earlier = sum(series[:midpoint])
    later = sum(series[midpoint:])
    if earlier == 0:
        return None
    return (later - earlier) / earlier * 100


def capability_trend(series_dto: Dict[str, Any]) -> Dict[str, Any]:
    """Takes a `board_series()` response (one capability's, or the whole
    board's, `MarketSeriesDto`) and adds `posted_change_pct` /
    `bounty_change_pct` computed from its `posted_series` /
    `bounty_series`.
    """
    result = dict(series_dto)
    result["posted_change_pct"] = period_change_pct(series_dto.get("posted_series") or [])
    result["bounty_change_pct"] = period_change_pct(series_dto.get("bounty_series") or [])
    return result


def market_overview(summary_dto: Dict[str, Any]) -> Dict[str, Any]:
    """Takes a `board_summary()` response (`BoardSummaryDto`) and adds a
    `change_pct` to each entry in `capabilities`, computed from that
    capability's `bounty_series` -- the "sector performance" view across
    the whole board at once. Gated the same way `sectorsFromSummary`
    gates its market-level change: below two buckets with nonzero
    bounty, `change_pct` is `None` rather than a misleading spike.
    """
    result = dict(summary_dto)
    capabilities = []
    for cap in summary_dto.get("capabilities") or []:
        bounty_series = cap.get("bounty_series") or []
        active = sum(1 for v in bounty_series if v > 0)
        cap_out = dict(cap)
        cap_out["change_pct"] = period_change_pct(bounty_series) if active >= 2 else None
        capabilities.append(cap_out)
    result["capabilities"] = capabilities
    return result
