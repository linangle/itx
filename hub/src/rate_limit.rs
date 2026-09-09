use axum::body::Body;
use axum::extract::{ConnectInfo, Request, State};
use axum::http::{Method, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use btclib::crypto::PublicKey;
use chrono::{DateTime, Duration, Utc};
use dashmap::DashMap;
use std::collections::HashSet;
use std::net::{AddrParseError, IpAddr, SocketAddr};
use std::time::Instant;

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
    /// `GET /metrics`. Its own bucket for the same reason `Health` has
    /// one, and it is the stronger case of the two: a 429 on a scrape is
    /// not a slow graph, it is a gap in the series, and a monitor that
    /// cannot read the hub reports the hub as down. Sharing the `Read`
    /// bucket would mean the read flood you are trying to diagnose is
    /// also what blinds you to it. Rendering a scrape touches only
    /// atomics and never the node or the board (see `metrics`), so this
    /// budget costs the hub nothing it needs to ration.
    Metrics,
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

/// Signed requests one verified key may make per window, across every
/// route and from wherever it connects.
///
/// This is the axis an IP limit cannot cover: keygen is free and a
/// determined client can spread itself over many addresses (§4), at which
/// point every per-IP bucket it touches looks idle. Charging the key as
/// well means the identity carries a budget with it.
///
/// Deliberately *not* tiered by endpoint, unlike the per-IP buckets. The
/// tiers exist because the middleware knows what a route costs before
/// running it; this one exists to bound an identity's total write rate,
/// and splitting it per tier would only hand an attacker the sum of the
/// parts. Sixty a minute is one per second, far above what an honest
/// agent does and far below what one needs to be a nuisance.
pub const MAX_SIGNED_REQUESTS_PER_PUBKEY_PER_WINDOW: u32 = 60;

impl Tier {
    const fn max_per_window(self) -> u32 {
        match self {
            Tier::Health => 120,
            // Two scrapers on a 5-second interval, with room to spare for
            // an operator running `curl` while diagnosing something.
            Tier::Metrics => 120,
            Tier::Read => MAX_REQUESTS_PER_WINDOW,
            Tier::Write => 60,
            Tier::Chain => 20,
        }
    }
}

impl Tier {
    /// This tier's slot in `metrics::Metrics::rate_limited_by_tier`.
    /// Written as a match rather than a `#[derive]`d discriminant so that
    /// reordering the enum cannot silently start charging rejections to
    /// the wrong label; `metrics::TIER_NAMES` is the other half of this
    /// pairing and `tier_indices_match_their_metric_names` holds the two
    /// together.
    pub(crate) fn metric_index(self) -> usize {
        match self {
            Tier::Health => 0,
            Tier::Metrics => 1,
            Tier::Read => 2,
            Tier::Write => 3,
            Tier::Chain => 4,
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
        return match segments.as_slice() {
            ["health"] => Tier::Health,
            ["metrics"] => Tier::Metrics,
            _ => Tier::Read,
        };
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
    /// A network source, bucketed per endpoint tier -- charged by the
    /// middleware before a handler runs.
    ///
    /// The address is `limit_key`'s, not the peer's: an IPv6 client is
    /// bucketed by its /64. A residential or datacentre customer is
    /// handed a whole /64 as a matter of course, so keying the full
    /// address let one customer take a fresh bucket for every request
    /// simply by using a different one of their own addresses -- every
    /// tier, unbounded, at no cost. There is no equivalent on the IPv4
    /// side, where an address is scarce and usually one client, so that
    /// one is bucketed whole.
    Ip(IpAddr, Tier),
    /// A verified identity, one bucket for every signed request it makes
    /// -- charged by a handler once the envelope's signature checks out.
    /// Keyed by the pubkey's string form, the same shape the board and
    /// the net-worth cache already key agents by.
    Pubkey(String),
}

impl Bucket {
    fn max_per_window(&self) -> u32 {
        match self {
            Bucket::Ip(_, tier) => tier.max_per_window(),
            Bucket::Pubkey(_) => MAX_SIGNED_REQUESTS_PER_PUBKEY_PER_WINDOW,
        }
    }
}

/// Returned when a verified key has spent its window's budget. Its own
/// type rather than a bare `bool` so a handler can pass it straight out
/// with `?` and get a 429 (see `ApiError`), the same status the per-IP
/// middleware returns.
#[derive(Debug, thiserror::Error)]
#[error("per-key request quota exceeded, slow down")]
pub struct QuotaExceeded;

/// Charges `pubkey` for one signed request, after its signature has been
/// verified.
///
/// After, not before, and that ordering is the whole subtlety. The
/// pubkey on an unverified envelope is just a string the sender chose,
/// so charging it early would let anyone burn a victim's quota by
/// putting the victim's key on junk requests. Verification is what makes
/// the claim real -- and it is also the expensive step this quota exists
/// to bound, which is why the cheap checks come first inside
/// `SignedEnvelope::verify_signature`: clock drift is rejected before
/// the payload is ever hashed or a signature ever checked, and the body
/// is size-capped by axum's extractor before that. What remains -- one
/// ECDSA verify per request from a genuine key -- is bounded by the
/// per-IP tier the middleware already charged.
pub fn charge_pubkey(state: &crate::AppState, pubkey: &PublicKey) -> Result<(), QuotaExceeded> {
    let bucket = Bucket::Pubkey(pubkey.to_string());
    if check_and_record(&state.rate_limits, bucket, Utc::now()) {
        Ok(())
    } else {
        // Counted here rather than folded into the per-IP tier totals:
        // the two controls answer different questions, and an operator
        // seeing per-key rejections with flat per-IP ones is looking at
        // one busy identity, not at a flood.
        state.metrics.rate_limited_per_key_quota.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        Err(QuotaExceeded)
    }
}

pub struct Window {
    started_at: DateTime<Utc>,
    count: u32,
}

/// Per-client request tracking. Deliberately a field on `AppState`
/// (constructed fresh per hub instance), *not* a process-wide
/// `#[dynamic] static`, which the replay guard's seen-signature set used
/// to be -- unlike a
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
/// the sweep loop, mirroring `auth::ReplayGuard::cleanup`.
pub fn cleanup(table: &RateLimitTable) {
    let cutoff = Utc::now() - Duration::seconds(WINDOW_SECONDS);
    table.retain(|_, w| w.started_at > cutoff);
}

/// The resolved client address, put into request extensions by
/// `middleware` so a handler can reach it without re-deriving it.
///
/// A handler must never work this out for itself. `client_ip` honours
/// `X-Forwarded-For` only from a trusted peer, and that rule is the
/// difference between a limit and a decoration -- 130 requests with 130
/// spoofed addresses shared one bucket with it and defeated the limit
/// entirely without it (`docs/deployment.md` §11). One implementation,
/// reached one way.
#[derive(Clone, Copy, Debug)]
pub struct ClientIp(pub IpAddr);

/// The network this address is treated as belonging to, for abuse
/// controls that want a unit bigger than one address.
///
/// **IPv4 to /24, IPv6 to /64**, and the asymmetry is the point. A single
/// v4 address is a meaningful unit of cost. A v6 address is not: a
/// residential customer is typically handed a whole /64, so limiting per
/// v6 address limits nothing at all -- an attacker walks 18 quintillion
/// addresses inside one allocation without exhausting a single bucket.
///
/// /64 rather than /48 is a deliberate accepted weakness. /64 matches the
/// usual residential allocation, so it groups one household as one
/// client, which is what we want. A hosting provider hands out a /64 per
/// VM, so a datacentre attacker with a /48 still gets 65,536 buckets --
/// and the answer to that is not a coarser prefix, which would start
/// grouping unrelated households behind one ISP segment. It is the
/// global budget (`--faucet-daily-grants`), which bounds the total
/// however many prefixes anyone assembles. This limit only has to make
/// casual abuse tedious; the budget is what bounds the loss.
pub fn prefix_of(ip: IpAddr) -> String {
    // Canonical first. An IPv4 client reaching a dual-stack listener
    // arrives as `::ffff:a.b.c.d`, and truncating *that* to four
    // segments yields `0:0:0:0::/64` -- the same string for every IPv4
    // client on earth, which would put them all in one bucket and make
    // the per-network price a global one.
    match ip.to_canonical() {
        IpAddr::V4(v4) => {
            let [a, b, c, _] = v4.octets();
            format!("{a}.{b}.{c}.0/24")
        }
        IpAddr::V6(v6) => {
            let s = v6.segments();
            format!("{:x}:{:x}:{:x}:{:x}::/64", s[0], s[1], s[2], s[3])
        }
    }
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
/// **An entry it cannot parse ends the walk rather than being skipped
/// over**, and that direction is the whole of the 2026-09-07 fix -- see
/// `parse_forwarded_entry`.
/// What a per-IP bucket is keyed by: the client, canonicalised, and for
/// IPv6 truncated to the /64 that is handed out as one allocation.
///
/// Keying the full IPv6 address meant a client holding a /64 -- which is
/// the ordinary residential and datacentre allocation -- could source
/// every request from a different address of its own and get a fresh
/// bucket each time. Every tier became unbounded for the cost of using
/// addresses it already had: node round trips through `/health`, an
/// fsync per faucet challenge, an ECDSA verify per signed write. The
/// per-pubkey quota is no help, because keygen is free.
///
/// `to_canonical` first for the reason `prefix_of` gives: an IPv4 client
/// on a dual-stack listener arrives mapped, and truncating a mapped
/// address would put every IPv4 client in one bucket -- turning a fix
/// for one denial of service into another.
fn limit_key(ip: IpAddr) -> IpAddr {
    match ip.to_canonical() {
        IpAddr::V4(v4) => IpAddr::V4(v4),
        IpAddr::V6(v6) => {
            let s = v6.segments();
            IpAddr::V6(std::net::Ipv6Addr::new(s[0], s[1], s[2], s[3], 0, 0, 0, 0))
        }
    }
}

fn client_ip(req: &Request<Body>, connect_addr: SocketAddr, trusted: &TrustedProxies) -> IpAddr {
    // Canonical, so a proxy that connects over a dual-stack socket and
    // therefore appears as `::ffff:127.0.0.1` still matches the
    // `127.0.0.1` an operator listed. Without this the hub silently
    // stops trusting its own proxy and charges every client to it.
    let peer = connect_addr.ip().to_canonical();
    if !trusted.contains(&peer) {
        return peer;
    }
    let entries: Vec<&str> = req
        .headers()
        .get_all("x-forwarded-for")
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(','))
        .collect();

    // Right to left, stopping at the first entry that is not one of our
    // own proxies. An unreadable entry stops the walk at the peer,
    // because from there leftward nothing is trustworthy.
    for entry in entries.iter().rev() {
        let Some(address) = parse_forwarded_entry(entry) else {
            return peer;
        };
        if !trusted.contains(&address) {
            return address;
        }
    }
    peer
}

/// One `X-Forwarded-For` entry, accepting the forms a real proxy emits:
/// a bare address, `203.0.113.9:51234`, or `[2001:db8::1]:443`.
///
/// The last two because HAProxy and several common nginx and Azure
/// configurations append the client's source port, and brackets are the
/// only unambiguous way to write an IPv6 address with one.
///
/// **`None` has to stop the caller's walk rather than be skipped, and
/// that is the bug this was extracted for.** The old code was
/// `filter_map(|v| v.trim().parse().ok())`, which silently dropped
/// anything it could not read and carried on leftward. A trusted proxy
/// that appended a port therefore made its own entry vanish, and the
/// rightmost survivor became whatever the client had pre-seeded the
/// header with -- a client naming its own rate-limit bucket, and the
/// traffic charged to an innocent address, behind a proxy configured
/// exactly as documented.
///
/// Failing toward the direct peer is the conservative direction: it
/// charges a proxy's whole traffic to one bucket, which is loud and
/// already has an alert pointed at it (§3.2, and the 429-rate expression
/// in `docs/deployment.md` §8.3), where the old behaviour was silent and
/// favoured whoever sent the header.
fn parse_forwarded_entry(entry: &str) -> Option<IpAddr> {
    let entry = entry.trim();
    if let Ok(address) = entry.parse::<IpAddr>() {
        return Some(address);
    }
    // `[v6]:port`, and bare `[v6]`, which some proxies also emit.
    if let Some(rest) = entry.strip_prefix('[') {
        let (inside, _) = rest.split_once(']')?;
        return inside.parse().ok();
    }
    // `v4:port`. Only with exactly one colon: more than one means a bare
    // IPv6 address that already failed to parse above, and lopping off
    // its last group would turn a malformed entry into a plausible wrong
    // answer, which is the failure mode this whole function exists to
    // avoid.
    let (host, port) = entry.split_once(':')?;
    if host.contains(':') || port.contains(':') {
        return None;
    }
    host.parse().ok()
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
    let started = Instant::now();
    let ip = client_ip(&req, connect_addr, &state.trusted_proxies);
    let tier = tier_for(req.method(), req.uri().path());
    // Resolved before the request is consumed by `next.run`, and resolved
    // to a *template* rather than the path itself -- see
    // `metrics::route_template` for why a raw path in a label is a memory
    // leak with a monitoring endpoint in front of it.
    let method = static_method_name(req.method());
    let template = crate::metrics::route_template(req.uri().path());

    if !check_and_record(&state.rate_limits, Bucket::Ip(limit_key(ip), tier), Utc::now()) {
        state.metrics.rate_limited_by_tier[tier.metric_index()].fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        // Recorded as a served request too, not just as a rejection. A
        // 429 is a response the client waited for, and leaving it out of
        // the histogram would make a hub that is rejecting everything
        // look idle rather than overloaded.
        state.metrics.observe_request(method, template, started.elapsed().as_secs_f64(), StatusCode::TOO_MANY_REQUESTS.as_u16());
        return (StatusCode::TOO_MANY_REQUESTS, "rate limit exceeded, slow down").into_response();
    }

    let mut req = req;
    req.extensions_mut().insert(ClientIp(ip));
    let response = next.run(req).await;
    // This is the hub's only per-route timing, and it is deliberately
    // here rather than in each handler. The middleware already wraps the
    // whole router, so one edit covers every route and none of them has
    // to be touched -- which is what lets per-endpoint latency land in
    // the same pass as the rest of the metrics instead of waiting for
    // the handler rewrite plan §10.1 defers. What it measures is the
    // whole served request minus the outer compression layer, which is
    // the number an operator actually wants: the `/leaderboard`
    // pathology in §6.1 is visible in exactly this measurement.
    state.metrics.observe_request(method, template, started.elapsed().as_secs_f64(), response.status().as_u16());
    response
}

/// The method name as a `'static` string, so it can be a metric label
/// without allocating one per request.
///
/// `Method::as_str` borrows from the request, and the per-route table is
/// keyed by `&'static str` to keep the key cheap to hash and impossible
/// to grow without bound. Anything outside the standard set collapses to
/// one shared label rather than being rejected -- an exotic method is
/// still a request that took time, and it is already going to 404 or 405
/// on its own.
fn static_method_name(method: &Method) -> &'static str {
    match *method {
        Method::GET => "GET",
        Method::POST => "POST",
        Method::HEAD => "HEAD",
        Method::PUT => "PUT",
        Method::DELETE => "DELETE",
        Method::PATCH => "PATCH",
        Method::OPTIONS => "OPTIONS",
        _ => "OTHER",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The asymmetry is the whole design, so pin it. A v4 address is a
    /// unit of cost and a v6 address is not -- a residential customer
    /// holds a whole /64, so per-address limiting in v6 limits nothing.
    #[test]
    fn a_v6_network_is_a_64_and_a_v4_network_is_a_24() {
        let v4: IpAddr = "203.0.113.9".parse().unwrap();
        assert_eq!(prefix_of(v4), "203.0.113.0/24");
        assert_eq!(prefix_of("203.0.113.250".parse().unwrap()), prefix_of(v4), "same /24, same bucket");
        assert_ne!(prefix_of("203.0.114.9".parse().unwrap()), prefix_of(v4));

        // Every address in one /64 is one client. Without this an
        // attacker walks 18 quintillion addresses inside a single
        // residential allocation and never fills a bucket.
        let a: IpAddr = "2001:db8:1:2::1".parse().unwrap();
        let b: IpAddr = "2001:db8:1:2:ffff:ffff:ffff:ffff".parse().unwrap();
        assert_eq!(prefix_of(a), prefix_of(b));
        assert_eq!(prefix_of(a), "2001:db8:1:2::/64");

        // ...and the next /64 along is a different client, which is the
        // accepted weakness: a datacentre /48 is 65,536 of these. The
        // global faucet budget is what bounds that case.
        assert_ne!(prefix_of("2001:db8:1:3::1".parse().unwrap()), prefix_of(a));
    }

    /// The same asymmetry, now applied where it was missing: the *rate
    /// limiter's* own bucket key.
    ///
    /// `prefix_of` has always known a v6 address is not a unit of cost,
    /// and the limiter keyed the full address anyway -- so every tier
    /// was unbounded for anyone holding a /64, which is the ordinary
    /// allocation. A node round trip per `/health`, an fsync per faucet
    /// challenge and an ECDSA verify per signed write, all for the cost
    /// of using addresses the client already had.
    #[test]
    fn the_limiter_buckets_a_v6_client_by_its_64_not_by_the_address_it_picked() {
        let first: IpAddr = "2001:db8:1:2::1".parse().unwrap();
        let far: IpAddr = "2001:db8:1:2:ffff:ffff:ffff:ffff".parse().unwrap();
        assert_eq!(
            Bucket::Ip(limit_key(first), Tier::Read),
            Bucket::Ip(limit_key(far), Tier::Read),
            "one allocation is one client however many of its addresses it uses"
        );

        // The next allocation along is a different client, and a v4
        // address is still a client on its own -- neighbours must not
        // start sharing a bucket in the name of fixing v6.
        assert_ne!(limit_key("2001:db8:1:3::1".parse().unwrap()), limit_key(first));
        let v4: IpAddr = "203.0.113.9".parse().unwrap();
        assert_eq!(limit_key(v4), v4);
        assert_ne!(limit_key("203.0.113.10".parse().unwrap()), limit_key(v4));
    }

    /// An IPv4 client reaching a dual-stack listener arrives as
    /// `::ffff:a.b.c.d`. Truncating *that* to a /64 gives `::/64` for
    /// every IPv4 client there is, so the fix above would have handed
    /// the whole internet one shared bucket -- trading one denial of
    /// service for a worse one. Canonicalise before classifying.
    #[test]
    fn a_mapped_v4_client_is_treated_as_the_v4_client_it_is() {
        let mapped: IpAddr = "::ffff:203.0.113.9".parse().unwrap();
        let plain: IpAddr = "203.0.113.9".parse().unwrap();
        assert_eq!(limit_key(mapped), limit_key(plain));
        assert_eq!(prefix_of(mapped), prefix_of(plain));

        // And two different v4 clients stay apart however they arrive.
        let other: IpAddr = "::ffff:198.51.100.9".parse().unwrap();
        assert_ne!(limit_key(other), limit_key(mapped));
        assert_ne!(prefix_of(other), prefix_of(mapped));
    }

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
        // Monitoring's two routes each get their own budget, so neither a
        // read flood nor the other one can make the hub look down.
        assert_eq!(tier_for(&get, "/metrics"), Tier::Metrics);
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
            // The grant itself pays out on chain. Issuing its challenge
            // does not, and is classified below with the local writes.
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
            "/faucet/challenge",
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

    /// `Tier::metric_index` and `metrics::TIER_NAMES` are two halves of
    /// one mapping kept in separate files, so nothing but a test stops
    /// them drifting -- and drift here is silent, mislabelling one tier's
    /// rejections as another's rather than failing.
    #[test]
    fn tier_indices_match_their_metric_names() {
        use crate::metrics::TIER_NAMES;
        for (tier, expected) in [
            (Tier::Health, "health"),
            (Tier::Metrics, "metrics"),
            (Tier::Read, "read"),
            (Tier::Write, "write"),
            (Tier::Chain, "chain"),
        ] {
            assert_eq!(TIER_NAMES[tier.metric_index()], expected, "{tier:?} is mislabelled in the metrics output");
        }
    }

    #[test]
    fn a_metrics_scrape_is_not_charged_to_the_read_budget() {
        let table = new_table();
        let ip: IpAddr = "127.0.0.1".parse().unwrap();
        let now = Utc::now();

        // A client that has exhausted the ordinary read budget -- the
        // exact situation in which an operator most needs to scrape.
        for _ in 0..MAX_REQUESTS_PER_WINDOW {
            assert!(check_and_record(&table, Bucket::Ip(ip, Tier::Read), now));
        }
        assert!(!check_and_record(&table, Bucket::Ip(ip, Tier::Read), now));
        assert!(
            check_and_record(&table, Bucket::Ip(ip, Tier::Metrics), now),
            "a read flood must not 429 the scrape that would explain it"
        );
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

    /// A proxy that appends the client's port must still be understood.
    ///
    /// HAProxy and several stock nginx and Azure configurations write
    /// `address:port`, and bracketed IPv6 is the only unambiguous way to
    /// write one. Every earlier test here used bare addresses, which is
    /// why nothing caught the parser dropping these.
    #[test]
    fn a_forwarded_entry_carrying_a_port_is_still_read() {
        let trusted = parse_trusted_proxies("10.0.0.1").unwrap();
        assert_eq!(
            client_ip(&request_with_forwarded_for(Some("203.0.113.9:51234")), peer("10.0.0.1"), &trusted),
            ip("203.0.113.9"),
            "an IPv4 address with a source port"
        );
        assert_eq!(
            client_ip(&request_with_forwarded_for(Some("[2001:db8::1]:443")), peer("10.0.0.1"), &trusted),
            ip("2001:db8::1"),
            "a bracketed IPv6 address with a port"
        );
        assert_eq!(
            client_ip(&request_with_forwarded_for(Some("[2001:db8::1]")), peer("10.0.0.1"), &trusted),
            ip("2001:db8::1"),
            "brackets with no port, which some proxies also emit"
        );
        assert_eq!(
            client_ip(&request_with_forwarded_for(Some("2001:db8::1")), peer("10.0.0.1"), &trusted),
            ip("2001:db8::1"),
            "and a bare IPv6 address is unaffected"
        );
    }

    /// The bug, stated as the attack: an entry the parser cannot read
    /// must not hand the bucket to whatever the client wrote.
    ///
    /// The old parser dropped unreadable entries and kept walking left,
    /// so a trusted proxy appending a port made its own entry vanish and
    /// the client's pre-seeded one became the rightmost survivor. The
    /// client named its own rate-limit bucket, and the traffic was
    /// charged to an address that had sent nothing -- behind a proxy
    /// configured exactly as the deployment doc describes.
    ///
    /// Falling back to the peer is the conservative answer: it charges
    /// the proxy's whole traffic to one bucket, which is loud and already
    /// alerted on, rather than silently believing the attacker.
    #[test]
    fn an_unreadable_entry_falls_back_to_the_peer_rather_than_trusting_what_is_left_of_it() {
        let trusted = parse_trusted_proxies("10.0.0.1").unwrap();

        // The shape that matters: the client pre-seeds an address, and
        // the proxy appends something this parser cannot read.
        let req = request_with_forwarded_for(Some("203.0.113.9, garbage"));
        assert_eq!(
            client_ip(&req, peer("10.0.0.1"), &trusted),
            ip("10.0.0.1"),
            "the client's entry must not be promoted when the proxy's own is unreadable"
        );

        // A malformed IPv6 must not be salvaged by lopping off what
        // looks like a port -- that would turn a bad entry into a
        // plausible wrong answer.
        let req = request_with_forwarded_for(Some("203.0.113.9, 2001:db8::zz:443"));
        assert_eq!(client_ip(&req, peer("10.0.0.1"), &trusted), ip("10.0.0.1"));
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
    fn a_key_is_limited_across_changing_addresses() {
        let table = new_table();
        let key = "some-pubkey".to_string();
        let now = Utc::now();

        // Every request from a different address, so no per-IP bucket
        // ever fills -- the exact evasion the key axis exists to cover.
        for i in 0..MAX_SIGNED_REQUESTS_PER_PUBKEY_PER_WINDOW {
            let ip: IpAddr = format!("203.0.113.{}", i % 256).parse().unwrap();
            assert!(check_and_record(&table, Bucket::Ip(ip, Tier::Write), now));
            assert!(check_and_record(&table, Bucket::Pubkey(key.clone()), now));
        }
        assert!(
            !check_and_record(&table, Bucket::Pubkey(key), now),
            "the key's own budget must run out even though every address looked idle"
        );
    }

    #[test]
    fn one_keys_exhausted_quota_does_not_touch_another_key_or_its_own_address() {
        let table = new_table();
        let spender = "spender".to_string();
        let now = Utc::now();
        let ip: IpAddr = "127.0.0.1".parse().unwrap();

        for _ in 0..MAX_SIGNED_REQUESTS_PER_PUBKEY_PER_WINDOW {
            assert!(check_and_record(&table, Bucket::Pubkey(spender.clone()), now));
        }
        assert!(!check_and_record(&table, Bucket::Pubkey(spender), now));

        assert!(
            check_and_record(&table, Bucket::Pubkey("neighbour".to_string()), now),
            "another key behind the same address must not inherit an exhausted quota"
        );
        assert!(
            check_and_record(&table, Bucket::Ip(ip, Tier::Write), now),
            "the address's own tiered budget is a separate axis"
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
        // Keys are swept on the same terms as addresses -- one table, one
        // sweep, so neither axis can quietly grow forever.
        check_and_record(&table, Bucket::Pubkey("stale-key".into()), now - Duration::seconds(WINDOW_SECONDS + 5));
        check_and_record(&table, Bucket::Pubkey("fresh-key".into()), now);

        cleanup(&table);

        assert!(table.get(&Bucket::Ip(stale, Tier::Read)).is_none());
        assert!(table.get(&Bucket::Ip(fresh, Tier::Read)).is_some());
        assert!(table.get(&Bucket::Pubkey("stale-key".into())).is_none());
        assert!(table.get(&Bucket::Pubkey("fresh-key".into())).is_some());
    }
}
