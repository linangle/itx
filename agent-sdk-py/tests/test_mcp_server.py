"""Tests for the MCP server's publishing invariants (skipped without the
`mcp` extra): every tool carries annotations and the money- or
reputation-affecting ones are marked destructive; the private key never
appears in any tool result, description or the server's instructions;
and configuration resolves flag > environment > default.

The hub is replaced with a canned fake, so these exercise the server's
own code paths (including the analytics-backed tools) without a node."""

import asyncio
import json
from typing import Any, Dict, List, Optional, Tuple

import pytest

mcp = pytest.importorskip("mcp")

from mcp.server.mcpserver.exceptions import ToolError  # noqa: E402

from itx_agent_sdk import Agent, HubError, mcp_server  # noqa: E402

OTHER = "03" + "cd" * 32

# The exact set of tools that may lock, spend, pay out or lose funds or
# reputation. A new tool that moves money must be added here *and* be
# annotated destructive, or `test_money_tools_are_marked_destructive`
# fails -- the point is that the list is reviewed, not inferred.
MONEY_OR_REPUTATION_TOOLS = {
    "post_task",
    "post_consensus_task",
    "post_disputable_task",
    "claim_task",
    "submit_work",
    "dispute_answer",
    "place_order",
    "withdraw_from_exchange",
}

# A valid argument set for every tool, so the leak test can call each
# one for real. Kept in one place so the "every listed tool was called"
# assertion below catches a tool added without a test entry.
TOOL_CALLS: Dict[str, Dict[str, Any]] = {
    "claim_faucet": {},
    "post_task": {"description": "d", "bounty": 10, "expected_output_hash": "ab" * 32},
    "post_consensus_task": {
        "description": "d",
        "bounty": 10,
        "num_assignees": 3,
        "join_window_minutes": 5,
        "submission_window_minutes": 5,
    },
    "post_disputable_task": {"description": "d", "bounty": 10, "dispute_window_minutes": 5},
    "confirm_task_funding": {"escrow_id": "e1"},
    "claim_task": {"task_id": "t1"},
    "submit_work": {"task_id": "t1", "output": "answer"},
    "dispute_answer": {"task_id": "t1", "reason": "wrong"},
    "confirm_dispute_funding": {"task_id": "t1", "escrow_id": "e1"},
    "deposit_to_exchange": {},
    "confirm_exchange_deposit": {"escrow_id": "e1"},
    "place_order": {"side": "buy", "price": 2, "quantity": 3},
    "cancel_order": {"order_id": "o1"},
    "withdraw_from_exchange": {"amount": 1},
    "get_health": {},
    "list_tasks": {},
    "get_task": {"task_id": "t1"},
    "get_reputation": {},
    "get_leaderboard": {},
    "get_market_summary": {},
    "get_market_series": {},
    "resolve_names": {"pubkeys": [OTHER]},
    "get_order_book": {},
    "get_exchange_account": {},
    "list_trades": {},
    "get_my_status": {},
    "find_matching_tasks": {},
    "get_activity_feed": {},
    "get_capability_trend": {},
    "get_market_overview": {},
    "get_price_history": {"interval_ms": 60_000},
    "get_market_depth": {},
    "get_rate_limit_status": {},
}


def _task(id_: str, poster: str = OTHER) -> Dict[str, Any]:
    return {
        "id": id_,
        "poster": poster,
        "claimant": None,
        "bounty": 10,
        "min_reputation": 0,
        "kind": "hash_match",
        "description": "d",
        "created_at": "2026-09-05T00:00:00+00:00",
    }


class FakeHub:
    """Stands in for `_ThrottledHubClient`: same method names, canned
    answers shaped like the hub's real DTOs."""

    ESCROW = {"escrow_id": "e1", "deposit_address": "addr", "required_amount": 110, "expires_at": "2026-09-05T00:10:00+00:00"}

    def __init__(self, base_url: str, rate_limiter: Any):
        self.base_url = base_url

    def faucet_claim(self, agent):
        return {"amount": 50_000_000}

    def create_task_escrow(self, agent, *a, **k):
        return dict(self.ESCROW)

    create_consensus_task_escrow = create_task_escrow
    create_disputable_task_escrow = create_task_escrow

    def create_dispute_escrow(self, agent, task_id, reason):
        return dict(self.ESCROW)

    def create_exchange_deposit(self, agent):
        return dict(self.ESCROW)

    def confirm_task_escrow(self, agent, escrow_id):
        return {"escrow_id": escrow_id, "status": "Open"}

    def confirm_dispute_escrow(self, agent, task_id, escrow_id):
        return {"task_id": task_id, "status": "Disputed"}

    def confirm_exchange_deposit(self, agent, escrow_id):
        return {"credited": 100}

    def get_task(self, task_id):
        return _task(task_id)

    def get_reputation(self, pubkey_hex):
        return {"pubkey": pubkey_hex, "completed": 3, "failed": 0, "earned": 100}

    def claim_task(self, agent, task_id):
        return {"id": task_id, "status": "Claimed"}

    def submit_task(self, agent, task_id, output):
        return {"id": task_id, "status": "Verified"}

    def get_exchange_account(self, pubkey_hex):
        return {"base_balance": 1000, "locked_base": 0, "compute_balance": 10, "locked_compute": 0}

    def place_order(self, agent, side, price, quantity):
        return {"id": "o1", "side": side, "price": price, "quantity": quantity, "filled": 0, "status": "open"}

    def cancel_order(self, agent, order_id):
        return {"id": order_id, "status": "cancelled"}

    def withdraw(self, agent, amount):
        return {"amount": amount}

    def get_health(self):
        return {"status": "ok", "chain_height": 7}

    def list_tasks_page(self, offset, limit, capability, status) -> Tuple[List[dict], Optional[int]]:
        return [_task("t1"), _task("t2")], 2

    def leaderboard_page(self, *a) -> Tuple[List[dict], Optional[int]]:
        return [{"pubkey": OTHER, "earned": 100}], 1

    def board_summary(self):
        return {"capabilities": [{"capability": "python", "bounty_series": [1, 2, 3, 4]}]}

    def board_series(self, capability, window_ms, buckets):
        return {"capability": capability, "posted_series": [1, 1, 3, 3], "bounty_series": [100, 100, 50, 50]}

    def resolve_names(self, pubkeys):
        return {p: "quiet-otter" for p in pubkeys}

    def get_order_book(self):
        return {
            "bids": [{"id": "b1", "price": 9, "quantity": 5, "filled": 1}],
            "asks": [{"id": "a1", "price": 11, "quantity": 2, "filled": 0}],
        }

    def list_trades_page(self, offset, limit) -> Tuple[List[dict], Optional[int]]:
        return [
            {"price": 10, "quantity": 1, "executed_at": "2026-09-05T00:00:30+00:00"},
            {"price": 12, "quantity": 2, "executed_at": "2026-09-05T00:00:10+00:00"},
        ], 2


@pytest.fixture
def server(monkeypatch, tmp_path):
    monkeypatch.setattr(mcp_server, "_ThrottledHubClient", FakeHub)
    key_file = tmp_path / "agent.key"
    srv = mcp_server.build_server("http://hub.test", str(key_file))
    private_hex = key_file.read_text(encoding="utf-8").strip()
    return srv, private_hex, Agent.from_private_key_hex(private_hex).pubkey_hex


def _tools(srv):
    return asyncio.run(srv.list_tools())


def _call(srv, name, args):
    return asyncio.run(srv.call_tool(name, args))


def test_every_tool_is_annotated_and_every_money_tool_is_destructive(server):
    srv, _, _ = server
    tools = {t.name: t for t in _tools(srv)}
    assert MONEY_OR_REPUTATION_TOOLS <= set(tools)

    for name, tool in tools.items():
        assert tool.annotations is not None, f"{name} has no annotations"
        ann = tool.annotations
        if name in MONEY_OR_REPUTATION_TOOLS:
            assert ann.read_only_hint is False, name
            assert ann.destructive_hint is True, name
            assert ann.idempotent_hint is False, name
        else:
            assert ann.destructive_hint is False, f"{name} is marked destructive but is not in the reviewed money set"


def test_read_only_tools_say_so(server):
    srv, _, _ = server
    tools = {t.name: t for t in _tools(srv)}
    for name in ("get_health", "list_tasks", "get_my_status", "find_matching_tasks", "get_market_depth"):
        assert tools[name].annotations.read_only_hint is True, name
    for name in ("claim_faucet", "confirm_task_funding", "cancel_order", "deposit_to_exchange"):
        assert tools[name].annotations.read_only_hint is False, name


def test_money_tools_take_explicit_amounts_with_no_defaults(server):
    srv, _, _ = server
    tools = {t.name: t for t in _tools(srv)}
    amount_fields = {
        "post_task": "bounty",
        "post_consensus_task": "bounty",
        "post_disputable_task": "bounty",
        "place_order": "quantity",
        "withdraw_from_exchange": "amount",
    }
    for name, field in amount_fields.items():
        schema = tools[name].input_schema if hasattr(tools[name], "input_schema") else tools[name].inputSchema
        assert field in schema.get("required", []), f"{name}.{field} must be required"
        assert "default" not in schema["properties"][field], f"{name}.{field} must not default"


def test_escrow_tools_return_the_deposit_details_as_structured_data(server):
    srv, _, _ = server
    for name in ("post_task", "post_consensus_task", "post_disputable_task", "dispute_answer", "deposit_to_exchange"):
        result = _call(srv, name, TOOL_CALLS[name]).model_dump(by_alias=True)
        assert not result.get("isError"), (name, result)
        structured = result.get("structuredContent") or json.loads(result["content"][0]["text"])
        payload = structured.get("result", structured)
        assert set(payload) >= {"escrow_id", "deposit_address", "required_amount", "expires_at"}, name


def test_no_tool_result_description_or_instruction_contains_the_private_key(server):
    srv, private_hex, pubkey_hex = server
    tools = _tools(srv)
    listed = {t.name for t in tools}
    assert listed == set(TOOL_CALLS), (
        f"tools without a test call: {listed - set(TOOL_CALLS)}; stale test entries: {set(TOOL_CALLS) - listed}"
    )

    assert pubkey_hex in srv.instructions
    assert private_hex not in srv.instructions
    for tool in tools:
        assert private_hex not in tool.model_dump_json()

    for name, args in TOOL_CALLS.items():
        result = _call(srv, name, args)
        dumped = result.model_dump(by_alias=True)
        assert not dumped.get("isError"), (name, dumped)
        assert private_hex not in result.model_dump_json(), name


def test_claim_task_refuses_the_agents_own_task_before_hitting_the_hub(server, monkeypatch):
    srv, _, pubkey_hex = server
    monkeypatch.setattr(FakeHub, "get_task", lambda self, task_id: _task(task_id, poster=pubkey_hex))
    # `ToolError` is the one exception type whose text the MCP runtime
    # forwards to the model; anything else is reported as a bare
    # "Error executing tool", which is useless to an agent.
    with pytest.raises(ToolError, match="cannot claim your own task"):
        _call(srv, "claim_task", {"task_id": "mine"})


def test_hub_rejections_reach_the_model_with_status_and_body(monkeypatch, tmp_path):
    # Through the real `_ThrottledHubClient` (not `FakeHub`), since the
    # translation from `HubError` to `ToolError` lives there.
    def reject(self, path, envelope):
        raise HubError(409, {"error": "faucet already claimed for this pubkey"})

    monkeypatch.setattr(mcp_server.HubClient, "_post", reject)
    srv = mcp_server.build_server("http://hub.test", str(tmp_path / "agent.key"))
    with pytest.raises(ToolError, match="409.*faucet already claimed"):
        _call(srv, "claim_faucet", {})


def test_place_order_checks_spendable_balance_before_hitting_the_hub(server):
    srv, _, _ = server
    # FakeHub reports 1000 spendable base; 2 * 600 exceeds it.
    with pytest.raises(ToolError, match="buy needs 1200 spendable base balance, this agent has 1000"):
        _call(srv, "place_order", {"side": "buy", "price": 600, "quantity": 2})


def test_main_resolves_hub_url_and_key_file_from_the_environment(monkeypatch, tmp_path):
    captured = {}

    class Stub:
        def run(self, transport):
            captured["transport"] = transport

    def fake_build_server(hub_url, key_file):
        captured["hub_url"] = hub_url
        captured["key_file"] = key_file
        return Stub()

    monkeypatch.setattr(mcp_server, "build_server", fake_build_server)
    monkeypatch.setenv("HOME", str(tmp_path))
    monkeypatch.setenv(mcp_server.ENV_HUB_URL, "http://from-env:1")
    monkeypatch.setenv(mcp_server.ENV_KEY_FILE, "~/keys/agent.key")
    monkeypatch.setattr("sys.argv", ["itx-agent-mcp-server"])

    mcp_server.main()
    assert captured == {
        "hub_url": "http://from-env:1",
        "key_file": str(tmp_path / "keys" / "agent.key"),
        "transport": "stdio",
    }

    monkeypatch.setattr("sys.argv", ["itx-agent-mcp-server", "--hub-url", "http://flag:2"])
    mcp_server.main()
    assert captured["hub_url"] == "http://flag:2"
