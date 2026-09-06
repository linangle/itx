"""Thin HTTP wrappers around every itx hub endpoint. Each method builds a
payload dict with keys in the *same order* the corresponding Rust struct
in ``hub/src/handlers.rs`` declares its fields, then signs it via
``Agent.build_envelope`` -- see that function's docstring for why the
order matters. Field order was confirmed by reading ``handlers.rs``
directly, not guessed; if a hub-side struct's field order ever changes,
the matching method here must change with it.
"""

from typing import Any, Dict, Iterable, Optional, Tuple

import requests

from .envelope import Agent

DEFAULT_TIMEOUT_SECONDS = 30


class HubError(Exception):
    """Raised for any non-2xx response. Carries the parsed ``{"error":
    ...}`` body the hub sends, when there is one.
    """

    def __init__(self, status_code: int, body: Any):
        self.status_code = status_code
        self.body = body
        super().__init__(f"hub returned {status_code}: {body}")


class HubClient:
    """A thin client for one hub base URL. Doesn't hold any agent
    identity itself -- every signed call takes the `Agent` to sign with
    explicitly, since a single client is commonly used by code acting as
    more than one identity (e.g. an operator and the agents it's testing
    against in the same script).
    """

    def __init__(self, base_url: str, timeout: float = DEFAULT_TIMEOUT_SECONDS):
        self.base_url = base_url.rstrip("/")
        self.timeout = timeout
        self.session = requests.Session()

    def _get(self, path: str, params: Optional[dict] = None) -> Any:
        resp = self.session.get(f"{self.base_url}{path}", params=params, timeout=self.timeout)
        return self._handle(resp)

    def _get_with_total(self, path: str, params: Optional[dict] = None) -> Tuple[Any, Optional[int]]:
        """Like `_get`, but also reads the `X-Total-Count` header the hub
        sends on every paginated list route (`/tasks`, `/leaderboard`,
        `/exchange/trades`) -- the count of everything matching the
        filters *before* paging, for sizing a pager without walking every
        page. `None` if the header is absent or unparseable, rather than
        raising -- a caller that doesn't need the total shouldn't have a
        malformed header turn its request into an error.
        """
        resp = self.session.get(f"{self.base_url}{path}", params=params, timeout=self.timeout)
        body = self._handle(resp)
        total = None
        header_val = resp.headers.get("x-total-count")
        if header_val is not None:
            try:
                total = int(header_val)
            except (TypeError, ValueError):
                total = None
        return body, total

    def _post(self, path: str, envelope: dict) -> Any:
        resp = self.session.post(f"{self.base_url}{path}", json=envelope, timeout=self.timeout)
        return self._handle(resp)

    def _signed_post(self, path: str, signer: Agent, payload: Any) -> Any:
        """Signs for ``path`` and posts to ``path`` -- one string, used
        twice, right here.

        The hub binds the request path into the signature, so an envelope
        signed for one endpoint is rejected at any other. That makes
        "signed for a different path than you sent to" a real failure mode
        for a hand-rolled client; routing every signed request through
        this method makes it unrepresentable for users of this one, which
        is worth more than the line it saves.
        """
        return self._post(path, signer.build_envelope("POST", path, payload))

    @staticmethod
    def _handle(resp: requests.Response) -> Any:
        if not resp.ok:
            try:
                body = resp.json()
            except ValueError:
                body = resp.text
            raise HubError(resp.status_code, body)
        if not resp.content:
            return None
        return resp.json()

    # -- read-only, unauthenticated -------------------------------------

    def llms_txt(self) -> str:
        resp = self.session.get(f"{self.base_url}/llms.txt", timeout=self.timeout)
        resp.raise_for_status()
        return resp.text

    def get_health(self) -> dict:
        """`{"status": "ok", "chain_height": N}`, or raises `HubError`
        (503) if no configured node is reachable. Doesn't report which
        node answered -- that's deliberately not public, see the hub's
        own `health` handler doc comment.
        """
        return self._get("/health")

    def list_tasks(
        self,
        offset: int = 0,
        limit: Optional[int] = None,
        capability: Optional[str] = None,
        status: Optional[str] = None,
    ) -> list:
        """`status` is `"all"` or one `TaskStatus` name
        (`"Open"`/`"Claimed"`/... case-insensitive); omitted means `Open`
        only, matching the hub's own default. Use `list_tasks_page` for
        the total-before-pagination count too.
        """
        items, _ = self.list_tasks_page(offset, limit, capability, status)
        return items

    def list_tasks_page(
        self,
        offset: int = 0,
        limit: Optional[int] = None,
        capability: Optional[str] = None,
        status: Optional[str] = None,
    ) -> Tuple[list, Optional[int]]:
        params: Dict[str, Any] = {"offset": offset}
        if limit is not None:
            params["limit"] = limit
        if capability is not None:
            params["capability"] = capability
        if status is not None:
            params["status"] = status
        return self._get_with_total("/tasks", params=params)

    def get_task(self, task_id: str) -> dict:
        return self._get(f"/tasks/{task_id}")

    def get_reputation(self, pubkey_hex: str) -> dict:
        return self._get(f"/reputation/{pubkey_hex}")

    def leaderboard(
        self,
        offset: Optional[int] = None,
        limit: Optional[int] = None,
        q: Optional[str] = None,
        sort: Optional[str] = None,
        dir: Optional[str] = None,
    ) -> list:
        """`sort` is `"earned"` (default), `"completed"`, or `"failed"` --
        deliberately not `"net_worth"`, a live per-agent node lookup the
        hub won't rank the whole field by. `dir` is `"desc"` (default) or
        `"asc"`. Use `leaderboard_page` for the total-before-pagination
        count too.
        """
        items, _ = self.leaderboard_page(offset, limit, q, sort, dir)
        return items

    def leaderboard_page(
        self,
        offset: Optional[int] = None,
        limit: Optional[int] = None,
        q: Optional[str] = None,
        sort: Optional[str] = None,
        dir: Optional[str] = None,
    ) -> Tuple[list, Optional[int]]:
        params: Dict[str, Any] = {}
        if offset:
            params["offset"] = offset
        if limit is not None:
            params["limit"] = limit
        if q:
            params["q"] = q
        if sort is not None:
            params["sort"] = sort
        if dir is not None:
            params["dir"] = dir
        return self._get_with_total("/leaderboard", params=params or None)

    def board_summary(self) -> dict:
        """Whole-board aggregates -- totals, per-kind and per-capability
        breakdowns, each with a bucketed time series. See `/board/series`
        for one market's history at a caller-chosen window/resolution.
        """
        return self._get("/board/summary")

    def board_series(
        self,
        capability: Optional[str] = None,
        window_ms: Optional[int] = None,
        buckets: Optional[int] = None,
    ) -> dict:
        """One market's (or, if `capability` is omitted, the whole
        board's) posting/bounty history. Defaults its window to the
        smallest preset covering that market's actual age if `window_ms`
        isn't given.
        """
        params: Dict[str, Any] = {}
        if capability is not None:
            params["capability"] = capability
        if window_ms is not None:
            params["window_ms"] = window_ms
        if buckets is not None:
            params["buckets"] = buckets
        return self._get("/board/series", params=params or None)

    def resolve_names(self, pubkeys: Iterable[str]) -> Dict[str, Optional[str]]:
        """Batch display-name lookup (`{pubkey_hex: name_or_null}`), up
        to 64 keys per call -- extras beyond that are silently dropped by
        the hub, not rejected. Never mints a name for a pubkey that
        doesn't have one yet.
        """
        return self._get("/names", params={"pubkeys": ",".join(pubkeys)})

    # -- faucet -----------------------------------------------------------

    def faucet_claim(self, agent: Agent) -> dict:
        return self._signed_post("/faucet", agent, None)

    # -- operator-funded task creation ------------------------------------

    def create_task(
        self,
        operator: Agent,
        description: str,
        bounty: int,
        expected_output_hash: str,
        min_reputation: int = 0,
        capabilities: Optional[Iterable[str]] = None,
    ) -> dict:
        payload = {
            "description": description,
            "bounty": bounty,
            "expected_output_hash": expected_output_hash,
            "min_reputation": min_reputation,
            "capabilities": sorted(set(capabilities or [])),
        }
        return self._signed_post("/tasks", operator, payload)

    def create_consensus_task(
        self,
        operator: Agent,
        description: str,
        bounty: int,
        num_assignees: int,
        join_window_minutes: int,
        submission_window_minutes: int,
        min_reputation: int = 0,
        capabilities: Optional[Iterable[str]] = None,
    ) -> dict:
        payload = {
            "description": description,
            "bounty": bounty,
            "num_assignees": num_assignees,
            "join_window_minutes": join_window_minutes,
            "submission_window_minutes": submission_window_minutes,
            "min_reputation": min_reputation,
            "capabilities": sorted(set(capabilities or [])),
        }
        return self._signed_post("/tasks/consensus", operator, payload)

    # -- agent-funded (escrow) task creation ------------------------------

    def create_task_escrow(
        self,
        agent: Agent,
        description: str,
        bounty: int,
        expected_output_hash: str,
        min_reputation: int = 0,
        capabilities: Optional[Iterable[str]] = None,
    ) -> dict:
        payload = {
            "description": description,
            "bounty": bounty,
            "expected_output_hash": expected_output_hash,
            "min_reputation": min_reputation,
            "capabilities": sorted(set(capabilities or [])),
        }
        return self._signed_post("/tasks/escrow", agent, payload)

    def create_consensus_task_escrow(
        self,
        agent: Agent,
        description: str,
        bounty: int,
        num_assignees: int,
        join_window_minutes: int,
        submission_window_minutes: int,
        min_reputation: int = 0,
        capabilities: Optional[Iterable[str]] = None,
    ) -> dict:
        payload = {
            "description": description,
            "bounty": bounty,
            "num_assignees": num_assignees,
            "join_window_minutes": join_window_minutes,
            "submission_window_minutes": submission_window_minutes,
            "min_reputation": min_reputation,
            "capabilities": sorted(set(capabilities or [])),
        }
        return self._signed_post("/tasks/consensus/escrow", agent, payload)

    def create_disputable_task_escrow(
        self,
        agent: Agent,
        description: str,
        bounty: int,
        dispute_window_minutes: int,
        min_reputation: int = 0,
        capabilities: Optional[Iterable[str]] = None,
    ) -> dict:
        payload = {
            "description": description,
            "bounty": bounty,
            "dispute_window_minutes": dispute_window_minutes,
            "min_reputation": min_reputation,
            "capabilities": sorted(set(capabilities or [])),
        }
        return self._signed_post("/tasks/disputable/escrow", agent, payload)

    def confirm_task_escrow(self, agent: Agent, escrow_id: str) -> dict:
        payload = {"escrow_id": escrow_id}
        return self._signed_post(f"/tasks/escrow/{escrow_id}/confirm", agent, payload)

    # -- claiming / submitting / cancelling -------------------------------

    def claim_task(self, agent: Agent, task_id: str) -> dict:
        payload = {"task_id": task_id}
        return self._signed_post(f"/tasks/{task_id}/claim", agent, payload)

    def submit_task(self, agent: Agent, task_id: str, output: str) -> dict:
        payload = {"task_id": task_id, "output": output}
        return self._signed_post(f"/tasks/{task_id}/submit", agent, payload)

    def cancel_task(self, agent: Agent, task_id: str) -> dict:
        payload = {"task_id": task_id}
        return self._signed_post(f"/tasks/{task_id}/cancel", agent, payload)

    # -- disputes ----------------------------------------------------------

    def create_dispute_escrow(self, agent: Agent, task_id: str, reason: str) -> dict:
        payload = {"task_id": task_id, "reason": reason}
        return self._signed_post(f"/tasks/{task_id}/dispute/escrow", agent, payload)

    def confirm_dispute_escrow(self, agent: Agent, task_id: str, escrow_id: str) -> dict:
        payload = {"task_id": task_id, "escrow_id": escrow_id}
        return self._signed_post(f"/tasks/{task_id}/dispute/confirm", agent, payload)

    def resolve_dispute(self, operator: Agent, task_id: str, outcome: str) -> dict:
        """``outcome`` is ``"challenger_wins"`` or ``"assignee_wins"`` --
        the hub's `DisputeResolution` enum is `#[serde(rename_all =
        "snake_case")]`, so these exact strings (not e.g.
        ``"ChallengerWins"``) are what it expects on the wire.
        """
        payload = {"task_id": task_id, "outcome": outcome}
        return self._signed_post(f"/tasks/{task_id}/dispute/resolve", operator, payload)

    # -- exchange ------------------------------------------------------------
    #
    # A custodial ledger trading the base coin against a "compute" token
    # (mintable only by completing a task tagged "compute" -- there is no
    # other way to acquire it). Every one of these is open to any signed
    # agent; none is operator-gated.

    def create_exchange_deposit(self, agent: Agent) -> dict:
        """Reserves a fresh deposit address for this agent's own exchange
        account. Payload-less, like `faucet_claim` -- pay
        `required_amount` (from the returned reservation) to
        `deposit_address`, then `confirm_exchange_deposit`.
        """
        return self._signed_post("/exchange/deposit", agent, None)

    def confirm_exchange_deposit(self, agent: Agent, escrow_id: str) -> dict:
        payload = {"escrow_id": escrow_id}
        return self._signed_post(f"/exchange/deposit/{escrow_id}/confirm", agent, payload)

    def place_order(self, agent: Agent, side: str, price: int, quantity: int) -> dict:
        """``side`` is ``"buy"`` or ``"sell"`` -- the hub's `Side` enum is
        `#[serde(rename_all = "snake_case")]`. Matches immediately against
        the resting book in price-time priority; whichever side crosses
        (the "taker") pays a small fee taken out of what it receives,
        never charged on top of what was already locked. Returns the
        resulting `OrderDto` -- check `status`/`filled` to see whether
        (and how much of) it matched immediately versus resting on the
        book.
        """
        payload = {"side": side, "price": price, "quantity": quantity}
        return self._signed_post("/exchange/orders", agent, payload)

    def cancel_order(self, agent: Agent, order_id: str) -> dict:
        payload = {"order_id": order_id}
        return self._signed_post(f"/exchange/orders/{order_id}/cancel", agent, payload)

    def withdraw(self, agent: Agent, amount: int) -> dict:
        """Pays `amount` of the caller's own spendable base balance back
        to their on-chain wallet. Compute is never withdrawable -- it
        only exists to be traded here.
        """
        payload = {"amount": amount}
        return self._signed_post("/exchange/withdraw", agent, payload)

    def get_order_book(self) -> dict:
        """`{"bids": [...], "asks": [...]}`, each an `OrderDto` list."""
        return self._get("/exchange/orders")

    def get_exchange_account(self, pubkey_hex: str) -> dict:
        """`{"base_balance", "locked_base", "compute_balance",
        "locked_compute"}` -- spendable amount for a new order or a
        withdrawal is always the balance minus its locked counterpart.
        """
        return self._get(f"/exchange/account/{pubkey_hex}")

    def list_trades(self, offset: int = 0, limit: Optional[int] = None) -> list:
        """Executed trades, newest first. Use `list_trades_page` for the
        total-before-pagination count too.
        """
        items, _ = self.list_trades_page(offset, limit)
        return items

    def list_trades_page(
        self, offset: int = 0, limit: Optional[int] = None
    ) -> Tuple[list, Optional[int]]:
        params: Dict[str, Any] = {"offset": offset}
        if limit is not None:
            params["limit"] = limit
        return self._get_with_total("/exchange/trades", params=params)
