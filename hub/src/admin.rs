//! The operators' own view of the hub: what is happening, what looks
//! wrong, and who is doing it.
//!
//! # Why this exists as its own surface
//!
//! `/metrics` is for a scraper and is deliberately shaped for one — flat
//! counters, no identities, no cardinality that could grow with traffic
//! (`metrics::RouteKey` documents that constraint at length). Everything
//! an operator wants during an incident is exactly what that shape
//! forbids: *which* network, *which* key, *what* the hub thinks is wrong
//! right now.
//!
//! So this is the other half. It is authenticated, never scraped, and
//! carries identities freely because there is no unbounded label space
//! behind a route only two people can call.
//!
//! # Alerts are computed here, not in the client
//!
//! The thresholds in `docs/deployment.md` §8.3 are the runbook, and a
//! console that re-implemented them would drift from it silently — the
//! console would say "healthy" while the runbook said "page someone",
//! and nobody would notice until an incident. Deriving them once, on the
//! hub, means the two cannot disagree: the console renders what it is
//! told and holds no opinion of its own.
//!
//! # What this is not
//!
//! Not a control plane. Every route here is a read. Moving money,
//! cancelling tasks and resolving disputes stay on the operator key and
//! its existing endpoints, so a viewer key handed to a collaborator can
//! observe everything and change nothing.

use crate::board::{TaskBoard, TaskStatus};
use crate::AppState;
use chrono::{DateTime, Duration, Utc};
use serde::Serialize;
use std::sync::atomic::Ordering;

/// How much of the recent past the activity figures cover.
const ACTIVITY_WINDOW_HOURS: i64 = 24;

/// Networks listed at most, busiest first. A cap because the response is
/// meant to be read by a person: past a screenful the useful signal is
/// the head of the distribution, not the tail.
const MAX_NETWORK_ROWS: usize = 50;

/// How serious a finding is, which decides only how the console orders
/// and colours it. Deliberately two levels: a third invites a middle
/// category that everyone learns to ignore.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Severity {
    /// Money is wrong, or the hub cannot vouch for its own integrity.
    /// Wake someone.
    Critical,
    /// Worth looking at before it becomes the row above.
    Warning,
}

#[derive(Debug, Clone, Serialize)]
pub struct Alert {
    pub severity: Severity,
    /// Stable identifier, so the console can style or suppress a
    /// specific finding without matching on prose.
    pub kind: &'static str,
    pub title: String,
    /// What it means and what to do, in a sentence. The runbook section
    /// is named rather than summarised.
    pub detail: String,
}

/// One network's activity. The clustering view: `prefix_of` decides what
/// a network is (/24 in v4, /64 in v6) and everything here is grouped by
/// that.
#[derive(Debug, Clone, Serialize)]
pub struct NetworkRow {
    pub prefix: String,
    /// Faucet grants from this network inside the window.
    pub grants_in_window: u64,
    pub grants_all_time: u64,
    /// What the next grant from here would cost, as a multiple of the
    /// base. This is the visible edge of the pricing curve, and the
    /// number that says whether a network is being priced out.
    pub next_price_multiplier: u64,
    /// Share of the whole faucet's window budget this one network has
    /// taken, 0.0 to 1.0. One network approaching 1.0 is the shape of a
    /// farm; several networks each at a few percent is a population.
    pub share_of_budget: f64,
}

#[derive(Debug, Clone, Serialize)]
pub struct Agents {
    /// Keys the hub has ever paid, gated or named — its whole known
    /// population.
    pub known: u64,
    /// Keys that have taken a faucet grant. The closest thing to "signed
    /// up", since it is the one step every arrival takes.
    pub funded_by_faucet: u64,
    /// Keys with a settled task behind them. The number that actually
    /// says the product works: everything above it can be had for free.
    pub with_completed_work: u64,
    pub with_failed_work: u64,
    /// Keys holding an exchange balance.
    pub holding_balance: u64,
}

#[derive(Debug, Clone, Serialize)]
pub struct Work {
    pub open: u64,
    pub claimed: u64,
    pub awaiting_settlement: u64,
    pub paid: u64,
    pub closed: u64,
    pub payout_failed: u64,
    /// Bounty riding on consensus tasks that have not settled, against
    /// the ceiling it is allowed. Consensus verifies agreement rather
    /// than correctness, so this is the amount currently exposed to a
    /// colluding majority.
    pub consensus_exposure: u64,
    pub consensus_exposure_ceiling: u64,
}

#[derive(Debug, Clone, Serialize)]
pub struct Faucet {
    pub granted_all_time: u64,
    pub granted_in_window: u64,
    pub granted_units_all_time: u64,
    pub window_budget: u64,
    pub window_remaining: u64,
    pub free_grants_per_network: u64,
    pub doubling_grants: u64,
}

#[derive(Debug, Clone, Serialize)]
pub struct Money {
    pub custody_balance: u64,
    pub exchange_liabilities: u64,
    pub solvent: bool,
    pub payments_pending: u64,
    pub payments_needs_review: u64,
    pub payments_oldest_pending_seconds: u64,
    pub operator_ready_outputs: u64,
    pub custody_ready_outputs: u64,
}

/// The signals that say somebody is probing rather than using.
#[derive(Debug, Clone, Serialize)]
pub struct Integrity {
    /// Envelopes refused as replays. A trickle is clients retrying badly;
    /// a spike is somebody replaying captured traffic.
    pub replays_rejected: u64,
    /// Claims the hub could not record durably. Each one is a request the
    /// client cannot retry, and the store is failing.
    pub burned_envelopes: u64,
    /// This process could not read its replay log at boot, so it lost the
    /// previous process's history.
    pub replay_guard_degraded: bool,
    /// Requests refused by the per-IP tiers and the per-key quota.
    pub rate_limited_by_tier: Vec<(&'static str, u64)>,
    pub rate_limited_per_key: u64,
    /// Records that disagree with each other, recomputed live rather than
    /// read from boot: an operator asking "is the store sane *now*" wants
    /// the answer now.
    pub store_disagreements: Vec<String>,
    pub ledger_store_divergences: u64,
}

#[derive(Debug, Clone, Serialize)]
pub struct Overview {
    pub generated_at: String,
    pub window_hours: i64,
    pub chain_height: u64,
    pub chain_observation_age_seconds: u64,
    pub alerts: Vec<Alert>,
    pub agents: Agents,
    pub work: Work,
    pub faucet: Faucet,
    pub money: Money,
    pub networks: Vec<NetworkRow>,
    pub integrity: Integrity,
}

/// Builds the whole picture under one read lock.
///
/// One acquisition rather than several, for the reason
/// `main::sample_gauges` gives: taking the board repeatedly would let a
/// concurrent write land between two of these figures and produce a view
/// that never existed — an agent count from before a settlement beside a
/// task count from after it. An operator diagnosing an incident from an
/// impossible snapshot is worse off than one who waited.
pub async fn overview(state: &AppState, now: DateTime<Utc>) -> Overview {
    let cutoff = (now - Duration::hours(ACTIVITY_WINDOW_HOURS)).timestamp();
    let m = &state.metrics;

    let (agents, work, networks, granted_in_window, granted_all_time, disagreements, liabilities) = {
        let board = state.board.read().await;
        (
            agents_of(&board),
            work_of(&board, state.consensus_max_exposure),
            networks_of(&board, state, cutoff),
            board.faucet_granted_since(cutoff),
            board.all_faucet_grants().count() as u64,
            crate::reconcile::reconcile(&board)
                .findings
                .into_iter()
                .map(|f| format!("[{}] {}", f.kind.label(), f.detail))
                .collect::<Vec<_>>(),
            board.exchange_liabilities(),
        )
    };

    let custody_balance = m.exchange_custody_balance.load(Ordering::Relaxed);
    let faucet = Faucet {
        granted_all_time,
        granted_in_window,
        granted_units_all_time: m.faucet_granted_units.load(Ordering::Relaxed),
        window_budget: state.faucet_daily_grants,
        window_remaining: state.faucet_daily_grants.saturating_sub(granted_in_window),
        free_grants_per_network: state.faucet_free_grants_per_prefix,
        doubling_grants: state.faucet_pow_doubling_grants,
    };
    let money = Money {
        custody_balance,
        exchange_liabilities: liabilities,
        solvent: custody_balance >= liabilities,
        payments_pending: m.payments_pending.load(Ordering::Relaxed),
        payments_needs_review: m.payments_needs_review.load(Ordering::Relaxed),
        payments_oldest_pending_seconds: m.payments_oldest_pending_seconds.load(Ordering::Relaxed),
        operator_ready_outputs: m.operator_ready_outputs.load(Ordering::Relaxed),
        custody_ready_outputs: m.custody_ready_outputs.load(Ordering::Relaxed),
    };
    let integrity = Integrity {
        replays_rejected: m.replay_signatures_rejected.load(Ordering::Relaxed),
        burned_envelopes: m.replay_durable_write_failures.load(Ordering::Relaxed),
        replay_guard_degraded: m.replay_guard_degraded.load(Ordering::Relaxed) > 0,
        rate_limited_by_tier: crate::metrics::TIER_NAMES
            .iter()
            .enumerate()
            .map(|(i, name)| (*name, m.rate_limited_by_tier[i].load(Ordering::Relaxed)))
            .collect(),
        rate_limited_per_key: m.rate_limited_per_key_quota.load(Ordering::Relaxed),
        store_disagreements: disagreements,
        ledger_store_divergences: m.withdrawal_reverts_not_persisted.load(Ordering::Relaxed),
    };

    let mut overview = Overview {
        generated_at: now.to_rfc3339(),
        window_hours: ACTIVITY_WINDOW_HOURS,
        chain_height: m.chain_height.load(Ordering::Relaxed),
        // Same derivation `/metrics` renders, and zero until the first
        // observation lands -- which is indistinguishable from fresh, so
        // the alert below pairs it with the failure counter rather than
        // trusting the age alone.
        chain_observation_age_seconds: {
            let observed = m.chain_observed_at_unix.load(Ordering::Relaxed);
            if observed == 0 { 0 } else { (now.timestamp() as u64).saturating_sub(observed) }
        },
        alerts: Vec::new(),
        agents,
        work,
        faucet,
        money,
        networks,
        integrity,
    };
    overview.alerts = alerts_for(&overview);
    overview
}

fn agents_of(board: &TaskBoard) -> Agents {
    let mut known: std::collections::BTreeSet<String> = Default::default();
    let mut completed = 0;
    let mut failed = 0;
    for (pubkey, reputation) in board.all_reputation() {
        known.insert(pubkey.to_string());
        if reputation.completed > 0 {
            completed += 1;
        }
        if reputation.failed > 0 {
            failed += 1;
        }
    }
    let funded = board.all_faucet_grants().count() as u64;
    for pubkey in board.all_faucet_grants() {
        known.insert(pubkey.to_string());
    }
    let mut holding = 0;
    for (pubkey, account) in board.all_exchange_accounts() {
        known.insert(pubkey.to_string());
        if account.base_balance > 0 || account.compute_balance > 0 {
            holding += 1;
        }
    }
    Agents {
        known: known.len() as u64,
        funded_by_faucet: funded,
        with_completed_work: completed,
        with_failed_work: failed,
        holding_balance: holding,
    }
}

fn work_of(board: &TaskBoard, ceiling: u64) -> Work {
    let mut w = Work {
        open: 0,
        claimed: 0,
        awaiting_settlement: 0,
        paid: 0,
        closed: 0,
        payout_failed: 0,
        consensus_exposure: board.consensus_exposure(),
        consensus_exposure_ceiling: ceiling,
    };
    for task in board.all_tasks() {
        match task.status {
            TaskStatus::Open => w.open += 1,
            TaskStatus::Claimed => w.claimed += 1,
            TaskStatus::AwaitingDispute | TaskStatus::Disputed | TaskStatus::Verified | TaskStatus::Submitted => {
                w.awaiting_settlement += 1
            }
            TaskStatus::Paid => w.paid += 1,
            TaskStatus::PayoutFailed => w.payout_failed += 1,
            TaskStatus::Closed => w.closed += 1,
        }
    }
    w
}

fn networks_of(board: &TaskBoard, state: &AppState, cutoff: i64) -> Vec<NetworkRow> {
    let mut rows: std::collections::BTreeMap<String, (u64, u64)> = Default::default();
    for (prefix, in_window) in board.faucet_grants_by_prefix(cutoff) {
        let entry = rows.entry(prefix).or_insert((0, 0));
        entry.0 = in_window.0;
        entry.1 = in_window.1;
    }
    let budget = state.faucet_daily_grants.max(1) as f64;
    let mut out: Vec<NetworkRow> = rows
        .into_iter()
        .map(|(prefix, (in_window, all_time))| NetworkRow {
            prefix,
            grants_in_window: in_window,
            grants_all_time: all_time,
            next_price_multiplier: crate::faucet_pow::expected_hashes_for_prefix(
                1,
                in_window,
                state.faucet_free_grants_per_prefix,
                state.faucet_pow_doubling_grants,
            ),
            share_of_budget: in_window as f64 / budget,
        })
        .collect();
    // Busiest first: the head of this distribution is the whole reason
    // an operator opens the page.
    out.sort_by(|a, b| {
        b.grants_in_window
            .cmp(&a.grants_in_window)
            .then_with(|| b.grants_all_time.cmp(&a.grants_all_time))
            .then_with(|| a.prefix.cmp(&b.prefix))
    });
    out.truncate(MAX_NETWORK_ROWS);
    out
}

/// Everything worth waking someone for, derived from the figures above.
///
/// The thresholds are `docs/deployment.md` §8.3's, and they live here
/// rather than in the console so the runbook and the screen cannot
/// disagree. Anything added to one belongs in the other.
fn alerts_for(o: &Overview) -> Vec<Alert> {
    let mut alerts = Vec::new();
    let mut critical = |kind, title: String, detail: String| {
        alerts.push(Alert { severity: Severity::Critical, kind, title, detail })
    };

    if !o.money.solvent {
        critical(
            "insolvent",
            format!(
                "Custody holds {} against {} owed",
                o.money.custody_balance, o.money.exchange_liabilities
            ),
            "The one number where being wrong is a financial statement. Stop withdrawals and \
             reconcile before anything else (deployment.md §8.3, §10.4)."
                .into(),
        );
    }
    if o.money.payments_needs_review > 0 {
        critical(
            "payments_need_review",
            format!("{} payment(s) need a human", o.money.payments_needs_review),
            "The hub has proven it cannot complete these: every resend refused, or no block \
             holds them and their inputs are gone. Nothing further happens automatically \
             (deployment.md §10.4)."
                .into(),
        );
    }
    if !o.integrity.store_disagreements.is_empty() {
        critical(
            "store_disagreement",
            format!("{} record(s) disagree", o.integrity.store_disagreements.len()),
            "Records that no sequence of correct operations could have produced. Each one is a \
             defect somewhere (deployment.md §10.3)."
                .into(),
        );
    }
    if o.integrity.burned_envelopes > 0 {
        critical(
            "burned_envelopes",
            format!("{} envelope(s) burned", o.integrity.burned_envelopes),
            "The replay guard could not record a claim durably, so the request failed and cannot \
             be retried. The store is failing."
                .into(),
        );
    }
    if o.integrity.ledger_store_divergences > 0 {
        critical(
            "ledger_store_divergence",
            format!("{} ledger/store divergence(s)", o.integrity.ledger_store_divergences),
            "A withdrawal credit-back that reached memory and not disk. A restart resolves it \
             against the user, so fix the disk before restarting (deployment.md §9.5)."
                .into(),
        );
    }

    let mut warn = |kind, title: String, detail: String| {
        alerts.push(Alert { severity: Severity::Warning, kind, title, detail })
    };

    if o.integrity.replay_guard_degraded {
        warn(
            "replay_guard_degraded",
            "Replay guard is running without its history".into(),
            "This process could not read its replay log at boot. It refused writes for its first \
             window so nothing is open now, but restart it once the disk is healthy — the gauge \
             cannot clear on its own."
                .into(),
        );
    }
    if o.chain_observation_age_seconds > 180 {
        warn(
            "chain_stale",
            format!("Chain last seen {}s ago", o.chain_observation_age_seconds),
            "Three blocks at the sixteen-second target. The height alone would keep reporting the \
             last value it knew, which is why the age is the alert."
                .into(),
        );
    }
    if o.money.operator_ready_outputs < 4 {
        warn(
            "operator_wallet_flat",
            format!("Operator has {} spendable output(s)", o.money.operator_ready_outputs),
            "About to start refusing grants and settlement, and the 503s will look like a client \
             problem. Expect 0 for one block after a fresh deploy."
                .into(),
        );
    }
    if o.faucet.window_remaining == 0 && o.faucet.window_budget > 0 {
        warn(
            "faucet_budget_exhausted",
            "Faucet budget is spent for this window".into(),
            "Either an attack absorbed exactly as intended, or the budget is below what an honest \
             cohort needs. The network table says which: one prefix near the whole budget is a \
             farm, many small ones are a population."
                .into(),
        );
    }
    if o.money.payments_oldest_pending_seconds > 900 {
        warn(
            "payment_stuck",
            format!(
                "A payment has been pending {}s",
                o.money.payments_oldest_pending_seconds
            ),
            "Brief excursions are normal — a payment is pending by design until the sweep sees it \
             on chain. Climbing without falling means the evidence scan is not reaching the node."
                .into(),
        );
    }
    // Divided rather than cross-multiplied: an unlimited ceiling is
    // u64::MAX and `ceiling * 4` overflows it.
    if o.work.consensus_exposure_ceiling > 0
        && o.work.consensus_exposure >= o.work.consensus_exposure_ceiling / 5 * 4
    {
        warn(
            "consensus_exposure_high",
            format!(
                "Consensus exposure at {} of {}",
                o.work.consensus_exposure, o.work.consensus_exposure_ceiling
            ),
            "Consensus verifies agreement rather than correctness, so this is what a colluding \
             majority could take. Posting more will start being refused."
                .into(),
        );
    }
    // One network holding most of the window's grants is the shape a farm
    // makes. A population makes many small rows instead, which is why
    // this reads the head of the distribution rather than a total.
    if let Some(busiest) = o.networks.first() {
        if busiest.share_of_budget >= 0.5 && busiest.grants_in_window > 1 {
            warn(
                "network_dominates_faucet",
                format!(
                    "{} has taken {:.0}% of the faucet window",
                    busiest.prefix,
                    busiest.share_of_budget * 100.0
                ),
                "One network holding most of the budget is the shape of a farm; many small rows \
                 are a population. The price curve is already charging it more — check whether \
                 that is a shared network being squeezed before tightening anything."
                    .into(),
            );
        }
    }

    alerts
}
