//! What the hub can say about itself while it is running.
//!
//! Everything here is counted in memory and rendered on demand at
//! `/metrics`. The two rules that shaped the design, both from the load
//! test written up in `docs/agent-ecosystem-plan.md` §6:
//!
//! 1. **A scrape must never reach the node.** `/metrics` is cheaper to
//!    call than any other route on the hub, so if it fanned out to the
//!    chain it would be the most efficient amplifier on the box -- one
//!    unauthenticated request turning into a TCP round trip that competes
//!    with real payouts for the connection pool. Anything that needs the
//!    node (the chain tip, the custody address's balance) is therefore
//!    sampled by the sweep on its own 60-second cadence and read here as
//!    a plain atomic. `endpoint_does_not_reach_the_node` in `main.rs`
//!    holds us to it.
//! 2. **A scrape must never take the board lock.** Board lock contention
//!    is one of the things we are trying to observe; an observer that
//!    queues for the same lock would be reporting on itself. So the
//!    board-derived numbers (faucet grants, exchange liabilities, open
//!    tasks) are also sampled by the sweep, which already holds that lock
//!    for other reasons, and stored as atomics.
//!
//! The cost of both decisions is staleness: a board-derived gauge is up
//! to one sweep interval old. That is the right trade for an operational
//! dashboard -- a 60-second-old liability figure is still useful, and a
//! `/metrics` route that can be turned into a lever against the hub is
//! not.
//!
//! Prometheus text format, because it is what every scraper already
//! speaks and it is a format you can read with `curl` when the scraper
//! itself is the thing that is broken. It costs one `String` per scrape
//! and no dependency: the exposition format is simple enough to write
//! directly, and pulling in a client library to emit a few dozen lines
//! would add a registry, a global, and a macro layer for no benefit.

use dashmap::DashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

/// Latency histogram bucket upper bounds, in seconds.
///
/// Chosen against measurements rather than by habit. The load test found
/// ordinary reads at about 5 ms and `/leaderboard` and
/// `/reputation/:pubkey` at about 1.3 s under mixed load (§6.1), so the
/// buckets have to resolve both ends: without something between 1 and 2.5
/// the pathology lands in the same bucket as a timeout, and without
/// something below 10 ms every healthy read lands in the first bucket and
/// the p50 is unreadable. The write path is bounded by an fsync (§6.3),
/// which is why the tens-of-milliseconds range is dense too.
const LATENCY_BUCKETS: [f64; 12] =
    [0.001, 0.0025, 0.005, 0.01, 0.025, 0.05, 0.1, 0.25, 0.5, 1.0, 2.5, 10.0];

/// Everything the hub counts about itself.
///
/// Held as an `Arc` on `AppState` rather than a process-wide static, for
/// the reason `rate_limit::RateLimitTable` gives at length: the test
/// suite runs many hub instances in one process, and a global would have
/// them all reporting into one another's numbers. The `Arc` is so the
/// two collaborators that are constructed *before* `AppState` exists --
/// the replay guard and the node client -- can hold the same table the
/// router will later render.
#[derive(Default)]
pub struct Metrics {
    // ---- sweep loop -------------------------------------------------
    /// Passes completed. The denominator for every other sweep number.
    pub sweep_passes: AtomicU64,
    /// Total time spent inside `run_sweep_once`, in milliseconds.
    /// A counter rather than a gauge so a scraper can `rate()` it and get
    /// "fraction of wall-clock the sweep is busy", which is the number
    /// that tells you the 60-second interval is about to stop being long
    /// enough.
    pub sweep_duration_ms_total: AtomicU64,
    /// How long the last pass took. Kept alongside the total because the
    /// tail is what hurts here: a single 40-second pass is an incident
    /// even when the average is 200 ms.
    pub sweep_last_duration_ms: AtomicU64,
    /// How late the last pass started, against the 60-second tick it was
    /// scheduled for. This is the sweep's *lag*, and it is the alert (see
    /// `docs/deployment.md` §8.3): duration says the work got slower, lag
    /// says the work is not being started on time, and only the second
    /// one means payouts are sitting unresolved.
    pub sweep_last_lag_ms: AtomicU64,
    /// The worst lag seen since boot -- a high-water mark, because the
    /// last value alone will read as healthy five seconds after the
    /// incident that mattered.
    pub sweep_max_lag_ms: AtomicU64,
    /// Time the sweep spent waiting to acquire the board's write lock.
    ///
    /// This is the hub's board-contention signal, and it is a deliberately
    /// partial one. Instrumenting every acquisition would mean touching
    /// every handler, which this pass is explicitly not doing (plan
    /// §10.1); what makes the sweep a useful sampler anyway is that it is
    /// the one caller that always wants the *write* lock, so it only
    /// proceeds when every reader has drained. A rising number here means
    /// readers are holding the board long enough to starve the writer,
    /// which is the shape of the contention worth alerting on.
    pub sweep_board_lock_wait_ms_total: AtomicU64,

    // ---- node connection pool ---------------------------------------
    /// Fresh TCP connections dialled and handshaked.
    pub node_connections_opened: AtomicU64,
    /// Operations served by a connection already in the pool.
    pub node_connections_reused: AtomicU64,
    /// Pooled connections that failed mid-exchange and were retried on a
    /// fresh one. The pool cannot tell a peer that closed an idle
    /// connection from a node that just died, so this rising slowly is
    /// normal and this rising sharply is the node restarting.
    pub node_connections_retried: AtomicU64,
    /// Operations that had to wait for a pool permit, i.e. every
    /// connection was already busy. The saturation signal: the pool is a
    /// fixed size, so this is what "the hub is queueing on the node"
    /// looks like before it shows up as request latency.
    pub node_pool_saturation_waits: AtomicU64,
    /// Time spent waiting for that permit, in milliseconds.
    pub node_pool_wait_ms_total: AtomicU64,
    /// Dial attempts that failed outright, across every configured
    /// address. Instrumented before the success path on purpose: a
    /// counter that only moves when things work is how you end up blind
    /// during the incident.
    pub node_connect_failures: AtomicU64,

    // ---- chain observation ------------------------------------------
    /// The last chain height the sweep observed.
    pub chain_height: AtomicU64,
    /// When that observation was taken, as a unix timestamp. Rendered as
    /// an age, because "the height is 41,203" is worthless without
    /// "…and we last checked eleven minutes ago". A stalled chain and a
    /// hub that has lost its node look identical in the height alone and
    /// completely different in the age.
    pub chain_observed_at_unix: AtomicU64,
    /// Sweeps that asked the node for the tip and did not get an answer.
    pub chain_observation_failures: AtomicU64,

    // ---- rate limiting ----------------------------------------------
    /// Per-IP rejections, indexed by `TIER_NAMES`. Split by tier because
    /// the tiers exist to fail independently: reads being throttled while
    /// chain writes flow is a busy dashboard, and the reverse is an
    /// attack.
    pub rate_limited_by_tier: [AtomicU64; TIER_NAMES.len()],
    /// Rejections from the per-key quota, counted separately from every
    /// per-IP tier. These are two different controls answering two
    /// different questions -- "is this address noisy" versus "is this
    /// identity noisy" -- and summing them would hide the case §9 of the
    /// plan warns about, where a misconfigured `--trusted-proxies` charges
    /// every agent to the proxy's address and the per-IP counter alone
    /// looks like a traffic spike.
    pub rate_limited_per_key_quota: AtomicU64,

    // ---- replay guard -----------------------------------------------
    /// Signatures claimed for the first time, i.e. authenticated requests
    /// that got as far as running.
    pub replay_signatures_claimed: AtomicU64,
    /// Envelopes rejected because their signature had already been used.
    pub replay_signatures_rejected: AtomicU64,
    /// Signatures evicted by the sweep once they were too old to replay.
    pub replay_signatures_evicted: AtomicU64,
    /// Claims that could not be written durably. Every one of these is a
    /// burned envelope and a failed request (see `ReplayGuard::claim`), so
    /// this is a page, not a graph.
    pub replay_durable_write_failures: AtomicU64,
    /// Time spent inside the replay guard's durable write, in
    /// milliseconds.
    ///
    /// A first-class metric rather than an afterthought: the load test
    /// measured this fsync at fifteen to twenty-two times the ECDSA
    /// verify in front of it (§6.3), which makes it *the* thing that
    /// bounds the write path. Request rate alone will not explain a slow
    /// hub; this will.
    pub replay_durable_write_ms_total: AtomicU64,

    // ---- faucet -----------------------------------------------------
    /// Distinct keys that have been granted, sampled from the board by
    /// the sweep. See `docs/deployment.md` §8.3 for why the burn in units
    /// is not here yet.
    pub faucet_grants: AtomicU64,

    // ---- exchange solvency ------------------------------------------
    /// Summed `base_balance + locked_base` across every exchange account:
    /// what the hub owes its depositors.
    pub exchange_liabilities: AtomicU64,
    /// The custody address's on-chain balance, as of the last sweep: what
    /// the hub actually holds against that.
    pub exchange_custody_balance: AtomicU64,
    /// Sweeps in which the custody balance could not be fetched, so the
    /// pair above is stale. Without this an operator cannot tell a
    /// solvent hub from one that stopped checking.
    pub exchange_solvency_check_failures: AtomicU64,

    // ---- board --------------------------------------------------------
    /// Tasks on the board that are not yet terminal, sampled by the sweep.
    pub board_open_tasks: AtomicU64,
    /// Payout attempts submitted but not yet confirmed on chain. This is
    /// the plan's "payout retry depth", and it is a gauge rather than a
    /// log count for the reason §8.3 gives: grepping the journal tells you
    /// a retry happened, not how many are outstanding right now.
    pub board_outstanding_payouts: AtomicU64,

    // ---- per-route request timing -------------------------------------
    /// Latency and status counts per route template.
    ///
    /// Populated by the rate-limit middleware, which is the only layer
    /// that sees every request without touching a single handler --
    /// handler-level instrumentation is what plan §10.1 defers, and doing
    /// it here gets the measurement without the collision.
    ///
    /// Keyed by *template* (`/tasks/:id`), never by the raw path: a
    /// pubkey or a uuid in a metric label is unbounded cardinality, which
    /// is how a monitoring endpoint becomes the memory leak that takes the
    /// process down. `route_template` is the whole defence and its
    /// fallthrough is a fixed string.
    pub routes: DashMap<RouteKey, RouteStats>,
}

/// Tier names, in the order `Metrics::rate_limited_by_tier` indexes them.
pub const TIER_NAMES: [&str; 5] = ["health", "metrics", "read", "write", "chain"];

/// One row of the per-route table.
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub struct RouteKey {
    pub method: &'static str,
    pub template: &'static str,
}

/// Latency and outcomes for one route template.
#[derive(Default)]
pub struct RouteStats {
    /// Cumulative bucket counts, parallel to `LATENCY_BUCKETS`. Rendered
    /// as-is; Prometheus histograms are cumulative by definition.
    buckets: [AtomicU64; LATENCY_BUCKETS.len()],
    count: AtomicU64,
    /// Total observed latency in *microseconds*, converted to seconds only
    /// at render time. Accumulating in an integer unit keeps the counter
    /// exact -- summing f64 seconds across millions of requests loses the
    /// small ones into the rounding, which is precisely the 5 ms reads we
    /// are trying to distinguish from the 1.3 s ones.
    micros_total: AtomicU64,
    /// Responses by status class, indexed by `status_class_index`.
    status_classes: [AtomicU64; 5],
}

impl RouteStats {
    fn observe(&self, seconds: f64, status: u16) {
        for (bucket, bound) in self.buckets.iter().zip(LATENCY_BUCKETS.iter()) {
            if seconds <= *bound {
                bucket.fetch_add(1, Ordering::Relaxed);
            }
        }
        self.count.fetch_add(1, Ordering::Relaxed);
        self.micros_total.fetch_add((seconds * 1_000_000.0) as u64, Ordering::Relaxed);
        self.status_classes[status_class_index(status)].fetch_add(1, Ordering::Relaxed);
    }
}

/// Buckets a status code by its first digit: 1xx..5xx into 0..4.
/// Anything outside that range is charged to 5xx, on the principle that an
/// unclassifiable response is a problem rather than a success.
fn status_class_index(status: u16) -> usize {
    match status / 100 {
        1 => 0,
        2 => 1,
        3 => 2,
        4 => 3,
        _ => 4,
    }
}

const STATUS_CLASS_NAMES: [&str; 5] = ["1xx", "2xx", "3xx", "4xx", "5xx"];

/// Collapses a request path to the route template it matched, so that a
/// uuid or a pubkey never reaches a metric label.
///
/// Matched on path segments rather than axum's `MatchedPath`, for the same
/// reason `rate_limit::tier_for` does: this runs from a middleware that
/// wraps the whole router, before a route has been matched, and an
/// unrouted path still has to be counted somewhere bounded.
///
/// The fallthrough is the important arm. Every unrecognised path collapses
/// to a single `other` label, so a client hammering random 404s adds rows
/// to the table exactly once rather than once per path it invents.
/// Deliberately takes no method. Every path this router serves maps to
/// one template regardless of verb (`/tasks` is `/tasks` whether it is
/// being listed or posted to), and the method travels as its own label
/// anyway -- so folding it in here would only be a second way to say the
/// same thing, with two arms per route to keep in agreement.
pub fn route_template(path: &str) -> &'static str {
    let segments: Vec<&str> = path.split('/').filter(|segment| !segment.is_empty()).collect();

    match segments.as_slice() {
        ["health"] => "/health",
        ["metrics"] => "/metrics",
        ["tasks"] => "/tasks",
        ["tasks", "consensus"] => "/tasks/consensus",
        ["tasks", "escrow"] => "/tasks/escrow",
        ["tasks", "consensus", "escrow"] => "/tasks/consensus/escrow",
        ["tasks", "disputable", "escrow"] => "/tasks/disputable/escrow",
        ["tasks", "escrow", _, "confirm"] => "/tasks/escrow/:id/confirm",
        ["tasks", _] => "/tasks/:id",
        ["tasks", _, "claim"] => "/tasks/:id/claim",
        ["tasks", _, "submit"] => "/tasks/:id/submit",
        ["tasks", _, "cancel"] => "/tasks/:id/cancel",
        ["tasks", _, "dispute", "escrow"] => "/tasks/:id/dispute/escrow",
        ["tasks", _, "dispute", "confirm"] => "/tasks/:id/dispute/confirm",
        ["tasks", _, "dispute", "resolve"] => "/tasks/:id/dispute/resolve",
        ["faucet"] => "/faucet",
        ["reputation", _] => "/reputation/:pubkey",
        ["leaderboard"] => "/leaderboard",
        ["board", "summary"] => "/board/summary",
        ["board", "series"] => "/board/series",
        ["names"] => "/names",
        ["llms.txt"] => "/llms.txt",
        ["exchange", "deposit"] => "/exchange/deposit",
        ["exchange", "deposit", _, "confirm"] => "/exchange/deposit/:id/confirm",
        ["exchange", "orders"] => "/exchange/orders",
        ["exchange", "orders", _, "cancel"] => "/exchange/orders/:id/cancel",
        ["exchange", "account", _] => "/exchange/account/:pubkey",
        ["exchange", "withdraw"] => "/exchange/withdraw",
        ["exchange", "trades"] => "/exchange/trades",
        _ => "other",
    }
}

impl Metrics {
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    /// Records one served request against its route template.
    pub fn observe_request(&self, method: &'static str, template: &'static str, seconds: f64, status: u16) {
        let key = RouteKey { method, template };
        // `entry` rather than `get`-then-`insert`: two requests to a route
        // nobody has hit yet arrive concurrently on a fresh hub, and the
        // read-then-write version loses one of them.
        self.routes.entry(key).or_default().observe(seconds, status);
    }

    /// Renders the whole table in Prometheus text exposition format.
    ///
    /// Takes `&self` and allocates one `String`; no locks beyond dashmap's
    /// per-shard ones, and no I/O. That is what makes it safe to expose on
    /// a route that anything is allowed to scrape.
    pub fn render(&self, now_unix: u64) -> String {
        let mut out = String::with_capacity(8 * 1024);

        counter(&mut out, "hub_sweep_passes_total", "Sweep passes completed since boot.", self.sweep_passes.load(Ordering::Relaxed));
        counter(&mut out, "hub_sweep_duration_ms_total", "Cumulative time spent inside a sweep pass.", self.sweep_duration_ms_total.load(Ordering::Relaxed));
        gauge(&mut out, "hub_sweep_last_duration_ms", "Duration of the most recent sweep pass.", self.sweep_last_duration_ms.load(Ordering::Relaxed));
        gauge(&mut out, "hub_sweep_last_lag_ms", "How late the most recent sweep pass started against its 60s tick.", self.sweep_last_lag_ms.load(Ordering::Relaxed));
        gauge(&mut out, "hub_sweep_max_lag_ms", "Worst sweep start lag observed since boot.", self.sweep_max_lag_ms.load(Ordering::Relaxed));
        counter(&mut out, "hub_sweep_board_lock_wait_ms_total", "Cumulative time the sweep waited for the board write lock.", self.sweep_board_lock_wait_ms_total.load(Ordering::Relaxed));

        counter(&mut out, "hub_node_connections_opened_total", "Fresh node connections dialled and handshaked.", self.node_connections_opened.load(Ordering::Relaxed));
        counter(&mut out, "hub_node_connections_reused_total", "Node operations served from a pooled connection.", self.node_connections_reused.load(Ordering::Relaxed));
        counter(&mut out, "hub_node_connections_retried_total", "Pooled connections that failed and were retried on a fresh one.", self.node_connections_retried.load(Ordering::Relaxed));
        counter(&mut out, "hub_node_pool_saturation_waits_total", "Node operations that waited for a pool permit.", self.node_pool_saturation_waits.load(Ordering::Relaxed));
        counter(&mut out, "hub_node_pool_wait_ms_total", "Cumulative time spent waiting for a node pool permit.", self.node_pool_wait_ms_total.load(Ordering::Relaxed));
        counter(&mut out, "hub_node_connect_failures_total", "Node dial attempts that failed on every configured address.", self.node_connect_failures.load(Ordering::Relaxed));

        gauge(&mut out, "hub_chain_height", "Chain height as of the last sweep observation.", self.chain_height.load(Ordering::Relaxed));
        let observed_at = self.chain_observed_at_unix.load(Ordering::Relaxed);
        // Rendered as an age rather than a timestamp so an alert reads
        // `hub_chain_observation_age_seconds > 180` instead of having to
        // subtract from the scraper's own clock. Zero until the first
        // observation lands, which is indistinguishable from fresh -- so
        // the alert in `docs/deployment.md` §8.3 pairs it with the failure
        // counter rather than trusting the age alone.
        let age = if observed_at == 0 { 0 } else { now_unix.saturating_sub(observed_at) };
        gauge(&mut out, "hub_chain_observation_age_seconds", "Seconds since the sweep last read the chain tip.", age);
        counter(&mut out, "hub_chain_observation_failures_total", "Sweeps that could not read the chain tip.", self.chain_observation_failures.load(Ordering::Relaxed));

        out.push_str("# HELP hub_rate_limited_total Requests rejected by the per-IP rate limiter.\n");
        out.push_str("# TYPE hub_rate_limited_total counter\n");
        for (name, counter) in TIER_NAMES.iter().zip(self.rate_limited_by_tier.iter()) {
            out.push_str(&format!("hub_rate_limited_total{{tier=\"{name}\"}} {}\n", counter.load(Ordering::Relaxed)));
        }
        counter(&mut out, "hub_rate_limited_per_key_total", "Signed requests rejected by the per-pubkey quota.", self.rate_limited_per_key_quota.load(Ordering::Relaxed));

        counter(&mut out, "hub_replay_signatures_claimed_total", "Envelope signatures claimed for the first time.", self.replay_signatures_claimed.load(Ordering::Relaxed));
        counter(&mut out, "hub_replay_signatures_rejected_total", "Envelopes rejected as replays.", self.replay_signatures_rejected.load(Ordering::Relaxed));
        counter(&mut out, "hub_replay_signatures_evicted_total", "Signatures evicted by the sweep once unreplayable.", self.replay_signatures_evicted.load(Ordering::Relaxed));
        counter(&mut out, "hub_replay_durable_write_failures_total", "Replay-guard claims that could not be recorded durably.", self.replay_durable_write_failures.load(Ordering::Relaxed));
        counter(&mut out, "hub_replay_durable_write_ms_total", "Cumulative time inside the replay guard's durable write.", self.replay_durable_write_ms_total.load(Ordering::Relaxed));

        gauge(&mut out, "hub_faucet_grants", "Distinct keys granted by the faucet, as of the last sweep.", self.faucet_grants.load(Ordering::Relaxed));

        gauge(&mut out, "hub_exchange_liabilities", "Base units owed to exchange depositors, as of the last sweep.", self.exchange_liabilities.load(Ordering::Relaxed));
        gauge(&mut out, "hub_exchange_custody_balance", "On-chain balance of the custody address, as of the last sweep.", self.exchange_custody_balance.load(Ordering::Relaxed));
        counter(&mut out, "hub_exchange_solvency_check_failures_total", "Sweeps that could not read the custody balance.", self.exchange_solvency_check_failures.load(Ordering::Relaxed));

        gauge(&mut out, "hub_board_open_tasks", "Non-terminal tasks on the board, as of the last sweep.", self.board_open_tasks.load(Ordering::Relaxed));
        gauge(&mut out, "hub_board_outstanding_payouts", "Payouts submitted but not yet confirmed, as of the last sweep.", self.board_outstanding_payouts.load(Ordering::Relaxed));

        out.push_str("# HELP hub_http_requests_total Responses served, by route template and status class.\n");
        out.push_str("# TYPE hub_http_requests_total counter\n");
        // Sorted so a scrape is byte-stable between calls when nothing has
        // changed. Dashmap iterates in shard order, which varies run to
        // run; an unstable ordering makes diffing two captured scrapes --
        // the obvious way to debug this endpoint -- useless.
        let mut rows: Vec<_> = self.routes.iter().collect();
        rows.sort_by(|a, b| (a.key().template, a.key().method).cmp(&(b.key().template, b.key().method)));
        for row in &rows {
            let (method, template) = (row.key().method, row.key().template);
            for (class, counter) in STATUS_CLASS_NAMES.iter().zip(row.value().status_classes.iter()) {
                let value = counter.load(Ordering::Relaxed);
                if value > 0 {
                    out.push_str(&format!(
                        "hub_http_requests_total{{route=\"{template}\",method=\"{method}\",status=\"{class}\"}} {value}\n"
                    ));
                }
            }
        }

        out.push_str("# HELP hub_http_request_duration_seconds Request latency by route template.\n");
        out.push_str("# TYPE hub_http_request_duration_seconds histogram\n");
        for row in &rows {
            let (method, template) = (row.key().method, row.key().template);
            let stats = row.value();
            for (bucket, bound) in stats.buckets.iter().zip(LATENCY_BUCKETS.iter()) {
                out.push_str(&format!(
                    "hub_http_request_duration_seconds_bucket{{route=\"{template}\",method=\"{method}\",le=\"{bound}\"}} {}\n",
                    bucket.load(Ordering::Relaxed)
                ));
            }
            let count = stats.count.load(Ordering::Relaxed);
            out.push_str(&format!(
                "hub_http_request_duration_seconds_bucket{{route=\"{template}\",method=\"{method}\",le=\"+Inf\"}} {count}\n"
            ));
            out.push_str(&format!(
                "hub_http_request_duration_seconds_sum{{route=\"{template}\",method=\"{method}\"}} {:.6}\n",
                stats.micros_total.load(Ordering::Relaxed) as f64 / 1_000_000.0
            ));
            out.push_str(&format!(
                "hub_http_request_duration_seconds_count{{route=\"{template}\",method=\"{method}\"}} {count}\n"
            ));
        }

        out
    }
}

fn counter(out: &mut String, name: &str, help: &str, value: u64) {
    out.push_str(&format!("# HELP {name} {help}\n# TYPE {name} counter\n{name} {value}\n"));
}

fn gauge(out: &mut String, name: &str, help: &str, value: u64) {
    out.push_str(&format!("# HELP {name} {help}\n# TYPE {name} gauge\n{name} {value}\n"));
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The cardinality guard, and the reason this module has a
    /// `route_template` at all.
    ///
    /// A uuid or a pubkey reaching a metric label means one new row per
    /// distinct value, forever, in a table an unauthenticated caller
    /// controls the keys of -- a monitoring endpoint that turns into the
    /// memory leak that takes the process down. Every dynamic segment has
    /// to collapse, and everything unrecognised has to land on one shared
    /// label.
    #[test]
    fn no_request_path_can_grow_the_route_table_without_bound() {
        assert_eq!(route_template("/tasks/019321f0-0000-7000-8000-000000000001"), "/tasks/:id");
        assert_eq!(route_template("/tasks/019321f0-0000-7000-8000-000000000002"), "/tasks/:id");
        assert_eq!(route_template("/reputation/02a1b2c3d4e5f6"), "/reputation/:pubkey");
        assert_eq!(route_template("/exchange/account/02deadbeef"), "/exchange/account/:pubkey");
        assert_eq!(route_template("/tasks/any-id/submit"), "/tasks/:id/submit");

        // The fallthrough. An attacker inventing paths adds one row, once.
        let invented = ["/wp-admin", "/../../etc/passwd", "/tasks/a/b/c/d/e", "/%00", "/a/b/c/d/e/f/g"];
        for path in invented {
            assert_eq!(route_template(path), "other", "{path} must not get its own label");
        }
    }

    /// Every route the router actually serves should be nameable, or the
    /// per-route timing quietly lumps real endpoints in with the 404s --
    /// which is exactly the aggregation that would hide the `/leaderboard`
    /// pathology this instrumentation exists to see (plan §6.1).
    #[test]
    fn every_served_route_has_a_template_of_its_own() {
        for (method, path) in [
            ("GET", "/health"),
            ("GET", "/metrics"),
            ("GET", "/tasks"),
            ("POST", "/tasks"),
            ("POST", "/tasks/consensus"),
            ("POST", "/tasks/escrow"),
            ("POST", "/tasks/consensus/escrow"),
            ("POST", "/tasks/disputable/escrow"),
            ("POST", "/tasks/some-id/claim"),
            ("POST", "/tasks/some-id/cancel"),
            ("POST", "/tasks/some-id/dispute/escrow"),
            ("POST", "/tasks/some-id/dispute/confirm"),
            ("POST", "/tasks/some-id/dispute/resolve"),
            ("POST", "/tasks/escrow/some-id/confirm"),
            ("POST", "/faucet"),
            ("GET", "/leaderboard"),
            ("GET", "/board/summary"),
            ("GET", "/board/series"),
            ("GET", "/names"),
            ("GET", "/llms.txt"),
            ("POST", "/exchange/deposit"),
            ("POST", "/exchange/deposit/some-id/confirm"),
            ("GET", "/exchange/orders"),
            ("POST", "/exchange/orders/some-id/cancel"),
            ("POST", "/exchange/withdraw"),
            ("GET", "/exchange/trades"),
        ] {
            assert_ne!(route_template(path), "other", "{method} {path} is a real route and needs its own label");
        }
    }

    /// The buckets have to separate a healthy read from the pathological
    /// one. The load test put ordinary reads at about 5 ms and
    /// `/leaderboard` at about 1.3 s under mixed load; if those two land
    /// in the same bucket the histogram cannot show the thing it was
    /// built to show.
    #[test]
    fn the_histogram_separates_a_healthy_read_from_a_pathological_one() {
        let metrics = Metrics::default();
        metrics.observe_request("GET", "/tasks", 0.005, 200);
        metrics.observe_request("GET", "/leaderboard", 1.3, 200);

        let rendered = metrics.render(0);
        // A 5 ms read is inside the 5 ms bucket and everything above it.
        assert!(rendered.contains(r#"hub_http_request_duration_seconds_bucket{route="/tasks",method="GET",le="0.005"} 1"#), "{rendered}");
        // A 1.3 s read is in neither the 1 s bucket nor anything below.
        assert!(rendered.contains(r#"hub_http_request_duration_seconds_bucket{route="/leaderboard",method="GET",le="1"} 0"#), "{rendered}");
        assert!(rendered.contains(r#"hub_http_request_duration_seconds_bucket{route="/leaderboard",method="GET",le="2.5"} 1"#), "{rendered}");
    }

    /// Failures have to be visible as failures. A counter that only moves
    /// on success is how an incident looks like idleness.
    #[test]
    fn a_failing_request_is_counted_apart_from_a_succeeding_one() {
        let metrics = Metrics::default();
        metrics.observe_request("POST", "/faucet", 0.01, 200);
        metrics.observe_request("POST", "/faucet", 0.01, 429);
        metrics.observe_request("POST", "/faucet", 0.01, 500);

        let rendered = metrics.render(0);
        for (class, count) in [("2xx", 1), ("4xx", 1), ("5xx", 1)] {
            assert!(
                rendered.contains(&format!(r#"hub_http_requests_total{{route="/faucet",method="POST",status="{class}"}} {count}"#)),
                "{class} missing from:\n{rendered}"
            );
        }
    }

    /// A stale observation has to read as stale. The height alone cannot
    /// distinguish a stalled chain from a hub that lost its node, and the
    /// age is the whole difference.
    #[test]
    fn an_unobserved_chain_reports_no_age_and_an_observed_one_ages() {
        let metrics = Metrics::default();
        assert!(metrics.render(1_000_000).contains("hub_chain_observation_age_seconds 0"));

        metrics.chain_observed_at_unix.store(1_000_000, Ordering::Relaxed);
        assert!(metrics.render(1_000_180).contains("hub_chain_observation_age_seconds 180"));
        // A scraper whose clock is behind the hub's must not produce a
        // negative age wrapping to eighteen quintillion seconds.
        assert!(metrics.render(999_000).contains("hub_chain_observation_age_seconds 0"));
    }

    /// Two scrapes of an unchanged hub should be byte-identical, so that
    /// diffing captured scrapes -- the obvious way to debug this endpoint
    /// -- actually works. Dashmap iterates in shard order, which is not
    /// stable across calls.
    #[test]
    fn a_scrape_is_byte_stable_when_nothing_has_changed() {
        let metrics = Metrics::default();
        for route in ["/tasks", "/leaderboard", "/health", "/exchange/orders", "/names"] {
            metrics.observe_request("GET", route, 0.004, 200);
        }
        assert_eq!(metrics.render(1_000), metrics.render(1_000));
    }
}
