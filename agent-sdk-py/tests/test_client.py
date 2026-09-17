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

import hashlib
from unittest.mock import ANY, MagicMock

import pytest

from itx_agent_sdk import Agent, HubClient, HubError
from itx_agent_sdk.client import FaucetSolveTimeout, solve_faucet_challenge


# The hub these tests sign for. Supplied to the constructor so no test
# here has to answer the `/health` lookup a fresh client would otherwise
# make before its first signed call; `test_hub_id_*` below cover that.
HUB_ID = "02" + "ab" * 32


def make_client_with_mock_session() -> HubClient:
    client = HubClient("http://hub.test", hub_id=HUB_ID)
    client.session = MagicMock()
    return client


def test_hub_id_is_read_from_health_once_and_kept():
    client = HubClient("http://hub.test")
    client.session = MagicMock()
    client.session.get.return_value = mock_response({"status": "ok", "chain_height": 1, "operator": HUB_ID})
    assert client.hub_id() == HUB_ID
    assert client.hub_id() == HUB_ID
    client.session.get.assert_called_once_with("http://hub.test/health", timeout=ANY)


def test_a_hub_that_names_no_operator_cannot_be_signed_for():
    client = HubClient("http://hub.test")
    client.session = MagicMock()
    client.session.get.return_value = mock_response({"status": "ok", "chain_height": 1})
    with pytest.raises(HubError, match="names no operator"):
        client.hub_id()


def test_a_degraded_hub_still_says_who_it_is():
    # The node being down is no reason a client cannot learn who it is
    # talking to; the hub puts `operator` on the 503 as well.
    client = HubClient("http://hub.test")
    client.session = MagicMock()
    client.session.get.return_value = mock_response({"status": "degraded", "operator": HUB_ID}, status_code=503)
    assert client.hub_id() == HUB_ID


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


def test_faucet_challenge_signs_a_null_payload():
    client = make_client_with_mock_session()
    client.session.post.return_value = mock_response({"challenge_id": "x"})
    agent = Agent.generate()

    client.faucet_challenge(agent)

    args, kwargs = client.session.post.call_args
    assert args[0] == "http://hub.test/faucet/challenge"
    envelope = kwargs["json"]
    assert envelope["pubkey"] == agent.pubkey_hex
    assert envelope["payload"] is None


def test_faucet_claim_sends_the_challenge_id_and_solution():
    client = make_client_with_mock_session()
    client.session.post.return_value = mock_response({"amount": 50_000_000})
    agent = Agent.generate()

    client.faucet_claim(agent, "0F5F1E1A-0000-4000-8000-00000000ABCD", 12345)

    args, kwargs = client.session.post.call_args
    assert args[0] == "http://hub.test/faucet"
    payload = kwargs["json"]["payload"]
    # Canonicalised on the way out, like every other id: the hub
    # re-serialises the UUID it parsed, so an uppercase one signed
    # verbatim would produce a different signing string and a 401.
    assert payload == {
        "challenge_id": "0f5f1e1a-0000-4000-8000-00000000abcd",
        "solution": 12345,
    }


def _challenge(expected_hashes: int = 64) -> dict:
    """A challenge shaped exactly like the hub's, at a difficulty a test
    clears in microseconds."""
    target = (1 << 256) // expected_hashes
    return {
        "challenge_id": "0f5f1e1a-0000-4000-8000-00000000abcd",
        "server_nonce": "ab" * 32,
        "pubkey": "02" + "cd" * 32,
        "action": "faucet",
        "target": f"{target:064x}",
        "expected_hashes": expected_hashes,
        "preimage_template": "0f5f1e1a-0000-4000-8000-00000000abcd:"
        + "ab" * 32
        + ":02"
        + "cd" * 32
        + ":faucet:{solution}",
    }


def test_solving_a_challenge_produces_a_solution_that_meets_the_target():
    challenge = _challenge()
    solution = solve_faucet_challenge(challenge)

    preimage = challenge["preimage_template"].replace("{solution}", str(solution))
    digest = hashlib.sha256(preimage.encode()).digest()
    # Little-endian, which is the whole subtlety -- see
    # `solve_faucet_challenge`'s docstring.
    assert int.from_bytes(digest, "little") <= int(challenge["target"], 16)


def test_solving_gives_up_rather_than_hanging_on_an_impossible_target():
    """A difficulty this machine cannot reach must fail loudly. Without
    a bound, an operator raising the knob turns every client into a hang
    with no output."""
    impossible = _challenge()
    impossible["target"] = f"{0:064x}"
    with pytest.raises(FaucetSolveTimeout):
        solve_faucet_challenge(impossible, max_seconds=0.25)


def test_claim_faucet_walks_all_three_steps():
    client = make_client_with_mock_session()
    challenge = _challenge()
    client.session.post.side_effect = [
        mock_response(challenge),
        mock_response({"amount": 50_000_000}),
    ]
    agent = Agent.generate()

    result = client.claim_faucet(agent)

    assert result == {"amount": 50_000_000}
    first, second = client.session.post.call_args_list
    assert first[0][0] == "http://hub.test/faucet/challenge"
    assert second[0][0] == "http://hub.test/faucet"
    assert second[1]["json"]["payload"]["challenge_id"] == challenge["challenge_id"]


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


def test_create_disputable_task_escrow_defaults_min_reputation_to_one():
    """The hub's own default for this kind, which pays whatever answer it
    is given. Sent explicitly either way, because the hub signs over the
    filled-in value -- see `Agent.build_envelope`.
    """
    client = make_client_with_mock_session()
    client.session.post.return_value = mock_response({"escrow_id": "e1"})
    agent = Agent.generate()

    client.create_disputable_task_escrow(agent, "desc", 900, 30)
    args, kwargs = client.session.post.call_args
    assert args[0] == "http://hub.test/tasks/disputable/escrow"
    payload = kwargs["json"]["payload"]
    assert list(payload.keys()) == [
        "description",
        "bounty",
        "dispute_window_minutes",
        "min_reputation",
        "capabilities",
    ]
    assert payload["min_reputation"] == 1

    client.create_disputable_task_escrow(agent, "desc", 900, 30, min_reputation=0)
    assert client.session.post.call_args[1]["json"]["payload"]["min_reputation"] == 0


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


def test_get_with_total_reads_the_header_when_it_is_there():
    client = make_client_with_mock_session()
    client.session.get.return_value = mock_response([{"id": "t1"}], headers={"x-total-count": "2"})
    items, total = client._get_with_total("/tasks", params={"offset": 0, "limit": 1})
    client.session.get.assert_called_once_with(
        "http://hub.test/tasks", params={"offset": 0, "limit": 1}, timeout=ANY
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
    client.session.post.return_value = mock_response({"id": "t1"})

    client.submit_task(Agent.generate(), "11111111-1111-4111-8111-111111111111", "42")

    _, kwargs = client.session.post.call_args
    assert kwargs["allow_redirects"] is False


def test_a_redirected_signed_post_raises_and_explains_the_http_base_url():
    client = make_client_with_mock_session()
    client.session.post.return_value = mock_response(
        None, status_code=301, headers={"location": "https://hub.test/tasks"}
    )

    with pytest.raises(HubError) as excinfo:
        client.create_task(Agent.generate(), "work", 10, "cafe")

    assert excinfo.value.status_code == 301
    body = excinfo.value.body
    assert "https://hub.test/tasks" in body
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


# -- wallet ---------------------------------------------------------------

from itx_agent_sdk.client import HUB_TRANSACTION_FEE, InsufficientFunds, plan_spend  # noqa: E402

OUT_A = "aa" * 32
OUT_B = "bb" * 32
OUT_C = "cc" * 32


def _wallet(agent: Agent) -> dict:
    return {
        "pubkey": agent.pubkey_hex,
        "balance": 9_000,
        "pending": 3_000,
        "outputs": [
            {"hash": OUT_A, "value": 7_000, "pending": False},
            {"hash": OUT_B, "value": 3_000, "pending": True},
            {"hash": OUT_C, "value": 2_000, "pending": False},
        ],
    }


def test_get_wallet_reads_the_wallet_route():
    client = make_client_with_mock_session()
    client.session.get.return_value = mock_response({"pubkey": "02" + "00" * 32, "balance": 0, "pending": 0, "outputs": []})
    client.get_wallet("02" + "00" * 32)
    client.session.get.assert_called_once_with("http://hub.test/wallet/02" + "00" * 32, params=None, timeout=ANY)


def test_plan_spend_takes_the_largest_unpending_outputs_until_covered():
    outputs = _wallet(Agent.generate())["outputs"]
    assert [o["hash"] for o in plan_spend(outputs, 7_000)] == [OUT_A]
    assert [o["hash"] for o in plan_spend(outputs, 7_001)] == [OUT_A, OUT_C], "the pending one is skipped, not spent"
    # Order on the wire does not matter: the plan sorts.
    assert [o["hash"] for o in plan_spend(list(reversed(outputs)), 7_001)] == [OUT_A, OUT_C]
    with pytest.raises(InsufficientFunds) as excinfo:
        plan_spend(outputs, 9_001)
    assert excinfo.value.available == 9_000 and excinfo.value.needed == 9_001


def test_send_signs_each_input_pays_the_recipient_and_returns_the_change():
    client = make_client_with_mock_session()
    agent = Agent.generate()
    client.session.get.return_value = mock_response(_wallet(agent))
    client.session.post.return_value = mock_response({"tx_hash": "dd" * 32, "fee": HUB_TRANSACTION_FEE, "outputs": []})

    to = "03" + "cd" * 32
    client.send(agent, to, 6_500)

    args, kwargs = client.session.post.call_args
    assert args[0] == "http://hub.test/wallet/send"
    payload = kwargs["json"]["payload"]
    assert list(payload.keys()) == ["inputs", "outputs"], "field order is signing order"
    assert [list(i.keys()) for i in payload["inputs"]] == [["output", "signature"]] * 2
    assert [i["output"] for i in payload["inputs"]] == [OUT_A, OUT_C], "7000 alone is short of 6500 + fee"
    for i in payload["inputs"]:
        assert i["signature"] == agent.sign_output(i["output"]), "each input signed by the spending key"
    assert payload["outputs"] == [
        {"pubkey": to, "value": 6_500},
        {"pubkey": agent.pubkey_hex, "value": 9_000 - 6_500 - HUB_TRANSACTION_FEE},
    ]


def test_send_with_nothing_left_over_adds_no_change_output():
    client = make_client_with_mock_session()
    agent = Agent.generate()
    client.session.get.return_value = mock_response(_wallet(agent))
    client.session.post.return_value = mock_response({"tx_hash": "dd" * 32, "fee": HUB_TRANSACTION_FEE, "outputs": []})

    client.send(agent, "03" + "cd" * 32, 7_000 - HUB_TRANSACTION_FEE)

    payload = client.session.post.call_args.kwargs["json"]["payload"]
    assert [i["output"] for i in payload["inputs"]] == [OUT_A]
    assert len(payload["outputs"]) == 1


def test_send_refuses_before_signing_or_posting_when_the_balance_is_short():
    client = make_client_with_mock_session()
    agent = Agent.generate()
    client.session.get.return_value = mock_response(_wallet(agent))
    with pytest.raises(InsufficientFunds):
        client.send(agent, "03" + "cd" * 32, 9_000)
    client.session.post.assert_not_called()
    with pytest.raises(ValueError):
        client.send(agent, "03" + "cd" * 32, 0)
    with pytest.raises(ValueError):
        client.send(agent, "03" + "cd" * 32, 100, fee=HUB_TRANSACTION_FEE - 1)


def test_fund_escrow_pays_the_reservation_its_required_amount():
    client = make_client_with_mock_session()
    agent = Agent.generate()
    client.session.get.return_value = mock_response(_wallet(agent))
    client.session.post.return_value = mock_response({"tx_hash": "dd" * 32, "fee": HUB_TRANSACTION_FEE, "outputs": []})

    client.fund_escrow(agent, {"escrow_id": "e1", "deposit_address": "02" + "ee" * 32, "required_amount": 2_000})

    payload = client.session.post.call_args.kwargs["json"]["payload"]
    assert payload["outputs"][0] == {"pubkey": "02" + "ee" * 32, "value": 2_000}


def test_wait_for_task_funding_waits_through_409s_and_raises_anything_else(monkeypatch):
    client = make_client_with_mock_session()
    agent = Agent.generate()
    monkeypatch.setattr("itx_agent_sdk.client.time.sleep", lambda s: None)
    client.session.post.side_effect = [
        mock_response({"error": "escrow underfunded"}, status_code=409),
        mock_response({"error": "escrow underfunded"}, status_code=409),
        mock_response({"id": "t1", "status": "Open"}),
    ]
    assert client.wait_for_task_funding(agent, "e1") == {"id": "t1", "status": "Open"}
    assert client.session.post.call_count == 3

    client.session.post.side_effect = [mock_response({"error": "not yours"}, status_code=403)]
    with pytest.raises(HubError):
        client.wait_for_task_funding(agent, "e1")

    ticks = iter([0.0, 0.0, 10.0])
    monkeypatch.setattr("itx_agent_sdk.client.time.monotonic", lambda: next(ticks))
    client.session.post.side_effect = [
        mock_response({"error": "escrow underfunded"}, status_code=409),
        mock_response({"error": "escrow underfunded"}, status_code=409),
    ]
    with pytest.raises(TimeoutError):
        client.wait_for_task_funding(agent, "e1", timeout_seconds=5)
