//! The operators' console: a local process that holds an admin key and
//! renders what the hub says about itself.
//!
//! # Why this is a local binary and not a page on the hub
//!
//! Every read on `/admin/overview` is a signed envelope, so something has
//! to hold a private key. A page served by the hub would have to hold it
//! in the browser — in a file input, in `localStorage`, in a form field —
//! and a treasury-adjacent key in a browser is a key in every extension's
//! address space and every screenshot.
//!
//! So the key stays in a process on the operator's own machine. This
//! binds to loopback, signs the one request shape it knows, and serves a
//! page that holds no credential at all: it fetches `/api/overview` from
//! this process, which signs and forwards. The browser never sees a key
//! and never talks to the hub.
//!
//! # Why it holds no opinions
//!
//! Alerts, thresholds and severities are computed by the hub
//! (`hub/src/admin.rs`), against the same numbers `docs/deployment.md`
//! §8.3 alerts on. A console that decided for itself when something was
//! wrong would drift from the runbook silently — showing green while the
//! runbook said page someone. This renders what it is told.
//!
//! # Read-only, and structurally so
//!
//! There is exactly one upstream request in this file and it is a read.
//! An admin key cannot move money even if this binary is compromised,
//! because the hub does not accept it anywhere that can.

use anyhow::{Context, Result};
use axum::{extract::State, http::StatusCode, response::Html, routing::get, Json, Router};
use std::net::SocketAddr;
use std::sync::Arc;

#[derive(argh::FromArgs)]
/// A local window onto a running itx hub.
struct Args {
    #[argh(option, default = "String::from(\"http://127.0.0.1:9100\")")]
    /// the hub to watch.
    hub: String,
    #[argh(option, default = "String::from(\"hub_operator.priv.cbor\")")]
    /// CBOR private key file for an admin or the operator key. Never
    /// leaves this process.
    key_file: String,
    #[argh(option, default = "8787")]
    /// loopback port to serve the console on.
    port: u16,
}

struct Console {
    hub: String,
    key: btclib::crypto::PrivateKey,
    http: reqwest::Client,
}

#[tokio::main]
async fn main() -> Result<()> {
    let args: Args = argh::from_env();
    let bytes = std::fs::read(&args.key_file)
        .with_context(|| format!("could not read the key file {}", args.key_file))?;
    let key: btclib::crypto::PrivateKey = ciborium::de::from_reader(bytes.as_slice())
        .with_context(|| format!("{} is not a CBOR private key", args.key_file))?;

    let console = Arc::new(Console {
        hub: args.hub.trim_end_matches('/').to_string(),
        key,
        http: reqwest::Client::new(),
    });

    let app = Router::new()
        .route("/", get(|| async { Html(include_str!("console.html")) }))
        .route("/api/overview", get(overview))
        .with_state(console.clone());

    // Loopback only, and not configurable. Binding this to an interface
    // would publish an unauthenticated window onto the hub's internals to
    // whatever can reach it -- the page has no login because the security
    // boundary is the socket.
    let addr = SocketAddr::from(([127, 0, 0, 1], args.port));
    let listener = tokio::net::TcpListener::bind(addr).await?;
    println!("itx console watching {} -- open http://{addr}", console.hub);
    println!("signing as {}", console.key.public_key());
    axum::serve(listener, app).await?;
    Ok(())
}

/// Signs one read and hands back whatever the hub said, verbatim.
///
/// Deliberately a pass-through. Reshaping the payload here would put a
/// second opinion between the hub and the screen, and the first thing
/// that goes wrong with two opinions is that nobody knows which one they
/// are looking at.
async fn overview(
    State(console): State<Arc<Console>>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let envelope = sdk::build_envelope(&console.key, "POST", "/admin/overview", ());
    let response = console
        .http
        .post(format!("{}/admin/overview", console.hub))
        .json(&envelope)
        .send()
        .await
        .map_err(|e| (StatusCode::BAD_GATEWAY, format!("could not reach the hub: {e}")))?;

    let status = response.status();
    let body = response.text().await.unwrap_or_default();
    if !status.is_success() {
        // Passed through rather than flattened, because the two failures
        // an operator actually hits here look nothing alike: a 403 means
        // this key is not on --admin-keys, and a 401 usually means the
        // machine's clock has drifted outside the envelope's window.
        return Err((
            StatusCode::from_u16(status.as_u16()).unwrap_or(StatusCode::BAD_GATEWAY),
            body,
        ));
    }
    let value = serde_json::from_str(&body)
        .map_err(|e| (StatusCode::BAD_GATEWAY, format!("hub sent something unreadable: {e}")))?;
    Ok(Json(value))
}
