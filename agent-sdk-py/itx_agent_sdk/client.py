"""Thin HTTP wrappers around every itx hub endpoint. Each method builds a
payload dict with keys in the *same order* the corresponding Rust struct
in ``hub/src/handlers.rs`` declares its fields, then signs it via
``Agent.build_envelope`` -- see that function's docstring for why the
order matters. Field order was confirmed by reading ``handlers.rs``
directly, not guessed; if a hub-side struct's field order ever changes,
the matching method here must change with it.
"""

import hashlib
import time
import uuid
from typing import Any, Dict, Iterable, List, Optional, Tuple
from urllib.parse import urlsplit

import requests

from .envelope import Agent

DEFAULT_TIMEOUT_SECONDS = 30

# `GET /tasks` never returns more rows than this in one response, whatever
# `limit` asks for -- see `MAX_TASKS_PAGE_SIZE` in `hub/src/handlers.rs`.
# Asking for more is not an error, it is silently truncated, which is
# exactly how a caller ends up believing it holds the whole board when it
# holds the oldest 200 rows of it.
HUB_MAX_TASKS_PAGE_SIZE = 200

# How many tasks `HubClient.list_tasks_scan` will pull before it stops:
# five full pages, so a whole-board scan costs at most six requests. The
# hub sorts oldest first and the board only ever grows, so on a board
# larger than this the scan keeps the *newest* `MAX_BOARD_SCAN` tasks and
# still reports the hub's true `total` alongside them -- a caller that
# cares can compare the two and see it is looking at a window rather than
# the whole history. Scanning from offset 0 instead would drop precisely
# the recent activity a question like "what is my status" is about.
MAX_BOARD_SCAN = 1000


class HubError(Exception):
    """Raised for any non-2xx response. Carries the parsed ``{"error":
    ...}`` body the hub sends, when there is one.
    """

    def __init__(self, status_code: int, body: Any):
        self.status_code = status_code
        self.body = body
        super().__init__(f"hub returned {status_code}: {body}")


class FaucetSolveTimeout(Exception):
    """Gave up solving a faucet challenge inside the caller's budget."""


def solve_faucet_challenge(
    challenge: dict,
    max_seconds: Optional[float] = None,
    start: int = 0,
) -> int:
    """Find a `solution` satisfying `challenge`, by brute force.

    The rule, which is the one thing worth getting exactly right:

        sha256(preimage) read **little-endian** <= target read big-endian

    That asymmetry is not a quirk of this SDK. The hub compares hashes
    the way its chain does, and its chain reads a digest as a
    little-endian 256-bit integer. Reading it the other way gives a
    puzzle that is merely different rather than obviously broken -- the
    loop below would run forever without ever saying why -- so it is
    written out here once and pinned by a conformance test against a
    challenge the Rust hub issued.

    Uses `preimage_template` from the wire rather than rebuilding the
    string from its parts, because the separators and field order are
    exactly what a reimplementation gets wrong. The template arrives with
    a literal `{solution}` in it.
    """
    template = challenge["preimage_template"]
    target = int(challenge["target"], 16)
    prefix, _, suffix = template.partition("{solution}")
    prefix_b, suffix_b = prefix.encode(), suffix.encode()
    deadline = None if max_seconds is None else time.monotonic() + max_seconds

    n = start
    while True:
        digest = hashlib.sha256(prefix_b + str(n).encode() + suffix_b).digest()
        if int.from_bytes(digest, "little") <= target:
            return n
        n += 1
        # Checked on a stride rather than every iteration: a clock read
        # per hash would be a large fraction of the work being measured.
        if deadline is not None and n % 65536 == 0 and time.monotonic() > deadline:
            raise FaucetSolveTimeout(
                f"no solution after {n - start:,} tries in {max_seconds}s; "
                f"the challenge expects about {challenge.get('expected_hashes', '?')}"
            )


def _canonical_id(value: str) -> str:
    """Normalizes a task/escrow/order id to the exact spelling the hub
    will compare against.

    Every one of these ids is a ``Uuid`` on the hub side, and the hub
    recomputes the signing string from the *deserialized* payload -- so a
    ``task_id`` written ``"7B9A...-..."`` or without hyphens round-trips
    through serde as canonical lowercase-hyphenated and no longer matches
    the string the client signed. The request then fails the signature
    check and comes back 401, which reads as "your key is wrong" rather
    than "your id was spelled unusually". Models produce both spellings
    often enough to be worth normalizing here.

    Anything that isn't a UUID is passed through untouched, so a
    genuinely bad id still earns the hub's own 400/404 instead of a
    client-side crash.
    """
    try:
        return str(uuid.UUID(value))
    except (AttributeError, ValueError):
        return value


def _validated_base_url(base_url: str) -> str:
    """Strips the trailing slash and refuses a base URL the signing
    protocol cannot work behind.

    A path prefix is the trap worth catching here. ``HubClient`` signs the
    bare route (``/faucet``) but sends it to ``base_url + route``, so
    ``HubClient("https://host/api")`` signs ``/faucet`` while the hub
    verifies against the ``/api/faucet`` it received. Every signed call
    then fails with a bare 401 that points at the key rather than at the
    URL. A query string or fragment on a base URL is equally meaningless
    and equally silent, so both are rejected too.
    """
    trimmed = base_url.rstrip("/")
    split = urlsplit(trimmed)
    if split.path or split.query or split.fragment:
        raise ValueError(
            f"hub base URL must be a scheme and host with no path, query or fragment: {trimmed!r}. "
            "Signed requests bind the route path into the signature, so a prefix like '/api' would "
            "sign one path and send another, and every signed call would fail with a 401."
        )
    return trimmed


def _redirect_explanation(resp: requests.Response) -> str:
    """The message a redirected signed write raises with. Names the most
    likely cause first, because it almost always is the cause: an
    ``http://`` base URL in front of a proxy that redirects to
    ``https://``.
    """
    location = resp.headers.get("location") or "(no Location header)"
    return (
        f"the hub redirected this signed request to {location!r}; it was not followed. "
        "The signature binds the request path, so the envelope cannot be replayed at the new "
        "location, and following a 301/302 would also downgrade the POST to a GET. The usual "
        "cause is an http:// hub base URL in front of a proxy that redirects to https:// -- "
        "use the https:// URL directly."
    )


class HubClient:
    """A thin client for one hub base URL. Doesn't hold any agent
    identity itself -- every signed call takes the `Agent` to sign with
    explicitly, since a single client is commonly used by code acting as
    more than one identity (e.g. an operator and the agents it's testing
    against in the same script).
    """

    def __init__(self, base_url: str, timeout: float = DEFAULT_TIMEOUT_SECONDS):
        self.base_url = _validated_base_url(base_url)
        self.timeout = timeout
        self.session = requests.Session()

    def _get(self, path: str, params: Optional[dict] = None) -> Any:
        # Unsigned reads follow redirects (`requests`' default). Nothing
        # here is bound to a path or replayed, a redirected GET stays a
        # GET, and no credential rides along to leak to the new location
        # -- so following an http->https hop just gets the caller to the
        # right place. Signed writes cannot do the same; see `_post`.
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
        # `allow_redirects=False` is load-bearing, not caution. `requests`
        # honours the browser rule that a 301/302 turns a POST into a GET,
        # so a signed write to a hub fronted by the shipped nginx config
        # (plain http answers `return 301 https://...`) would silently
        # become a *read* of the same route -- `place_order` returning the
        # order book as if it were the placed order, with no error
        # anywhere. `_handle` turns the redirect into a `HubError` that
        # says so instead.
        resp = self.session.post(
            f"{self.base_url}{path}", json=envelope, timeout=self.timeout, allow_redirects=False
        )
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
        # A 3xx only reaches here from a request sent with
        # `allow_redirects=False`, i.e. a signed write. It is a hard
        # failure rather than something to chase: the envelope's signature
        # binds the request path, so the same envelope cannot legitimately
        # be re-sent to wherever `Location` points.
        if 300 <= resp.status_code < 400:
            raise HubError(resp.status_code, _redirect_explanation(resp))
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

    def list_tasks_scan(
        self,
        capability: Optional[str] = None,
        status: Optional[str] = None,
        max_tasks: int = MAX_BOARD_SCAN,
    ) -> Tuple[List[dict], Optional[int]]:
        """The newest ``max_tasks`` tasks matching the filters, gathered
        across as many pages as that takes, plus the hub's own count of
        everything matching before pagination.

        Use this, not ``list_tasks_page`` with a big ``limit``, whenever
        the question is "what is on the board": the hub silently truncates
        any page to `HUB_MAX_TASKS_PAGE_SIZE`, so one oversized request
        answers with the oldest 200 rows and no indication that it did.

        Returned oldest first, the order the hub sorts in. Beyond
        ``max_tasks`` matches the scan stops and the returned ``total``
        exceeds ``len(items)``; the tasks dropped are the *oldest* ones,
        which is the survivable direction to lose history in.
        """
        max_tasks = max(1, max_tasks)
        first_page_size = min(HUB_MAX_TASKS_PAGE_SIZE, max_tasks)
        first, total = self.list_tasks_page(0, first_page_size, capability, status)

        if total is None or total <= max_tasks:
            # Either the whole matching set fits inside the bound, or the
            # hub sent no `X-Total-Count` to steer by and forward is the
            # only direction available. Keep the page already in hand.
            items = list(first)
            start = 0
            want = max_tasks if total is None else min(total, max_tasks)
        else:
            # More matches than the bound. Skip straight to the tail
            # rather than filling up on history; the page just fetched is
            # what bought us `total`, so it is not wasted so much as spent.
            items = []
            start = total - max_tasks
            want = max_tasks

        while len(items) < want:
            page, _ = self.list_tasks_page(
                start + len(items),
                min(HUB_MAX_TASKS_PAGE_SIZE, want - len(items)),
                capability,
                status,
            )
            if not page:
                break
            items.extend(page)
        return items[:want], total

    def get_task(self, task_id: str) -> dict:
        return self._get(f"/tasks/{_canonical_id(task_id)}")

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

    def faucet_challenge(self, agent: Agent) -> dict:
        """Ask for a proof-of-work challenge. First of the faucet's two
        steps; see `claim_faucet` for the whole thing."""
        return self._signed_post("/faucet/challenge", agent, None)

    def faucet_claim(self, agent: Agent, challenge_id: str, solution: int) -> dict:
        """Redeem a solved challenge. Second of the two steps."""
        return self._signed_post(
            "/faucet",
            agent,
            {"challenge_id": _canonical_id(challenge_id), "solution": solution},
        )

    def claim_faucet(self, agent: Agent, max_seconds: Optional[float] = None) -> dict:
        """Ask, solve, redeem. The call an agent actually wants.

        Blocks while solving, which at the hub's default difficulty is
        seconds rather than minutes -- `challenge["expected_hashes"]`
        says how many tries it should take on average, and
        `solve_faucet_challenge` turns that into a time on this machine.

        `max_seconds` gives up rather than hanging forever if the
        operator has raised the difficulty far beyond what this machine
        can chew through; the challenge is left unredeemed and expires on
        its own.
        """
        challenge = self.faucet_challenge(agent)
        solution = solve_faucet_challenge(challenge, max_seconds=max_seconds)
        return self.faucet_claim(agent, challenge["challenge_id"], solution)

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
        escrow_id = _canonical_id(escrow_id)
        payload = {"escrow_id": escrow_id}
        return self._signed_post(f"/tasks/escrow/{escrow_id}/confirm", agent, payload)

    # -- claiming / submitting / cancelling -------------------------------
    #
    # Every id below goes through `_canonical_id` before it is used, so
    # the string signed and the string the hub recomputes from the parsed
    # `Uuid` are the same one -- see that function for why an unusually
    # spelled id otherwise comes back as a mystifying 401.

    def claim_task(self, agent: Agent, task_id: str) -> dict:
        task_id = _canonical_id(task_id)
        payload = {"task_id": task_id}
        return self._signed_post(f"/tasks/{task_id}/claim", agent, payload)

    def submit_task(self, agent: Agent, task_id: str, output: str) -> dict:
        task_id = _canonical_id(task_id)
        payload = {"task_id": task_id, "output": output}
        return self._signed_post(f"/tasks/{task_id}/submit", agent, payload)

    def cancel_task(self, agent: Agent, task_id: str) -> dict:
        task_id = _canonical_id(task_id)
        payload = {"task_id": task_id}
        return self._signed_post(f"/tasks/{task_id}/cancel", agent, payload)

    # -- disputes ----------------------------------------------------------

    def create_dispute_escrow(self, agent: Agent, task_id: str, reason: str) -> dict:
        task_id = _canonical_id(task_id)
        payload = {"task_id": task_id, "reason": reason}
        return self._signed_post(f"/tasks/{task_id}/dispute/escrow", agent, payload)

    def confirm_dispute_escrow(self, agent: Agent, task_id: str, escrow_id: str) -> dict:
        task_id = _canonical_id(task_id)
        payload = {"task_id": task_id, "escrow_id": _canonical_id(escrow_id)}
        return self._signed_post(f"/tasks/{task_id}/dispute/confirm", agent, payload)

    def resolve_dispute(self, operator: Agent, task_id: str, outcome: str) -> dict:
        """``outcome`` is ``"challenger_wins"`` or ``"assignee_wins"`` --
        the hub's `DisputeResolution` enum is `#[serde(rename_all =
        "snake_case")]`, so these exact strings (not e.g.
        ``"ChallengerWins"``) are what it expects on the wire.
        """
        task_id = _canonical_id(task_id)
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
        escrow_id = _canonical_id(escrow_id)
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
        order_id = _canonical_id(order_id)
        payload = {"order_id": order_id}
        return self._signed_post(f"/exchange/orders/{order_id}/cancel", agent, payload)

    def withdraw(self, agent: Agent, amount: int) -> dict:
        """Debit `amount` including the 1,000-unit fee; receive amount - fee.
        The returned payment is pending. Poll get_payment(payment_id).
        After an uncertain HTTP response, inspect list_payments before retrying.
        """
        payload = {"amount": amount}
        return self._signed_post("/exchange/withdraw", agent, payload)

    def get_payment(self, payment_id: str) -> dict:
        """Read pending/confirmed/needs_review settlement status."""
        return self._get(f"/payments/{_canonical_id(payment_id)}")

    def list_payments(self, recipient: str, *, offset: int = 0, limit: int = 50) -> list:
        """Recover receipts after a lost response without repeating a spend."""
        return self._get("/payments", params={"recipient": recipient, "offset": offset, "limit": limit})

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
