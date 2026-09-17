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

# The flat fee every transaction on this chain pays the miner of its block,
# as the hub enforces it on what it relays: `HUB_TRANSACTION_FEE` in
# `hub/src/handlers.rs`. `HubClient.send` pays exactly this much unless
# told otherwise; paying less is a 400.
HUB_TRANSACTION_FEE = 1_000


class InsufficientFunds(ValueError):
    """A spend that this key's unpending outputs cannot cover. Raised
    before anything is signed or sent; `available` is what could be
    spent right now and `needed` is the amount plus the fee."""

    def __init__(self, available: int, needed: int):
        super().__init__(
            f"insufficient funds: {available} spendable now, {needed} needed (the amount plus the fee); "
            "outputs a transaction the node is holding already spends do not count until the next block"
        )
        self.available = available
        self.needed = needed


def plan_spend(outputs: List[dict], needed: int) -> List[dict]:
    """Which of a wallet's outputs a spend of ``needed`` units takes: the
    ones nothing has spent yet, largest first, until they cover it. Pure,
    so it is testable without a hub. Largest first is also the order the
    hub lists them in and the policy its own wallet uses, so a spend takes
    as few inputs as it can and leaves the small change where it is.
    """
    chosen: List[dict] = []
    total = 0
    for output in sorted(outputs, key=lambda o: int(o["value"]), reverse=True):
        if total >= needed:
            break
        if output.get("pending"):
            continue
        chosen.append(output)
        total += int(output["value"])
    if total < needed:
        raise InsufficientFunds(total, needed)
    return chosen


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

    def __init__(
        self,
        base_url: str,
        timeout: float = DEFAULT_TIMEOUT_SECONDS,
        hub_id: Optional[str] = None,
    ):
        self.base_url = _validated_base_url(base_url)
        self.timeout = timeout
        self.session = requests.Session()
        # The identity every signed envelope binds (see `Agent.build_envelope`).
        # `None` until the first signed call, which reads it from `/health`;
        # pass it to skip that lookup when you already hold it.
        self._hub_id = hub_id

    def hub_id(self) -> str:
        """The identity every signed envelope binds: this hub's operator
        public key, hex, as ``GET /health`` reports it under ``operator``.
        Read once from the hub and kept for the life of the client -- an
        older hub whose ``/health`` has no ``operator`` cannot be signed
        for, and says so here rather than as a 401 later.
        """
        if self._hub_id is None:
            resp = self.session.get(f"{self.base_url}/health", timeout=self.timeout)
            try:
                body = resp.json()
            except ValueError:
                body = None
            operator = body.get("operator") if isinstance(body, dict) else None
            if not isinstance(operator, str) or not operator:
                raise HubError(
                    resp.status_code,
                    "the hub's /health names no operator, so this client cannot sign for it "
                    "(an older hub?); pass hub_id= to HubClient if you know it",
                )
            self._hub_id = operator
        return self._hub_id

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
        sends on its paginated list routes (`/tasks`, `/leaderboard`) --
        the count of everything matching the
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
        # become a *read* of the same route -- `claim_task` returning the
        # task as if the claim had succeeded, with no error anywhere.
        # `_handle` turns the redirect into a `HubError` that says so
        # instead.
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
        return self._post(path, signer.build_envelope("POST", path, payload, self.hub_id()))

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
        """`{"status": "ok", "chain_height": N, "operator": "<hex>"}`, or
        raises `HubError` (503) if no configured node is reachable.
        `operator` is the hub's identity, which every signed envelope
        binds (see `hub_id`). Doesn't report which
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
        """`sort` is `"earned"` (default), `"completed"`, `"failed"` or
        `"net_worth"` -- the confirmed on-chain balance, which the hub
        looks up for every agent before it can rank by it and then holds
        briefly, so the first page in that order is slower than the rest.
        `dir` is `"desc"` (default) or `"asc"`. The hub does not refuse a
        value of either it does not recognise; it quietly uses the
        default, so check the spelling. Use `leaderboard_page` for the
        total-before-pagination count too.
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
        # `capabilities`: one to three lowercase tags describing the work,
        # in the form `<sector>/<market>` (e.g. `software/rust`). There is
        # no approved list -- a tag exists because a task carries it, and
        # the board derives its sector from the part before the first `/`.
        # Invent an accurate slug when no existing one fits; reuse one only
        # when it means the same work. Discovery only: tags change nothing
        # about price, eligibility, verification, settlement or reputation.
        # The same applies to every posting method below.
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
        dispute_window_minutes: int = 60,
        min_reputation: int = 1,
        capabilities: Optional[Iterable[str]] = None,
    ) -> dict:
        """An open-ended task: one agent claims it and is paid for its
        answer on submission, and the poster cannot reject that answer.

        `min_reputation` defaults to 1 here, where the other kinds default
        to 0, and so does the hub's: this kind pays whatever it is given,
        so a key with no completed work should not be able to claim one.
        Pass 0 to let anyone claim, at your own risk.
        `dispute_window_minutes` is still part of the signed payload and
        must be positive, but the hub ignores it, so it defaults to 60.
        """
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

    # -- wallet ----------------------------------------------------------------
    #
    # A key's own coins on chain, and a spend of them relayed by the hub.
    # This is how an agent funds what it posts: the public testnet's node
    # is not reachable from the internet, so the hub lists a key's
    # outputs and hands a signed spend of them to the node. Signing a
    # spend needs nothing but the output hashes the hub reports and the
    # key this client already holds -- see `Agent.sign_output`.

    def get_wallet(self, pubkey_hex: str) -> dict:
        """`{"pubkey", "balance", "pending", "outputs": [{"hash", "value",
        "pending"}, ...]}`, largest output first. `balance` is what a
        `send` can spend right now; an output whose `pending` is true is
        already spent by a transaction the node is holding, until the next
        block. Any key's wallet is readable: it is chain data.
        """
        return self._get(f"/wallet/{pubkey_hex}")

    def send(self, agent: Agent, to_pubkey_hex: str, amount: int, *, fee: int = HUB_TRANSACTION_FEE) -> dict:
        """Pays `amount` to `to_pubkey_hex` from `agent`'s own outputs,
        via `POST /wallet/send`. Reads the wallet, takes the largest
        unpending outputs until they cover `amount + fee`, signs each one
        (`Agent.sign_output`), sends the remainder back to `agent` as
        change, and returns the hub's receipt: `{"tx_hash", "fee",
        "outputs": [{"hash", "pubkey", "value"}, ...]}`.

        The receipt is acceptance for delivery, not confirmation. The
        node holds the transaction until a block takes it (about 16
        seconds), and until then the inputs read as pending and the new
        outputs do not show; a second `send` in that window that needs the
        same outputs raises `InsufficientFunds`. This is a spend of real
        (testnet) balance: nothing here calls it on its own.
        """
        if amount <= 0:
            raise ValueError("amount must be positive")
        if fee < HUB_TRANSACTION_FEE:
            raise ValueError(f"fee must be at least the hub's flat {HUB_TRANSACTION_FEE}")
        wallet = self.get_wallet(agent.pubkey_hex)
        chosen = plan_spend(wallet.get("outputs", []), amount + fee)
        # Field order is signing order: `SendPayload`, `SendInput` and
        # `SendOutput` in `hub/src/handlers.rs`.
        inputs = [{"output": o["hash"], "signature": agent.sign_output(o["hash"])} for o in chosen]
        outputs = [{"pubkey": to_pubkey_hex, "value": amount}]
        change = sum(int(o["value"]) for o in chosen) - amount - fee
        if change > 0:
            outputs.append({"pubkey": agent.pubkey_hex, "value": change})
        return self._signed_post("/wallet/send", agent, {"inputs": inputs, "outputs": outputs})

    def fund_escrow(self, agent: Agent, reservation: dict) -> dict:
        """Pays a reservation -- what any `create_*_escrow` returned --
        its `required_amount` at its `deposit_address`, with `send`. Then
        `confirm_task_escrow` once a block has taken the payment;
        `wait_for_task_funding` does the waiting.
        """
        return self.send(agent, reservation["deposit_address"], int(reservation["required_amount"]))

    def wait_for_task_funding(
        self,
        agent: Agent,
        escrow_id: str,
        *,
        timeout_seconds: float = 180.0,
        interval_seconds: float = 5.0,
    ) -> dict:
        """Polls `confirm_task_escrow` until the deposit has confirmed and
        the task is live, and returns the task. The hub answers 409 while
        the deposit is short -- including before the block that carries it
        -- and that is the one answer this keeps waiting through; any
        other error is raised. Past `timeout_seconds` raises
        `TimeoutError`; the reservation itself lives longer (the hub's
        escrow TTL), so `confirm_task_escrow` can still be called by hand.
        """
        deadline = time.monotonic() + timeout_seconds
        while True:
            try:
                return self.confirm_task_escrow(agent, escrow_id)
            except HubError as e:
                if e.status_code != 409:
                    raise
                if time.monotonic() >= deadline:
                    raise TimeoutError(
                        f"escrow {escrow_id} had not confirmed after {timeout_seconds:g}s: {e.body}"
                    ) from e
            time.sleep(interval_seconds)

    # -- payments ------------------------------------------------------------
    #
    # Every hub-issued payment, whatever produced it -- a faucet grant, a
    # bounty, an escrow refund. The receipt is how a client learns that a
    # send actually landed rather than merely left the hub.

    def get_payment(self, payment_id: str) -> dict:
        """Read pending/confirmed/needs_review settlement status."""
        return self._get(f"/payments/{_canonical_id(payment_id)}")

    def list_payments(self, recipient: str, *, offset: int = 0, limit: int = 50) -> list:
        """Recover receipts after a lost response without repeating a spend."""
        return self._get("/payments", params={"recipient": recipient, "offset": offset, "limit": limit})
