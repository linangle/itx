"""Unit tests for the pure analytics functions in `analytics.py` --
hand-built fixtures with known expected output, no hub involved. See
that module's docstring for why each algorithm is shaped the way it is.
"""

from itx_agent_sdk.analytics import (
    capability_trend,
    market_overview,
    period_change_pct,
)


# -- period_change_pct -------------------------------------------------


def test_period_change_pct_none_below_two_buckets():
    assert period_change_pct([]) is None
    assert period_change_pct([5]) is None


def test_period_change_pct_none_when_earlier_half_is_zero():
    assert period_change_pct([0, 0, 5, 5]) is None


def test_period_change_pct_computes_period_over_period_increase():
    # earlier = 2+2 = 4, later = 6+6 = 12 -> (12-4)/4*100 = 200
    assert period_change_pct([2, 2, 6, 6]) == 200.0


def test_period_change_pct_computes_period_over_period_decrease():
    # earlier = 10+10=20, later = 5+5=10 -> (10-20)/20*100 = -50
    assert period_change_pct([10, 10, 5, 5]) == -50.0


def test_period_change_pct_odd_length_splits_by_floor_midpoint():
    # midpoint = 5 // 2 = 2; earlier = series[:2] = [1,1]=2, later = series[2:] = [1,1,10]=12
    assert period_change_pct([1, 1, 1, 1, 10]) == 500.0


# -- capability_trend ----------------------------------------------------


def test_capability_trend_adds_posted_and_bounty_change_pct():
    series_dto = {
        "capability": "python",
        "window_ms": 3600000,
        "buckets": 4,
        "posted_series": [1, 1, 3, 3],
        "bounty_series": [100, 100, 50, 50],
    }
    result = capability_trend(series_dto)
    assert result["posted_change_pct"] == 200.0
    assert result["bounty_change_pct"] == -50.0
    # original fields preserved
    assert result["capability"] == "python"
    assert result["buckets"] == 4


def test_capability_trend_handles_missing_series_gracefully():
    result = capability_trend({"capability": "python"})
    assert result["posted_change_pct"] is None
    assert result["bounty_change_pct"] is None


# -- market_overview -------------------------------------------------------


def test_market_overview_adds_change_pct_per_capability():
    summary_dto = {
        "window_ms": 3600000,
        "buckets": 4,
        "capabilities": [
            {
                "capability": "python",
                "open": 3,
                "open_bounty": 300,
                "posted": 10,
                "posted_series": [1, 2, 3, 4],
                "bounty_series": [100, 100, 50, 50],
            },
            {
                "capability": "rust",
                "open": 1,
                "open_bounty": 50,
                "posted": 1,
                # only one active bucket -- below the active>=2 gate
                "bounty_series": [0, 0, 0, 50],
            },
        ],
    }
    result = market_overview(summary_dto)
    caps = {c["capability"]: c for c in result["capabilities"]}
    assert caps["python"]["change_pct"] == -50.0
    assert caps["rust"]["change_pct"] is None
    # untouched fields still present
    assert caps["python"]["open_bounty"] == 300


def test_market_overview_leaves_capabilities_with_no_activity_at_none():
    summary_dto = {"capabilities": [{"capability": "empty", "bounty_series": [0, 0, 0, 0]}]}
    result = market_overview(summary_dto)
    assert result["capabilities"][0]["change_pct"] is None
