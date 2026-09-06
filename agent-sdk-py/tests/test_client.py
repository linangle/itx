"""Unit tests for `HubClient`'s request construction -- verifies each
wrapper method hits the right URL with the right payload shape, without
needing a live hub. Mocks at the `requests.Session` level (rather than
patching `HubClient._get`/`_post` themselves) so the real `_handle`
response-parsing logic is exercised too, not bypassed.

The signing itself is already exhaustively covered against the real Rust
implementation by `test_envelope_conformance.py`; these tests are only
about whether each method sends the envelope to the right place with the
right payload.
"""

from unittest.mock import ANY, MagicMock

import pytest

from itx_agent_sdk import Agent, HubClient, HubError


def make_client_with_mock_session() -> HubClient:
    client = HubClient("http://hub.test")
    client.session = MagicMock()
    return client


def mock_response(json_body=None, status_code: int = 200, headers: dict = None) -> MagicMock:
    resp = MagicMock()
    resp.ok = 200 <= status_code < 300
    resp.status_code = status_code
    resp.content = b"{}" if json_body is not None else b""
    resp.json.return_value = json_body
    resp.text = "" if json_body is None else str(json_body)
    # A real `requests.Response.headers` is a case-insensitive dict-like
    # with `.get`; mirrored here (rather than left as a MagicMock
    # attribute, which would return a truthy Mock for any `.get(...)`
    # instead of `None`) so `_get_with_total`'s header lookup behaves the
    # same in tests as it does against a real hub.
    resp.headers = headers or {}
    return resp


def test_list_tasks_hits_the_right_url_and_params():
    client = make_client_with_mock_session()
    client.session.get.return_value = mock_response([])
    client.list_tasks(offset=5, limit=10, capability="python")
    client.session.get.assert_called_once_with(
        "http://hub.test/tasks",
        params={"offset": 5, "limit": 10, "capability": "python"},
        timeout=ANY,
    )


def test_list_tasks_omits_absent_optional_params():
    client = make_client_with_mock_session()
    client.session.get.return_value = mock_response([])
    client.list_tasks()
    client.session.get.assert_called_once_with(
        "http://hub.test/tasks", params={"offset": 0}, timeout=ANY
    )


def test_get_task_hits_the_right_url():
    client = make_client_with_mock_session()
    client.session.get.return_value = mock_response({"id": "abc"})
    result = client.get_task("abc")
    client.session.get.assert_called_once_with(
        "http://hub.test/tasks/abc", params=None, timeout=ANY
    )
    assert result == {"id": "abc"}


def test_leaderboard_and_reputation_urls():
    client = make_client_with_mock_session()
    client.session.get.return_value = mock_response([])
    client.leaderboard()
    client.session.get.assert_called_with("http://hub.test/leaderboard", params=None, timeout=ANY)

    client.get_reputation("02aabbcc")
    client.session.get.assert_called_with(
        "http://hub.test/reputation/02aabbcc", params=None, timeout=ANY
    )


def test_faucet_claim_signs_a_null_payload():
    client = make_client_with_mock_session()
    client.session.post.return_value = mock_response({"amount": 50_000_000})
    agent = Agent.generate()

    client.faucet_claim(agent)

    args, kwargs = client.session.post.call_args
    assert args[0] == "http://hub.test/faucet"
    envelope = kwargs["json"]
    assert envelope["pubkey"] == agent.pubkey_hex
    assert envelope["payload"] is None


def test_create_task_sends_fields_in_struct_declaration_order():
    """Field order matters here -- see `Agent.build_envelope`'s
    docstring. `hub/src/handlers.rs::CreateTaskPayload` declares its
    fields as description, bounty, expected_output_hash, min_reputation,
    capabilities, in that order.
    """
    client = make_client_with_mock_session()
    client.session.post.return_value = mock_response({"id": "abc"})
    operator = Agent.generate()

    client.create_task(operator, "desc", 1000, "deadbeef", min_reputation=2, capabilities=["b", "a"])

    _, kwargs = client.session.post.call_args
    payload = kwargs["json"]["payload"]
    assert list(payload.keys()) == [
        "description",
        "bounty",
        "expected_output_hash",
        "min_reputation",
        "capabilities",
    ]
    assert payload["capabilities"] == ["a", "b"], "capabilities are sorted, matching a BTreeSet's own ordering"


def test_create_consensus_task_escrow_url_and_payload():
    client = make_client_with_mock_session()
    client.session.post.return_value = mock_response({"escrow_id": "e1"})
    agent = Agent.generate()

    client.create_consensus_task_escrow(agent, "desc", 900, 3, 30, 30)

    args, kwargs = client.session.post.call_args
    assert args[0] == "http://hub.test/tasks/consensus/escrow"
    payload = kwargs["json"]["payload"]
    assert list(payload.keys()) == [
        "description",
        "bounty",
        "num_assignees",
        "join_window_minutes",
        "submission_window_minutes",
        "min_reputation",
        "capabilities",
    ]


def test_confirm_task_escrow_uses_escrow_id_in_both_url_and_payload():
    client = make_client_with_mock_session()
    client.session.post.return_value = mock_response({"id": "t1"})
    agent = Agent.generate()

    client.confirm_task_escrow(agent, "e1")

    args, kwargs = client.session.post.call_args
    assert args[0] == "http://hub.test/tasks/escrow/e1/confirm"
    assert kwargs["json"]["payload"] == {"escrow_id": "e1"}


def test_claim_task_uses_the_task_id_in_both_url_and_payload():
    client = make_client_with_mock_session()
    client.session.post.return_value = mock_response({"id": "t1", "status": "Claimed"})
    agent = Agent.generate()

    client.claim_task(agent, "t1")

    args, kwargs = client.session.post.call_args
    assert args[0] == "http://hub.test/tasks/t1/claim"
    assert kwargs["json"]["payload"] == {"task_id": "t1"}


def test_submit_task_url_and_payload():
    client = make_client_with_mock_session()
    client.session.post.return_value = mock_response({"verified": True})
    agent = Agent.generate()

    client.submit_task(agent, "t1", "42")

    args, kwargs = client.session.post.call_args
    assert args[0] == "http://hub.test/tasks/t1/submit"
    assert kwargs["json"]["payload"] == {"task_id": "t1", "output": "42"}


def test_dispute_flow_urls_and_payloads():
    client = make_client_with_mock_session()
    client.session.post.return_value = mock_response({"id": "t1"})
    challenger = Agent.generate()
    operator = Agent.generate()

    client.create_dispute_escrow(challenger, "t1", "wrong answer")
    args, kwargs = client.session.post.call_args
    assert args[0] == "http://hub.test/tasks/t1/dispute/escrow"
    assert kwargs["json"]["payload"] == {"task_id": "t1", "reason": "wrong answer"}

    client.confirm_dispute_escrow(challenger, "t1", "e1")
    args, kwargs = client.session.post.call_args
    assert args[0] == "http://hub.test/tasks/t1/dispute/confirm"
    assert kwargs["json"]["payload"] == {"task_id": "t1", "escrow_id": "e1"}

    client.resolve_dispute(operator, "t1", "challenger_wins")
    args, kwargs = client.session.post.call_args
    assert args[0] == "http://hub.test/tasks/t1/dispute/resolve"
    assert kwargs["json"]["payload"] == {"task_id": "t1", "outcome": "challenger_wins"}


def test_non_ok_response_raises_hub_error_with_parsed_body():
    client = make_client_with_mock_session()
    client.session.post.return_value = mock_response({"error": "nope"}, status_code=403)
    agent = Agent.generate()

    with pytest.raises(HubError) as excinfo:
        client.claim_task(agent, "t1")

    assert excinfo.value.status_code == 403
    assert excinfo.value.body == {"error": "nope"}


def test_get_health_hits_the_right_url():
    client = make_client_with_mock_session()
    client.session.get.return_value = mock_response({"status": "ok", "chain_height": 42})
    result = client.get_health()
    client.session.get.assert_called_once_with(
        "http://hub.test/health", params=None, timeout=ANY
    )
    assert result == {"status": "ok", "chain_height": 42}


def test_list_tasks_passes_status_through():
    client = make_client_with_mock_session()
    client.session.get.return_value = mock_response([])
    client.list_tasks(capability="python", status="all")
    client.session.get.assert_called_once_with(
        "http://hub.test/tasks",
        params={"offset": 0, "capability": "python", "status": "all"},
        timeout=ANY,
    )


def test_list_tasks_page_returns_total_from_header():
    client = make_client_with_mock_session()
    client.session.get.return_value = mock_response([{"id": "a"}], headers={"x-total-count": "7"})
    items, total = client.list_tasks_page(offset=0, limit=5)
    assert items == [{"id": "a"}]
    assert total == 7


def test_leaderboard_page_with_pagination_search_and_sort():
    client = make_client_with_mock_session()
    client.session.get.return_value = mock_response([], headers={"x-total-count": "3"})
    items, total = client.leaderboard_page(offset=20, limit=10, q="alice", sort="completed", dir="asc")
    client.session.get.assert_called_once_with(
        "http://hub.test/leaderboard",
        params={"offset": 20, "limit": 10, "q": "alice", "sort": "completed", "dir": "asc"},
        timeout=ANY,
    )
    assert total == 3


def test_leaderboard_with_no_args_still_sends_params_none():
    """Regression guard: `leaderboard_page` builds `params={}` when
    nothing is set, but the call to the hub must still see `params=None`
    (matching `test_leaderboard_and_reputation_urls` above), not an empty
    dict -- `params or None` is what makes that so.
    """
    client = make_client_with_mock_session()
    client.session.get.return_value = mock_response([])
    client.leaderboard_page()
    client.session.get.assert_called_once_with(
        "http://hub.test/leaderboard", params=None, timeout=ANY
    )


def test_board_summary_hits_the_right_url():
    client = make_client_with_mock_session()
    client.session.get.return_value = mock_response({"capabilities": []})
    result = client.board_summary()
    client.session.get.assert_called_once_with(
        "http://hub.test/board/summary", params=None, timeout=ANY
    )
    assert result == {"capabilities": []}


def test_board_series_omits_absent_optional_params():
    client = make_client_with_mock_session()
    client.session.get.return_value = mock_response({"buckets": []})
    client.board_series()
    client.session.get.assert_called_once_with(
        "http://hub.test/board/series", params=None, timeout=ANY
    )


def test_board_series_passes_all_optional_params():
    client = make_client_with_mock_session()
    client.session.get.return_value = mock_response({"buckets": []})
    client.board_series(capability="python", window_ms=3600000, buckets=12)
    client.session.get.assert_called_once_with(
        "http://hub.test/board/series",
        params={"capability": "python", "window_ms": 3600000, "buckets": 12},
        timeout=ANY,
    )


def test_resolve_names_joins_pubkeys_with_commas():
    client = make_client_with_mock_session()
    client.session.get.return_value = mock_response({"02aa": "alice", "02bb": None})
    result = client.resolve_names(["02aa", "02bb"])
    client.session.get.assert_called_once_with(
        "http://hub.test/names", params={"pubkeys": "02aa,02bb"}, timeout=ANY
    )
    assert result == {"02aa": "alice", "02bb": None}


def test_create_exchange_deposit_signs_a_null_payload():
    client = make_client_with_mock_session()
    client.session.post.return_value = mock_response({"deposit_address": "addr1"})
    agent = Agent.generate()

    client.create_exchange_deposit(agent)

    args, kwargs = client.session.post.call_args
    assert args[0] == "http://hub.test/exchange/deposit"
    assert kwargs["json"]["payload"] is None


def test_confirm_exchange_deposit_url_and_payload():
    client = make_client_with_mock_session()
    client.session.post.return_value = mock_response({"base_balance": 100})
    agent = Agent.generate()

    client.confirm_exchange_deposit(agent, "e1")

    args, kwargs = client.session.post.call_args
    assert args[0] == "http://hub.test/exchange/deposit/e1/confirm"
    assert kwargs["json"]["payload"] == {"escrow_id": "e1"}


def test_place_order_sends_fields_in_struct_declaration_order():
    """`hub/src/handlers.rs::PlaceOrderPayload` declares side, price,
    quantity in that order.
    """
    client = make_client_with_mock_session()
    client.session.post.return_value = mock_response({"id": "o1", "status": "Open"})
    agent = Agent.generate()

    client.place_order(agent, "buy", 100, 5)

    args, kwargs = client.session.post.call_args
    assert args[0] == "http://hub.test/exchange/orders"
    payload = kwargs["json"]["payload"]
    assert list(payload.keys()) == ["side", "price", "quantity"]
    assert payload == {"side": "buy", "price": 100, "quantity": 5}


def test_cancel_order_uses_the_order_id_in_both_url_and_payload():
    client = make_client_with_mock_session()
    client.session.post.return_value = mock_response({"id": "o1", "status": "Cancelled"})
    agent = Agent.generate()

    client.cancel_order(agent, "o1")

    args, kwargs = client.session.post.call_args
    assert args[0] == "http://hub.test/exchange/orders/o1/cancel"
    assert kwargs["json"]["payload"] == {"order_id": "o1"}


def test_withdraw_url_and_payload():
    client = make_client_with_mock_session()
    client.session.post.return_value = mock_response({"amount": 500})
    agent = Agent.generate()

    client.withdraw(agent, 500)

    args, kwargs = client.session.post.call_args
    assert args[0] == "http://hub.test/exchange/withdraw"
    assert kwargs["json"]["payload"] == {"amount": 500}


def test_get_order_book_hits_the_right_url():
    client = make_client_with_mock_session()
    client.session.get.return_value = mock_response({"bids": [], "asks": []})
    result = client.get_order_book()
    client.session.get.assert_called_once_with(
        "http://hub.test/exchange/orders", params=None, timeout=ANY
    )
    assert result == {"bids": [], "asks": []}


def test_get_exchange_account_hits_the_right_url():
    client = make_client_with_mock_session()
    client.session.get.return_value = mock_response({"base_balance": 0})
    client.get_exchange_account("02aabbcc")
    client.session.get.assert_called_once_with(
        "http://hub.test/exchange/account/02aabbcc", params=None, timeout=ANY
    )


def test_list_trades_page_returns_total_from_header():
    client = make_client_with_mock_session()
    client.session.get.return_value = mock_response([{"id": "t1"}], headers={"x-total-count": "2"})
    items, total = client.list_trades_page(offset=0, limit=1)
    client.session.get.assert_called_once_with(
        "http://hub.test/exchange/trades", params={"offset": 0, "limit": 1}, timeout=ANY
    )
    assert items == [{"id": "t1"}]
    assert total == 2


def test_get_with_total_returns_none_when_header_absent():
    client = make_client_with_mock_session()
    client.session.get.return_value = mock_response([])
    _, total = client._get_with_total("/tasks", params={"offset": 0})
    assert total is None


def test_get_with_total_returns_none_when_header_unparseable():
    client = make_client_with_mock_session()
    client.session.get.return_value = mock_response([], headers={"x-total-count": "not-a-number"})
    _, total = client._get_with_total("/tasks", params={"offset": 0})
    assert total is None


def test_llms_txt_returns_plain_text_not_json():
    client = make_client_with_mock_session()
    resp = MagicMock()
    resp.text = "# itx agent hub"
    resp.raise_for_status = MagicMock()
    client.session.get.return_value = resp

    result = client.llms_txt()

    assert result == "# itx agent hub"
    client.session.get.assert_called_once_with("http://hub.test/llms.txt", timeout=ANY)


# -- redirects on signed writes -------------------------------------------


def test_signed_posts_do_not_follow_redirects():
    """`requests` rewrites a redirected POST as a GET, so a signed write
    to an http:// hub behind a redirecting proxy would land as a *read* of
    the same route and return the reply as if the write had happened.
    """
    client = make_client_with_mock_session()
    client.session.post.return_value = mock_response({"id": "o1"})

    client.place_order(Agent.generate(), "buy", 100, 5)

    _, kwargs = client.session.post.call_args
    assert kwargs["allow_redirects"] is False


def test_a_redirected_signed_post_raises_and_explains_the_http_base_url():
    client = make_client_with_mock_session()
    client.session.post.return_value = mock_response(
        None, status_code=301, headers={"location": "https://hub.test/exchange/orders"}
    )

    with pytest.raises(HubError) as excinfo:
        client.place_order(Agent.generate(), "buy", 100, 5)

    assert excinfo.value.status_code == 301
    body = excinfo.value.body
    assert "https://hub.test/exchange/orders" in body
    assert "http://" in body and "signature binds the request path" in body


def test_unsigned_reads_still_follow_redirects():
    """Nothing is bound to a path and no credential rides along, so a GET
    that gets redirected http->https should just get where it was going.
    """
    client = make_client_with_mock_session()
    client.session.get.return_value = mock_response({"status": "ok"})
    client.get_health()
    _, kwargs = client.session.get.call_args
    assert "allow_redirects" not in kwargs, "reads use requests' default, which follows"


# -- base URL validation ---------------------------------------------------


@pytest.mark.parametrize(
    "bad_url",
    ["https://hub.test/api", "https://hub.test/api/", "https://hub.test?token=x", "https://hub.test#frag"],
)
def test_a_base_url_with_a_path_query_or_fragment_is_rejected_at_construction(bad_url):
    """`HubClient` signs the bare route but sends `base_url + route`, so a
    prefix signs one path and posts another -- an unexplained 401 on every
    signed call, discovered at the worst possible moment.
    """
    with pytest.raises(ValueError, match="no path, query or fragment"):
        HubClient(bad_url)


def test_a_plain_host_base_url_is_accepted_with_or_without_a_trailing_slash():
    assert HubClient("https://hub.test").base_url == "https://hub.test"
    assert HubClient("https://hub.test/").base_url == "https://hub.test"
    assert HubClient("http://127.0.0.1:9100").base_url == "http://127.0.0.1:9100"


# -- id normalization ------------------------------------------------------

CANONICAL_ID = "3f2504e0-4f89-11d3-9a0c-0305e82c3301"


@pytest.mark.parametrize(
    "written_as",
    [
        CANONICAL_ID,
        CANONICAL_ID.upper(),
        CANONICAL_ID.replace("-", ""),
        "{" + CANONICAL_ID + "}",
        "urn:uuid:" + CANONICAL_ID,
    ],
)
def test_task_ids_are_canonicalised_in_both_the_signed_path_and_the_payload(written_as):
    """The hub recomputes the signing string from the *parsed* `Uuid`, so
    an unusually spelled id would produce a different string on each side
    and come back 401 rather than 404.
    """
    client = make_client_with_mock_session()
    client.session.post.return_value = mock_response({"id": CANONICAL_ID})

    client.claim_task(Agent.generate(), written_as)

    args, kwargs = client.session.post.call_args
    assert args[0] == f"http://hub.test/tasks/{CANONICAL_ID}/claim"
    assert kwargs["json"]["payload"] == {"task_id": CANONICAL_ID}


def test_every_signed_route_that_carries_an_id_canonicalises_it():
    client = make_client_with_mock_session()
    client.session.post.return_value = mock_response({"ok": True})
    agent = Agent.generate()
    loud = CANONICAL_ID.upper()

    for call, expected_path in [
        (lambda: client.submit_task(agent, loud, "42"), f"/tasks/{CANONICAL_ID}/submit"),
        (lambda: client.cancel_task(agent, loud), f"/tasks/{CANONICAL_ID}/cancel"),
        (lambda: client.confirm_task_escrow(agent, loud), f"/tasks/escrow/{CANONICAL_ID}/confirm"),
        (lambda: client.create_dispute_escrow(agent, loud, "why"), f"/tasks/{CANONICAL_ID}/dispute/escrow"),
        (lambda: client.confirm_dispute_escrow(agent, loud, loud), f"/tasks/{CANONICAL_ID}/dispute/confirm"),
        (lambda: client.resolve_dispute(agent, loud, "assignee_wins"), f"/tasks/{CANONICAL_ID}/dispute/resolve"),
        (lambda: client.confirm_exchange_deposit(agent, loud), f"/exchange/deposit/{CANONICAL_ID}/confirm"),
        (lambda: client.cancel_order(agent, loud), f"/exchange/orders/{CANONICAL_ID}/cancel"),
    ]:
        call()
        args, kwargs = client.session.post.call_args
        assert args[0] == f"http://hub.test{expected_path}"
        for key, value in kwargs["json"]["payload"].items():
            if key.endswith("_id"):
                assert value == CANONICAL_ID, key


def test_an_id_that_is_not_a_uuid_is_passed_through_untouched():
    """A bad id should earn the hub's own 400/404, not a client-side
    crash -- and the tests above use short ids like "t1" for readability.
    """
    client = make_client_with_mock_session()
    client.session.post.return_value = mock_response({"ok": True})
    client.claim_task(Agent.generate(), "not-a-uuid")
    args, _ = client.session.post.call_args
    assert args[0] == "http://hub.test/tasks/not-a-uuid/claim"


# -- list_tasks_scan -------------------------------------------------------


def paged_board(size: int, page_cap: int = 200):
    """A `list_tasks_page` stand-in over a board of `size` tasks numbered
    oldest first, truncating any page to `page_cap` the way the hub does.
    Records the (offset, limit) of every call it served.
    """
    board = [{"id": f"t{i}"} for i in range(size)]
    calls = []

    def list_tasks_page(offset, limit, capability, status):
        calls.append((offset, limit))
        limit = page_cap if limit is None else min(limit, page_cap)
        return board[offset : offset + limit], len(board)

    return list_tasks_page, calls


def test_scan_pages_past_the_hubs_200_row_cap():
    client = make_client_with_mock_session()
    client.list_tasks_page, calls = paged_board(450)

    items, total = client.list_tasks_scan()

    assert [t["id"] for t in items] == [f"t{i}" for i in range(450)]
    assert total == 450
    assert len(calls) == 3, calls


def test_scan_never_asks_for_more_than_the_hub_will_serve():
    client = make_client_with_mock_session()
    client.list_tasks_page, calls = paged_board(450)
    client.list_tasks_scan()
    assert all(limit <= 200 for _, limit in calls), calls


def test_scan_keeps_the_newest_tasks_when_the_board_is_bigger_than_the_bound():
    """The hub sorts oldest first and the board only grows, so a bounded
    scan that started at offset 0 would return pure history and drop the
    very tasks a "what is my status" question is about.
    """
    client = make_client_with_mock_session()
    client.list_tasks_page, _ = paged_board(1300)

    items, total = client.list_tasks_scan(max_tasks=500)

    assert total == 1300
    assert [t["id"] for t in items] == [f"t{i}" for i in range(800, 1300)]


def test_scan_stops_at_one_page_when_the_whole_board_fits():
    client = make_client_with_mock_session()
    client.list_tasks_page, calls = paged_board(12)

    items, total = client.list_tasks_scan()

    assert len(items) == 12 and total == 12
    assert len(calls) == 1, "a board smaller than one page costs one request"


def test_scan_only_fetches_as_much_as_it_was_asked_for():
    client = make_client_with_mock_session()
    client.list_tasks_page, calls = paged_board(1000)

    items, _ = client.list_tasks_scan(max_tasks=20)

    assert [t["id"] for t in items] == [f"t{i}" for i in range(980, 1000)]
    assert all(limit <= 20 for _, limit in calls), calls


def test_scan_falls_back_to_a_forward_walk_when_the_hub_sends_no_total():
    """Without `X-Total-Count` there is no way to find the far end of the
    board, so forward from the start is all that is left -- but it must
    still page rather than trusting one oversized request.
    """
    client = make_client_with_mock_session()
    board = [{"id": f"t{i}"} for i in range(350)]

    def list_tasks_page(offset, limit, capability, status):
        limit = 200 if limit is None else min(limit, 200)
        return board[offset : offset + limit], None

    client.list_tasks_page = list_tasks_page

    items, total = client.list_tasks_scan()

    assert total is None
    assert [t["id"] for t in items] == [f"t{i}" for i in range(350)]


def test_scan_passes_the_filters_through():
    client = make_client_with_mock_session()
    seen = []

    def list_tasks_page(offset, limit, capability, status):
        seen.append((capability, status))
        return [], 0

    client.list_tasks_page = list_tasks_page
    client.list_tasks_scan(capability="python", status="all")
    assert seen == [("python", "all")]
