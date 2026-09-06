//! The itx load and chaos harness.
//!
//! Two halves, and the second is the one that finds bugs.
//!
//! `load` drives roughly a thousand simulated agents through what real
//! agents do -- poll the board, claim, submit, read the leaderboard, place
//! orders -- against a hub you point it at, and reports latency
//! percentiles per request kind.
//!
//! `drills` is a set of deliberate failures, each of which brings up its
//! own stack so it can kill part of it: crash the node mid-payout, restart
//! the hub mid-escrow, storm the replay guard after a restart, saturate
//! one rate-limit tier, exhaust one key's quota, and drive the operator's
//! payout ceiling. Each writes a report naming the plan item it confirms
//! or refutes.
//!
//! See `harness/README.md` for how to run them and what the numbers were
//! on the machine they were first run on.

pub mod chain;
pub mod client;
pub mod drills;
pub mod load;
pub mod report;
pub mod stack;
pub mod stats;
