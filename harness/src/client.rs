//! Every hub call the harness makes, in one place.
//!
//! Two things this deliberately does not do. It does not deserialize into
//! the hub's own DTO types: the harness must be able to run against a hub
//! binary built from a different commit than the one it was compiled
//! against (that is the whole point of a chaos drill that restarts the
//! hub, and of a load test re-run after another workstream lands), and a
//! shared struct would turn a field rename into a compile error instead of
//! a measurement. It reads `serde_json::Value` and asks only for the
//! fields it needs. And it does not retry: a retry hides exactly the
//! failures the drills exist to count.

use anyhow::{Context, Result};
use btclib::crypto::PrivateKey;
use sdk::build_envelope;
use serde::Serialize;
use serde_json::Value;
use std::time::{Duration, Instant};

/// One hub reply, with what it cost to get it.
///
/// The status code is kept as a `u16` rather than a `reqwest::StatusCode`
/// because every consumer here either compares it to a number or buckets
/// it, and because the drills report raw codes -- a 429 and a 409 mean
/// completely different things to a load report and both are ordinary.
#[derive(Debug, Clone)]
pub struct Reply {
    pub status: u16,
    pub body: Value,
    pub latency: Duration,
}

impl Reply {
    pub fn ok(&self) -> bool {
        (200..300).contains(&self.status)
    }

    /// The hub's error text, for a reply that carries one. Errors come
    /// back as `{"error": "..."}`; anything else stringifies whole so a
    /// surprise is legible rather than swallowed.
    pub fn error_text(&self) -> String {
        match self.body.get("error").and_then(Value::as_str) {
            Some(text) => text.to_string(),
            None => self.body.to_string(),
        }
    }
}

/// A client for one hub.
///
/// `forwarded_for`, when set, is sent as `X-Forwarded-For`. The hub
/// believes that header only from an address it was started with as a
/// trusted proxy, so this is inert against an ordinarily-configured hub
/// and is only meaningful for the drill that has to look like many source
/// addresses from one box (see `drills::quota_isolation`).
#[derive(Clone)]
pub struct HubClient {
    base_url: String,
    http: reqwest::Client,
    forwarded_for: Option<String>,
}

impl HubClient {
    pub fn new(base_url: &str) -> Result<Self> {
        let http = reqwest::Client::builder()
            // A thousand simulated agents against one host will otherwise
            // spend the run tearing down and redialling sockets, and the
            // measurement becomes one of connection setup rather than of
            // the hub. Well above the concurrency any profile here uses.
            .pool_max_idle_per_host(1024)
            // Long enough that a slow reply is recorded as slow rather
            // than as an error, short enough that a hung hub does not
            // stall a whole run behind one request.
            .timeout(Duration::from_secs(30))
            .build()
            .context("building the HTTP client")?;
        Ok(Self {
            base_url: base_url.trim_end_matches('/').to_string(),
            http,
            forwarded_for: None,
        })
    }

    /// A clone of this client that presents itself as coming from `ip`.
    pub fn from_source(&self, ip: &str) -> Self {
        Self {
            forwarded_for: Some(ip.to_string()),
            ..self.clone()
        }
    }

    pub fn base_url(&self) -> &str {
        &self.base_url
    }

    fn decorate(&self, builder: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        match &self.forwarded_for {
            Some(ip) => builder.header("X-Forwarded-For", ip),
            None => builder,
        }
    }

    /// An unauthenticated read. `path` may carry a query string; only the
    /// signed calls need it kept off, and none of these are signed.
    pub async fn get(&self, path: &str) -> Result<Reply> {
        let started = Instant::now();
        let response = self
            .decorate(self.http.get(format!("{}{path}", self.base_url)))
            .send()
            .await
            .with_context(|| format!("GET {path}"))?;
        Self::finish(response, started).await
    }

    /// A signed write. `path` is bound into the signature, so it is passed
    /// once and used for both the signing string and the request line --
    /// signing one path and sending to another is a 401, and the single
    /// argument is what makes that unrepresentable here.
    pub async fn post_signed<T: Serialize>(
        &self,
        key: &PrivateKey,
        path: &str,
        payload: T,
    ) -> Result<Reply> {
        let envelope = build_envelope(key, "POST", path, payload);
        self.post_envelope(path, &envelope).await
    }

    /// Sends an envelope that was built earlier, possibly for an earlier
    /// request. Only the replay drill wants this; everything else should
    /// use `post_signed`, which cannot get the binding wrong.
    pub async fn post_envelope<T: Serialize>(&self, path: &str, envelope: &T) -> Result<Reply> {
        let started = Instant::now();
        let response = self
            .decorate(self.http.post(format!("{}{path}", self.base_url)))
            .json(envelope)
            .send()
            .await
            .with_context(|| format!("POST {path}"))?;
        Self::finish(response, started).await
    }

    async fn finish(response: reqwest::Response, started: Instant) -> Result<Reply> {
        let status = response.status().as_u16();
        let text = response.text().await.unwrap_or_default();
        let latency = started.elapsed();
        // A body that is not JSON is not an error here: the hub serves
        // `/llms.txt` as text, and an upstream 502 is HTML. Keep it as a
        // JSON string so callers have one shape to handle.
        let body = serde_json::from_str(&text).unwrap_or(Value::String(text));
        Ok(Reply {
            status,
            body,
            latency,
        })
    }
}

// ---------------------------------------------------------------------
// Payload mirrors
// ---------------------------------------------------------------------
//
// Mirrors of the hub's own request payloads. Field name *and declaration
// order* must match the hub's structs exactly: the signing string is built
// from `serde_json::to_string(&payload)`, and the hub independently
// recomputes it from its own statically-typed struct in declaration order.
// A `serde_json::json!` literal would serialize keys alphabetically and
// silently fail signature verification instead. `hub/examples/smoke_agent.rs`
// carries the same warning, and got it wrong once; nothing but running this
// against a live hub catches it.
//
// `Uuid`-typed fields are mirrored as `String`, which serializes to
// byte-identical JSON.

#[derive(Serialize)]
pub struct CreateTaskPayload {
    pub description: String,
    pub bounty: u64,
    pub expected_output_hash: String,
    pub min_reputation: u64,
    pub capabilities: Vec<String>,
}

#[derive(Serialize)]
pub struct ClaimPayload {
    pub task_id: String,
}

#[derive(Serialize)]
pub struct SubmitPayload {
    pub task_id: String,
    pub output: String,
}

#[derive(Serialize)]
pub struct CancelPayload {
    pub task_id: String,
}

#[derive(Serialize)]
pub struct EscrowDisputableTaskPayload {
    pub description: String,
    pub bounty: u64,
    pub dispute_window_minutes: i64,
    pub min_reputation: u64,
    pub capabilities: Vec<String>,
}

#[derive(Serialize)]
pub struct DisputeEscrowPayload {
    pub task_id: String,
    pub reason: String,
}

#[derive(Serialize)]
pub struct ConfirmDisputeEscrowPayload {
    pub task_id: String,
    pub escrow_id: String,
}

/// `outcome` is the hub's `DisputeResolution`, which serializes
/// snake_case -- `"assignee_wins"` / `"challenger_wins"`. Kept a plain
/// string rather than mirroring the enum, so the harness has one less
/// type to drift out of sync with the hub.
#[derive(Serialize)]
pub struct ResolveDisputePayload {
    pub task_id: String,
    pub outcome: String,
}

#[derive(Serialize)]
pub struct ConfirmEscrowPayload {
    pub escrow_id: String,
}

#[derive(Serialize)]
pub struct PlaceOrderPayload {
    pub side: &'static str,
    pub price: u64,
    pub quantity: u64,
}

#[derive(Serialize)]
pub struct CancelOrderPayload {
    pub order_id: String,
}

#[derive(Serialize)]
pub struct WithdrawPayload {
    pub amount: u64,
}

/// What `POST /faucet` carries now that the grant is priced in work.
#[derive(Serialize)]
pub struct FaucetClaimPayload {
    pub challenge_id: String,
    pub solution: u64,
}

/// Asks for a proof-of-work challenge. The first of the faucet's two
/// legs, and an **ordinary-tier** write -- the redemption is the
/// chain-tier one. A drill probing one tier and not the other has to
/// call the legs separately for that reason; see
/// `drills::rate_limit_tiers`.
pub async fn faucet_challenge(client: &HubClient, key: &PrivateKey) -> Result<Reply> {
    client.post_signed(key, "/faucet/challenge", ()).await
}

/// Finds a solution to `challenge`, rebuilding the preimage from the
/// wire fields the way a third-party client has to.
///
/// Deliberately does not import the hub's own `Challenge` type. The
/// harness runs against a hub binary built from another commit (see this
/// module's header), and more to the point, reconstructing the preimage
/// from `preimage_template` is the step an SDK gets wrong -- doing it
/// here means the drills would notice.
pub fn solve_faucet_challenge(challenge: &Value) -> Result<u64> {
    let template = challenge["preimage_template"]
        .as_str()
        .context("the challenge carries no preimage_template")?;
    let target = btclib::U256::from_str_radix(
        challenge["target"].as_str().context("the challenge carries no target")?,
        16,
    )
    .context("the challenge's target is not hex")?;
    // Little-endian, which is what `Hash::hash_bytes` compares and the
    // one convention a client can get backwards while still appearing to
    // work.
    Ok((0u64..)
        .find(|n| {
            btclib::sha256::Hash::hash_bytes(
                template.replace("{solution}", &n.to_string()).as_bytes(),
            )
            .matches_target(target)
        })
        .expect("u64 is not exhaustible in practice"))
}

/// Presents a solved `challenge`. The chain-tier leg: this is the one
/// that costs the operator a payment.
pub async fn redeem_faucet(
    client: &HubClient,
    key: &PrivateKey,
    challenge: &Value,
) -> Result<Reply> {
    let payload = FaucetClaimPayload {
        challenge_id: challenge["challenge_id"]
            .as_str()
            .context("the challenge carries no challenge_id")?
            .to_string(),
        solution: solve_faucet_challenge(challenge)?,
    };
    client.post_signed(key, "/faucet", payload).await
}

/// The whole faucet flow for `key`: ask, solve, redeem.
///
/// **This used to be a payload-less `POST /faucet` and had been wrong
/// since the proof of work landed (plan §5.2).** The hub answers an
/// envelope whose payload does not deserialize with a 422 before any
/// handler runs, so every drill that went through here was measuring a
/// rejected body: `payout-ceiling` in particular would have counted zero
/// grants, read that as a ceiling comfortably held, and reported
/// `Confirmed` against a hub it never asked for a single payout.
///
/// The returned `latency` is the whole flow including the solve, because
/// that is what an arriving agent actually waits through and what §7.1's
/// time-to-first-payout is made of. A drill that wants one leg on its own
/// should call `faucet_challenge` and `redeem_faucet` directly.
///
/// If the challenge leg fails, its reply is what comes back -- a 409 for
/// an already-granted key, or a 429, is the answer to "did the faucet
/// serve this agent" and must not be hidden behind a redemption that
/// never happened.
pub async fn claim_faucet(client: &HubClient, key: &PrivateKey) -> Result<Reply> {
    let started = Instant::now();
    let challenge = faucet_challenge(client, key).await?;
    if !challenge.ok() {
        return Ok(challenge);
    }
    let mut claim = redeem_faucet(client, key, &challenge.body).await?;
    claim.latency = started.elapsed();
    Ok(claim)
}
