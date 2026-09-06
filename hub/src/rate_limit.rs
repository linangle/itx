use axum::body::Body;
use axum::extract::{ConnectInfo, Request, State};
use axum::http::{Method, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use chrono::{DateTime, Duration, Utc};
use dashmap::DashMap;
use std::collections::HashSet;
use std::net::{AddrParseError, IpAddr, SocketAddr};

/// How often a client's request count resets.
const WINDOW_SECONDS: i64 = 60;
/// Requests allowed per client per window on the ordinary read path --
/// generous for a legitimate agent (a market-making bot polling every few
/// seconds, a dashboard refreshing) while still bounding how much load one
/// client can throw at the process. `pub(crate)` so the end-to-end HTTP
/// test in `main.rs` can drive exactly this many requests rather than
/// hardcoding a second copy of the number.
pub(crate) const MAX_REQUESTS_PER_WINDOW: u32 = 120;

/// What a request costs the hub, and therefore which budget it draws
/// from. Reads are generous and writes are scarce (§3.4 of
/// `docs/agent-ecosystem-plan.md`), because the two cost wildly different
/// things: a read walks memory, while a write costs an ECDSA verify and,
/// for the `Chain` tier, a fresh TCP round trip to the node on top.
///
/// Each tier is its own bucket, so exhausting one leaves the others
/// untouched. That is the point of tiering rather than just lowering one
/// global number: a client that floods the read path must not be able to
/// take out anyone's ability to settle a task, and a burst of writes must
/// not blind the operator's monitoring.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Tier {
    /// `GET /health`. Its own bucket so that a read flood cannot make
    /// monitoring lie -- a 429 here reads as "the hub is down" to an
    /// uptime check, which is exactly the wrong thing to say under load.
    /// Not unlimited, though: `health` asks the node for its chain tip,
    /// so every call is a real TCP round trip (see `handlers::health`).
    Health,
    /// Every other read. Served from the in-memory board, and left at
    /// the number that has always applied so no existing client changes
    /// behaviour. Two of these (`/leaderboard`, `/reputation/:pubkey`)
    /// do reach the node, and are amplifiers worth watching -- but the
    /// fix for those is caching and pagination (§6.1/§6.2), not a limit
    /// tight enough to break the dashboard's 5-second poll.
    Read,
    /// Signed writes served from memory and redb: claiming, cancelling,
    /// placing an order, reserving an escrow address. One ECDSA verify
    /// each, no node round trip. One per second, sustained.
    Write,
    /// Signed writes that reach the node or move coins: posting a task,
    /// confirming an escrow, submitting work that may settle, the
    /// faucet, withdrawal. The scarcest budget, and deliberately paced
    /// against the chain rather than against the CPU -- a block is 16
    /// seconds, so an honest agent's on-chain work is already gated by
    /// confirmation, and the operator's own payout throughput is about
    /// one per block (§6.4b). Twenty a minute is several times more than
    /// an honest agent can put to use, and a fraction of what the old
    /// single limit allowed an attacker.
    Chain,
}

impl Tier {
    const fn max_per_window(self) -> u32 {
        match self {
            Tier::Health => 120,
            Tier::Read => MAX_REQUESTS_PER_WINDOW,
            Tier::Write => 60,
            Tier::Chain => 20,
        }
    }
}

/// Classifies a request by what serving it will cost.
///
/// Matched on the path's segments rather than axum's `MatchedPath`,
/// because this layer wraps the whole router and therefore runs before a
/// route has been matched -- and because an unrouted path still needs a
/// budget, or a flood of 404s would be free.
///
/// The `Chain` arms are not a guess: each is a route whose handler
/// reaches `NodeClient` directly or through a settlement path (see
/// `handlers::{create_task, confirm_task_escrow, submit_task,
/// confirm_dispute_escrow, resolve_dispute, faucet_claim,
/// confirm_exchange_deposit, withdraw}`). Anything else that writes falls
/// through to `Write`, which is the safe direction to be wrong in: a new
/// route is scarce until someone classifies it.
fn tier_for(method: &Method, path: &str) -> Tier {
    let segments: Vec<&str> = path.split('/').filter(|segment| !segment.is_empty()).collect();

    if *method == Method::GET || *method == Method::HEAD {
        return if segments.as_slice() == ["health"] { Tier::Health } else { Tier::Read };
    }

    match segments.as_slice() {
        ["tasks"] | ["tasks", "consensus"] => Tier::Chain,
        ["tasks", "escrow", _, "confirm"] => Tier::Chain,
        ["tasks", _, "submit"] => Tier::Chain,
        ["tasks", _, "dispute", "confirm"] | ["tasks", _, "dispute", "resolve"] => Tier::Chain,
        ["faucet"] => Tier::Chain,
        ["exchange", "deposit", _, "confirm"] | ["exchange", "withdraw"] => Tier::Chain,
        _ => Tier::Write,
    }
}

/// One counted budget: a network source at a given tier.
///
/// An enum rather than a bare tuple because a second axis -- the verified
/// pubkey -- joins it, and both belong in one table so one sweep covers
/// them.
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub enum Bucket {
    Ip(IpAddr, Tier),
}

impl Bucket {
    fn max_per_window(&self) -> u32 {
        match self {
            Bucket::Ip(_, tier) => tier.max_per_window(),
        }
    }
}

pub struct Window {
    started_at: DateTime<Utc>,
    count: u32,
}

/// Per-client request tracking. Deliberately a field on `AppState`
/// (constructed fresh per hub instance), *not* a process-wide
/// `#[dynamic] static` like `auth::SEEN_SIGNATURES` -- unlike a
/// signature (unique per request by construction), a client IP is not,
/// and every hub test instance in this workspace's test suite runs on
/// 127.0.0.1. A global static here would mean every test hub sharing
/// one rate-limit bucket and spuriously tripping each other's limits;
/// scoping it to `AppState` gives each hub instance (real or test) its
/// own independent table, the same reasoning `payout_lock` and `board`
/// are already instance-scoped rather than global.
pub type RateLimitTable = DashMap<Bucket, Window>;

pub fn new_table() -> RateLimitTable {
    DashMap::new()
}

/// Records one request against `bucket` and reports whether it's still
/// within that bucket's limit. Fixed-window, not sliding -- simple, and
/// good enough for "stop a naive flood," not meant to be a precise
/// leaky-bucket.
fn check_and_record(table: &RateLimitTable, bucket: Bucket, now: DateTime<Utc>) -> bool {
    let limit = bucket.max_per_window();
    let mut entry = table.entry(bucket).or_insert_with(|| Window { started_at: now, count: 0 });
    if now - entry.started_at > Duration::seconds(WINDOW_SECONDS) {
        entry.started_at = now;
        entry.count = 0;
    }
    entry.count += 1;
    entry.count <= limit
}

/// Evicts entries whose window has already lapsed -- keeps this from
/// growing forever on a long-running process. Call periodically from
/// the sweep loop, mirroring `auth::cleanup_replay_guard`.
pub fn cleanup(table: &RateLimitTable) {
    let cutoff = Utc::now() - Duration::seconds(WINDOW_SECONDS);
    table.retain(|_, w| w.started_at > cutoff);
}

/// The reverse-proxy addresses whose `X-Forwarded-For` header this hub
/// believes. Empty by default, and empty is the only safe default: the
/// header is set by whoever sends the request, so honouring it from an
/// arbitrary peer lets any client name its own rate-limit bucket and
/// step out of the limit entirely by varying one header. Populated from
/// `--trusted-proxies` when the hub actually sits behind a proxy we run
/// (see §3.2 of `docs/agent-ecosystem-plan.md`).
pub type TrustedProxies = HashSet<IpAddr>;

/// Parses the `--trusted-proxies` argument: a comma-separated list of IP
/// addresses, where empty (or all-whitespace) means "trust nothing".
/// Rejects anything unparseable rather than silently dropping it -- a
/// typo in this list is a security control that quietly does not apply,
/// so it should stop the hub at startup, not at the first spoofed header.
pub fn parse_trusted_proxies(spec: &str) -> Result<TrustedProxies, AddrParseError> {
    spec.split(',').map(str::trim).filter(|s| !s.is_empty()).map(str::parse).collect()
}

/// The address to charge for this request.
///
/// `X-Forwarded-For` is honoured only when the direct TCP peer is one of
/// `trusted` -- otherwise the peer address is used and the header is
/// ignored outright. For an un-proxied hub (the default, `trusted`
/// empty) that means the header can never move a client out of its own
/// bucket.
///
/// When the peer *is* a trusted proxy, the list is read right to left.
/// The rightmost entry is the one our own proxy appended, and is the
/// address it actually saw; everything to its left is whatever the
/// client sent, which it is free to invent. So we take the rightmost
/// entry that is not itself one of our proxies -- with a single hop that
/// is the real client, and with a chain of our own proxies it walks back
/// past them. Reading left to right instead would be spoofable even
/// behind a correctly configured proxy, since a client can simply
/// pre-seed the header with an address of its choosing.
///
/// Falls back to the peer address whenever the header is absent,
/// unparseable, or made up entirely of our own proxies.
fn client_ip(req: &Request<Body>, connect_addr: SocketAddr, trusted: &TrustedProxies) -> IpAddr {
    let peer = connect_addr.ip();
    if !trusted.contains(&peer) {
        return peer;
    }
    req.headers()
        .get_all("x-forwarded-for")
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(','))
        .filter_map(|v| v.trim().parse::<IpAddr>().ok())
        .filter(|forwarded| !trusted.contains(forwarded))
        .next_back()
        .unwrap_or(peer)
}

/// Registered via `middleware::from_fn_with_state` with the *same*
/// `Arc<AppState>` the router itself uses -- no separate state to keep
/// in sync, `state.rate_limits` is the single source of truth either
/// way a handler or this middleware reaches it.
pub async fn middleware(
    State(state): State<std::sync::Arc<crate::AppState>>,
    ConnectInfo(connect_addr): ConnectInfo<SocketAddr>,
    req: Request<Body>,
    next: Next,
) -> Response {
    let ip = client_ip(&req, connect_addr, &state.trusted_proxies);
    let tier = tier_for(req.method(), req.uri().path());
    if !check_and_record(&state.rate_limits, Bucket::Ip(ip, tier), Utc::now()) {
        return (StatusCode::TOO_MANY_REQUESTS, "rate limit exceeded, slow down").into_response();
    }
    next.run(req).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn allows_requests_within_the_limit_and_rejects_beyond_it() {
        let table = new_table();
        let ip: IpAddr = "127.0.0.1".parse().unwrap();
        let now = Utc::now();
        for _ in 0..MAX_REQUESTS_PER_WINDOW {
            assert!(check_and_record(&table, Bucket::Ip(ip, Tier::Read), now));
        }
        assert!(!check_and_record(&table, Bucket::Ip(ip, Tier::Read), now), "one past the limit must be rejected");
    }

    #[test]
    fn resets_after_the_window_elapses() {
        let table = new_table();
        let ip: IpAddr = "127.0.0.1".parse().unwrap();
        let now = Utc::now();
        for _ in 0..MAX_REQUESTS_PER_WINDOW {
            assert!(check_and_record(&table, Bucket::Ip(ip, Tier::Read), now));
        }
        assert!(!check_and_record(&table, Bucket::Ip(ip, Tier::Read), now));

        let later = now + Duration::seconds(WINDOW_SECONDS + 1);
        assert!(check_and_record(&table, Bucket::Ip(ip, Tier::Read), later), "a new window must start fresh");
    }

    #[test]
    fn tracks_different_ips_independently() {
        let table = new_table();
        let a: IpAddr = "127.0.0.1".parse().unwrap();
        let b: IpAddr = "10.0.0.1".parse().unwrap();
        let now = Utc::now();
        for _ in 0..MAX_REQUESTS_PER_WINDOW {
            assert!(check_and_record(&table, Bucket::Ip(a, Tier::Read), now));
        }
        assert!(!check_and_record(&table, Bucket::Ip(a, Tier::Read), now));
        assert!(check_and_record(&table, Bucket::Ip(b, Tier::Read), now), "a different IP must not be affected by another's limit");
    }

    #[test]
    fn each_route_lands_in_the_tier_its_handler_actually_costs() {
        let get = Method::GET;
        let post = Method::POST;

        assert_eq!(tier_for(&get, "/health"), Tier::Health);
        for read in ["/tasks", "/tasks/some-id", "/leaderboard", "/llms.txt", "/exchange/orders", "/board/summary"] {
            assert_eq!(tier_for(&get, read), Tier::Read, "{read} is a read");
        }

        // Reaches the node or moves coins.
        for chain in [
            "/tasks",
            "/tasks/consensus",
            "/tasks/escrow/some-id/confirm",
            "/tasks/some-id/submit",
            "/tasks/some-id/dispute/confirm",
            "/tasks/some-id/dispute/resolve",
            "/faucet",
            "/exchange/deposit/some-id/confirm",
            "/exchange/withdraw",
        ] {
            assert_eq!(tier_for(&post, chain), Tier::Chain, "{chain} reaches the node");
        }

        // Signed, but served from memory and redb.
        for write in [
            "/tasks/escrow",
            "/tasks/consensus/escrow",
            "/tasks/disputable/escrow",
            "/tasks/some-id/claim",
            "/tasks/some-id/cancel",
            "/tasks/some-id/dispute/escrow",
            "/exchange/deposit",
            "/exchange/orders",
            "/exchange/orders/some-id/cancel",
        ] {
            assert_eq!(tier_for(&post, write), Tier::Write, "{write} stays local");
        }
    }

    #[test]
    fn an_unrouted_path_is_still_budgeted_and_a_new_write_defaults_to_scarce() {
        assert_eq!(tier_for(&Method::GET, "/no/such/route"), Tier::Read, "404s must not be free");
        assert_eq!(
            tier_for(&Method::POST, "/some/route/added/later"), Tier::Write,
            "an unclassified write should be scarce until someone classifies it"
        );
        assert_eq!(tier_for(&Method::DELETE, "/tasks/some-id"), Tier::Write);
        assert_eq!(tier_for(&Method::GET, "/health/"), Tier::Health, "a trailing slash is the same route");
    }

    #[test]
    fn exhausting_one_tier_leaves_the_others_alone() {
        let table = new_table();
        let ip: IpAddr = "127.0.0.1".parse().unwrap();
        let now = Utc::now();

        for _ in 0..Tier::Chain.max_per_window() {
            assert!(check_and_record(&table, Bucket::Ip(ip, Tier::Chain), now));
        }
        assert!(!check_and_record(&table, Bucket::Ip(ip, Tier::Chain), now));

        // The same client, same window, having burned its whole chain
        // budget -- reads and health checks must still go through.
        assert!(check_and_record(&table, Bucket::Ip(ip, Tier::Read), now), "reads are a separate budget");
        assert!(check_and_record(&table, Bucket::Ip(ip, Tier::Health), now), "monitoring is a separate budget");
        assert!(check_and_record(&table, Bucket::Ip(ip, Tier::Write), now), "local writes are a separate budget");
    }

    #[test]
    fn writes_are_scarcer_than_reads_and_chain_writes_scarcest() {
        assert!(Tier::Chain.max_per_window() < Tier::Write.max_per_window());
        assert!(Tier::Write.max_per_window() < Tier::Read.max_per_window());
    }

    /// Builds a request carrying `forwarded` as its `X-Forwarded-For`,
    /// or no such header at all when `forwarded` is `None`.
    fn request_with_forwarded_for(forwarded: Option<&str>) -> Request<Body> {
        let builder = Request::builder();
        let builder = match forwarded {
            Some(value) => builder.header("x-forwarded-for", value),
            None => builder,
        };
        builder.body(Body::empty()).unwrap()
    }

    fn peer(addr: &str) -> SocketAddr {
        format!("{addr}:54321").parse().unwrap()
    }

    fn ip(addr: &str) -> IpAddr {
        addr.parse().unwrap()
    }

    #[test]
    fn forwarded_for_is_ignored_when_the_peer_is_not_a_trusted_proxy() {
        // The whole point: with nothing trusted (the default), a client
        // cannot name its own bucket. Before this, the header alone was
        // enough to walk out of the limit.
        let trusted = parse_trusted_proxies("").unwrap();
        let req = request_with_forwarded_for(Some("203.0.113.9"));
        assert_eq!(client_ip(&req, peer("198.51.100.4"), &trusted), ip("198.51.100.4"));
    }

    #[test]
    fn forwarded_for_is_ignored_from_an_untrusted_peer_even_when_some_proxy_is_trusted() {
        // A configured proxy list must not make the header believable
        // from *anyone* -- only from the proxies on it.
        let trusted = parse_trusted_proxies("10.0.0.1").unwrap();
        let req = request_with_forwarded_for(Some("203.0.113.9"));
        assert_eq!(client_ip(&req, peer("198.51.100.4"), &trusted), ip("198.51.100.4"));
    }

    #[test]
    fn forwarded_for_is_honoured_from_a_trusted_proxy() {
        let trusted = parse_trusted_proxies("10.0.0.1").unwrap();
        let req = request_with_forwarded_for(Some("203.0.113.9"));
        assert_eq!(client_ip(&req, peer("10.0.0.1"), &trusted), ip("203.0.113.9"));
    }

    #[test]
    fn a_client_cannot_prepend_a_fake_entry_to_escape_its_own_bucket() {
        // What the proxy appends is the address it saw; anything to the
        // left of that is the client's own invention. Reading left to
        // right would hand the attacker exactly what it asked for.
        let trusted = parse_trusted_proxies("10.0.0.1").unwrap();
        let req = request_with_forwarded_for(Some("203.0.113.9, 198.51.100.4"));
        assert_eq!(client_ip(&req, peer("10.0.0.1"), &trusted), ip("198.51.100.4"));
    }

    #[test]
    fn a_chain_of_our_own_proxies_is_walked_back_past() {
        let trusted = parse_trusted_proxies("10.0.0.1, 10.0.0.2").unwrap();
        let req = request_with_forwarded_for(Some("198.51.100.4, 10.0.0.2"));
        assert_eq!(client_ip(&req, peer("10.0.0.1"), &trusted), ip("198.51.100.4"));
    }

    #[test]
    fn repeated_forwarded_for_headers_are_read_as_one_list() {
        // A proxy chain may append a second header rather than extending
        // the first; both spellings mean the same thing on the wire.
        let trusted = parse_trusted_proxies("10.0.0.1, 10.0.0.2").unwrap();
        let req = Request::builder()
            .header("x-forwarded-for", "198.51.100.4")
            .header("x-forwarded-for", "10.0.0.2")
            .body(Body::empty())
            .unwrap();
        assert_eq!(client_ip(&req, peer("10.0.0.1"), &trusted), ip("198.51.100.4"));
    }

    #[test]
    fn a_trusted_peer_with_no_usable_forwarded_for_falls_back_to_itself() {
        let trusted = parse_trusted_proxies("10.0.0.1").unwrap();
        assert_eq!(
            client_ip(&request_with_forwarded_for(None), peer("10.0.0.1"), &trusted),
            ip("10.0.0.1"),
            "no header at all"
        );
        assert_eq!(
            client_ip(&request_with_forwarded_for(Some("not-an-address")), peer("10.0.0.1"), &trusted),
            ip("10.0.0.1"),
            "an unparseable header"
        );
        assert_eq!(
            client_ip(&request_with_forwarded_for(Some("10.0.0.1")), peer("10.0.0.1"), &trusted),
            ip("10.0.0.1"),
            "a header naming only our own proxies"
        );
    }

    #[test]
    fn parse_trusted_proxies_reads_a_list_and_defaults_to_trusting_nothing() {
        assert!(parse_trusted_proxies("").unwrap().is_empty(), "the default must trust nothing");
        assert!(parse_trusted_proxies("  ").unwrap().is_empty());
        assert_eq!(
            parse_trusted_proxies("10.0.0.1, 127.0.0.1 ,::1").unwrap(),
            [ip("10.0.0.1"), ip("127.0.0.1"), ip("::1")].into_iter().collect::<TrustedProxies>()
        );
        assert!(
            parse_trusted_proxies("10.0.0.1, nonsense").is_err(),
            "a typo must stop the hub at startup, not silently disable the control"
        );
    }

    #[test]
    fn cleanup_evicts_only_lapsed_windows() {
        let table = new_table();
        let stale: IpAddr = "127.0.0.1".parse().unwrap();
        let fresh: IpAddr = "10.0.0.1".parse().unwrap();
        let now = Utc::now();
        check_and_record(&table, Bucket::Ip(stale, Tier::Read), now - Duration::seconds(WINDOW_SECONDS + 5));
        check_and_record(&table, Bucket::Ip(fresh, Tier::Read), now);

        cleanup(&table);

        assert!(table.get(&Bucket::Ip(stale, Tier::Read)).is_none());
        assert!(table.get(&Bucket::Ip(fresh, Tier::Read)).is_some());
    }
}
