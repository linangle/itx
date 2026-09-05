//! End-to-end smoke test for the hub HTTP API. Not a unit test -- this
//! drives a real running `hub` (and the node/miner behind it) over HTTP,
//! exactly like an external agent would. Run manually:
//!
//!   cargo run -p hub --example smoke_agent -- <hub_base_url> <operator_priv_key_file> [node_address]
//!
//! Bringing the stack up, with the miner paid to the operator's own
//! address so the hub has something to pay bounties and faucet grants
//! from:
//!
//!   cargo run -p btclib --bin key_gen -- operator
//!   ./target/debug/node  --port 9000 --blockchain-file ./chain.redb
//!   ./target/debug/miner --addresses 127.0.0.1:9000 --public-key-file ./operator.pub.pem
//!   ./target/debug/hub   --port 9100 --node-addresses 127.0.0.1:9000 \
//!       --operator-key-file ./operator.priv.cbor
//!
//! Wait for the operator to actually hold mined coin before running this
//! (`GET /reputation/<operator pubkey>` reports a live `net_worth`);
//! every leg below spends real, mined currency. Do **not** wait on the
//! node by probing its port: a TCP connection that opens without
//! completing the handshake is a severe strike, and the node bans the
//! source IP for an hour on the first offence -- which locks out the
//! miner and hub too, since they share 127.0.0.1. Watch its log for
//! "Listening on" instead.

use anyhow::{Context, Result};
use btclib::crypto::{PrivateKey, PublicKey};
use btclib::network::Message;
use btclib::payment::build_payment;
use btclib::sha256::Hash;
use btclib::types::{Transaction, TransactionOutput};
use btclib::util::Saveable;
use sdk::build_envelope;
use serde::Serialize;
use serde_json::Value;
use std::collections::BTreeSet;
use std::time::Duration;
use tokio::net::TcpStream;

// Mirrors of `hub::handlers::{CreateTaskPayload, ClaimPayload,
// SubmitPayload}` -- field name AND declaration order must match exactly.
// The signing string is built from `serde_json::to_string(&payload)`, and
// the server independently recomputes that same string from its own
// deserialized, statically-typed struct (in declaration order), not from
// whatever bytes were on the wire. A `serde_json::Value` built via `json!`
// would serialize its keys alphabetically instead (it's backed by a
// `BTreeMap`), silently producing a different string and failing
// signature verification for any struct whose fields aren't already in
// alphabetical order. This drifting out of sync with the real struct (it
// was missing `min_reputation`/`capabilities` until this was caught by
// actually running this file against a live hub -- every operator task
// creation here was silently failing signature verification) is exactly
// that failure mode; there is no compiler check tying this mirror to the
// real one, only running it.
#[derive(Serialize)]
struct CreateTaskPayload {
    description: String,
    bounty: u64,
    expected_output_hash: String,
    min_reputation: u64,
    capabilities: BTreeSet<String>,
}

#[derive(Serialize)]
struct ClaimPayload {
    task_id: String,
}

#[derive(Serialize)]
struct SubmitPayload {
    task_id: String,
    output: String,
}

/// Mirror of `hub::handlers::ConfirmEscrowPayload`. Its `escrow_id` is a
/// `Uuid` server-side; a `String` holding the same text serializes to
/// byte-identical JSON, which is all the signing string cares about --
/// the same reason `ClaimPayload::task_id` is a `String` here.
#[derive(Serialize)]
struct ConfirmEscrowPayload {
    escrow_id: String,
}

#[tokio::main]
async fn main() -> Result<()> {
    let mut args = std::env::args().skip(1);
    let base_url = args
        .next()
        .unwrap_or_else(|| "http://127.0.0.1:9100".to_string());
    let operator_key_file = args
        .next()
        .context("usage: smoke_agent <hub_base_url> <operator_priv_key_file> [node_address]")?;
    // Read up front rather than where it's first used: the escrow leg
    // below needs the node to fund a deposit address long before the
    // closing on-chain balance check does.
    let node_address = args.next().unwrap_or_else(|| "127.0.0.1:9000".to_string());

    let operator_key = PrivateKey::load_from_file(&operator_key_file)
        .map_err(|e| anyhow::anyhow!("failed to load operator private key: {e}"))?;
    let agent_key = PrivateKey::new_key();
    println!("agent pubkey: {}", agent_key.public_key());

    let client = reqwest::Client::new();

    println!("\n== GET /llms.txt ==");
    let llms = client
        .get(format!("{base_url}/llms.txt"))
        .send()
        .await?
        .text()
        .await?;
    println!("({} bytes)", llms.len());
    assert!(llms.contains("itx agent hub"));

    println!("\n== POST /faucet (agent) ==");
    let envelope = build_envelope(&agent_key, ());
    let resp = client
        .post(format!("{base_url}/faucet"))
        .json(&envelope)
        .send()
        .await?;
    let status = resp.status();
    let body: Value = resp.json().await?;
    println!("status={status} body={body}");
    assert!(status.is_success(), "faucet claim should succeed");

    println!("\n== POST /faucet again (should be rejected, already claimed) ==");
    let envelope = build_envelope(&agent_key, ());
    let resp = client
        .post(format!("{base_url}/faucet"))
        .json(&envelope)
        .send()
        .await?;
    println!("status={}", resp.status());
    assert_eq!(resp.status(), reqwest::StatusCode::CONFLICT);

    println!("\n== POST /tasks as a non-operator (should be rejected) ==");
    let impostor_key = PrivateKey::new_key();
    let payload = CreateTaskPayload {
        description: "should be rejected".to_string(),
        bounty: 10,
        expected_output_hash: hex::encode(Hash::hash_bytes(b"x").as_bytes()),
        min_reputation: 0,
        capabilities: BTreeSet::new(),
    };
    let envelope = build_envelope(&impostor_key, payload);
    let resp = client
        .post(format!("{base_url}/tasks"))
        .json(&envelope)
        .send()
        .await?;
    println!("status={}", resp.status());
    assert_eq!(resp.status(), reqwest::StatusCode::FORBIDDEN);

    println!("\n== POST /tasks as the operator ==");
    let correct_answer = "the answer is 42";
    let expected_hash = hex::encode(Hash::hash_bytes(correct_answer.as_bytes()).as_bytes());
    let payload = CreateTaskPayload {
        description: "reply with the answer to everything".to_string(),
        bounty: 1_000_000u64,
        expected_output_hash: expected_hash,
        min_reputation: 0,
        capabilities: BTreeSet::new(),
    };
    let envelope = build_envelope(&operator_key, payload);
    let resp = client
        .post(format!("{base_url}/tasks"))
        .json(&envelope)
        .send()
        .await?;
    let status = resp.status();
    let task: Value = resp.json().await?;
    println!("status={status} task={task}");
    assert!(status.is_success(), "operator task creation should succeed");
    let task_id = task["id"].as_str().unwrap().to_string();

    println!("\n== GET /tasks (should list the new task) ==");
    let tasks: Value = client
        .get(format!("{base_url}/tasks"))
        .send()
        .await?
        .json()
        .await?;
    println!("{tasks}");
    // >= 1 (not == 1) since the hub's store persists across runs -- a
    // repeat run of this smoke test against the same store will see
    // whatever earlier runs left behind too.
    assert!(tasks
        .as_array()
        .unwrap()
        .iter()
        .any(|t| t["id"] == task_id));

    println!("\n== POST /tasks/{{id}}/claim (agent) ==");
    let payload = ClaimPayload {
        task_id: task_id.clone(),
    };
    let envelope = build_envelope(&agent_key, payload);
    let resp = client
        .post(format!("{base_url}/tasks/{task_id}/claim"))
        .json(&envelope)
        .send()
        .await?;
    let status = resp.status();
    let claimed_task: Value = resp.json().await?;
    println!("status={status} task={claimed_task}");
    assert!(status.is_success());
    assert_eq!(claimed_task["status"], "Claimed");

    println!("\n== POST /tasks/{{id}}/submit with the WRONG answer ==");
    let payload = SubmitPayload {
        task_id: task_id.clone(),
        output: "definitely wrong".to_string(),
    };
    let envelope = build_envelope(&agent_key, payload);
    let resp = client
        .post(format!("{base_url}/tasks/{task_id}/submit"))
        .json(&envelope)
        .send()
        .await?;
    let status = resp.status();
    let result: Value = resp.json().await?;
    println!("status={status} result={result}");
    assert!(status.is_success());
    assert_eq!(result["verified"], false);

    println!("\n== re-claim then submit the CORRECT answer ==");
    let payload = ClaimPayload {
        task_id: task_id.clone(),
    };
    let envelope = build_envelope(&agent_key, payload);
    client
        .post(format!("{base_url}/tasks/{task_id}/claim"))
        .json(&envelope)
        .send()
        .await?
        .error_for_status()?;

    let payload = SubmitPayload {
        task_id: task_id.clone(),
        output: correct_answer.to_string(),
    };
    let envelope = build_envelope(&agent_key, payload);
    let resp = client
        .post(format!("{base_url}/tasks/{task_id}/submit"))
        .json(&envelope)
        .send()
        .await?;
    let status = resp.status();
    let result: Value = resp.json().await?;
    println!("status={status} result={result}");
    assert!(status.is_success());
    assert_eq!(result["verified"], true);
    assert_eq!(result["paid"], true);

    println!("\n== GET /reputation/{{agent_pubkey}} ==");
    let agent_pubkey = agent_key.public_key().to_string();
    let reputation: Value = client
        .get(format!("{base_url}/reputation/{agent_pubkey}"))
        .send()
        .await?
        .json()
        .await?;
    println!("{reputation}");
    assert_eq!(reputation["completed"], 1);
    assert_eq!(reputation["failed"], 1);
    assert_eq!(reputation["total_earned"], 1_000_000);

    println!("\n== GET /leaderboard ==");
    let leaderboard: Value = client
        .get(format!("{base_url}/leaderboard"))
        .send()
        .await?
        .json()
        .await?;
    println!("{leaderboard}");
    // Same as the /tasks check above: >= 1, not == 1, since the store
    // persists across repeat runs of this smoke test.
    assert!(leaderboard
        .as_array()
        .unwrap()
        .iter()
        .any(|entry| entry["pubkey"] == agent_pubkey && entry["total_earned"] == 1_000_000));

    println!("\n== verify on-chain settlement directly against the node (bypassing hub) ==");
    // Waits for the *full* expected amount rather than for any non-zero
    // balance. The faucet grant and the task bounty are two separate
    // payouts that need not land in the same block, so stopping at the
    // first coin seen made this assert fail whenever the grant mined a
    // block ahead of the bounty.
    let expected_agent_balance = 50_000_000 + 1_000_000;
    let confirmed_balance = wait_for_balance(
        &node_address,
        &agent_key.public_key(),
        expected_agent_balance,
        "the faucet grant and task bounty",
    )
    .await?;
    println!("agent's confirmed on-chain balance: {confirmed_balance}");
    assert_eq!(
        confirmed_balance,
        expected_agent_balance,
        "agent should have actually received both the faucet grant and the task bounty on-chain, not just a hub-side bookkeeping entry"
    );

    // ---- escrow-funded task, settled from the poster's own coin ----
    //
    // Everything above is funded by the operator, so it never exercises
    // an escrow deposit address. This leg does, and it is the only test
    // anywhere that proves the chain itself accepts a spend signed by an
    // escrow key: the hub's HTTP tests drive a fake node that does not
    // verify signatures, so a wrong key would pass them silently. That
    // matters since escrow keys became derived (HKDF over the hub's
    // escrow secret) rather than randomly generated and stored.
    //
    // The agent funds the task out of the balance just confirmed above,
    // and a fresh worker key does the work -- a poster cannot claim its
    // own task.
    let worker_key = PrivateKey::new_key();
    println!("\nworker pubkey: {}", worker_key.public_key());

    println!("\n== POST /tasks/escrow (agent funds a task from its own wallet) ==");
    let escrow_answer = "escrow settles from the poster's own coin";
    let escrow_bounty = 2_000_000u64;
    // `EscrowTaskPayload` server-side has exactly `CreateTaskPayload`'s
    // fields in the same order, so one mirror serializes correctly for
    // both -- see the note on `CreateTaskPayload` for why order matters.
    let payload = CreateTaskPayload {
        description: "escrow-funded: reply with the settling phrase".to_string(),
        bounty: escrow_bounty,
        expected_output_hash: hex::encode(Hash::hash_bytes(escrow_answer.as_bytes()).as_bytes()),
        min_reputation: 0,
        capabilities: BTreeSet::new(),
    };
    let envelope = build_envelope(&agent_key, payload);
    let resp = client
        .post(format!("{base_url}/tasks/escrow"))
        .json(&envelope)
        .send()
        .await?;
    let status = resp.status();
    let reservation: Value = resp.json().await?;
    println!("status={status} reservation={reservation}");
    assert!(status.is_success(), "escrow reservation should succeed");

    let escrow_id = reservation["escrow_id"].as_str().unwrap().to_string();
    let deposit_address = reservation["deposit_address"].as_str().unwrap().to_string();
    let required_amount = reservation["required_amount"].as_u64().unwrap();
    assert_eq!(
        required_amount,
        escrow_bounty + 1_000,
        "the hub asks for the bounty plus its flat transaction fee"
    );
    let deposit_pubkey = PublicKey::from_sec1_bytes(&hex::decode(&deposit_address)?)
        .map_err(|e| anyhow::anyhow!("hub returned an unparseable deposit address: {e}"))?;

    println!("\n== fund the deposit address on-chain, from the agent's own wallet ==");
    let funding_fee = 1_000u64;
    let available = spendable_utxos(&node_address, &agent_key.public_key()).await?;
    let funding_tx = build_payment(
        &available,
        &agent_key,
        deposit_pubkey.clone(),
        required_amount,
        funding_fee,
        agent_key.public_key(),
    )
    .map_err(|e| anyhow::anyhow!("failed to build the escrow funding transaction: {e}"))?;
    submit_transaction(&node_address, funding_tx).await?;
    println!("submitted {required_amount} (fee {funding_fee}) to {deposit_address}");
    wait_for_balance(
        &node_address,
        &deposit_pubkey,
        required_amount,
        "the escrow funding payment",
    )
    .await?;

    println!("\n== POST /tasks/escrow/{{id}}/confirm ==");
    let payload = ConfirmEscrowPayload {
        escrow_id: escrow_id.clone(),
    };
    let envelope = build_envelope(&agent_key, payload);
    let resp = client
        .post(format!("{base_url}/tasks/escrow/{escrow_id}/confirm"))
        .json(&envelope)
        .send()
        .await?;
    let status = resp.status();
    let escrow_task: Value = resp.json().await?;
    println!("status={status} task={escrow_task}");
    assert!(status.is_success(), "confirming a funded escrow should succeed");
    assert_eq!(escrow_task["status"], "Open");
    let escrow_task_id = escrow_task["id"].as_str().unwrap().to_string();

    println!("\n== worker claims and answers the escrow-funded task ==");
    let payload = ClaimPayload {
        task_id: escrow_task_id.clone(),
    };
    let envelope = build_envelope(&worker_key, payload);
    client
        .post(format!("{base_url}/tasks/{escrow_task_id}/claim"))
        .json(&envelope)
        .send()
        .await?
        .error_for_status()?;

    let payload = SubmitPayload {
        task_id: escrow_task_id.clone(),
        output: escrow_answer.to_string(),
    };
    let envelope = build_envelope(&worker_key, payload);
    let resp = client
        .post(format!("{base_url}/tasks/{escrow_task_id}/submit"))
        .json(&envelope)
        .send()
        .await?;
    let status = resp.status();
    let result: Value = resp.json().await?;
    println!("status={status} result={result}");
    assert!(status.is_success());
    assert_eq!(result["verified"], true);
    assert_eq!(result["paid"], true);

    println!("\n== verify the escrow bounty reached the worker on-chain ==");
    // The deposit held exactly `bounty + fee`, and settlement takes the
    // fee out of it, so the worker receives the bounty exactly. Reaching
    // this line at all is the real assertion: the payout is a spend from
    // the derived escrow key, and the chain would have rejected it if
    // that key did not actually control the deposit address.
    let worker_balance = wait_for_balance(
        &node_address,
        &worker_key.public_key(),
        escrow_bounty,
        "the escrow task payout",
    )
    .await?;
    println!("worker's confirmed on-chain balance: {worker_balance}");
    assert_eq!(
        worker_balance, escrow_bounty,
        "the worker should have received the escrowed bounty itself, net of the hub's fee"
    );

    println!("\nALL SMOKE TESTS PASSED");
    Ok(())
}

/// One request/response round trip to the node. Each call opens its own
/// connection rather than holding one across the block-length waits
/// below, which is what the balance polling here spends most of its time
/// doing.
async fn node_round_trip(node_address: &str, request: Message) -> Result<Message> {
    let mut stream = TcpStream::connect(node_address).await?;
    btclib::network::perform_handshake_initiator(&mut stream)
        .await
        .map_err(|e| anyhow::anyhow!("handshake with {node_address} failed: {e}"))?;
    request.send_async(&mut stream).await?;
    Ok(Message::receive_async(&mut stream).await?)
}

/// `pubkey`'s UTXOs in the `(marked, output)` order `build_payment`
/// expects, which is the reverse of what the node sends back.
async fn spendable_utxos(
    node_address: &str,
    pubkey: &PublicKey,
) -> Result<Vec<(bool, TransactionOutput)>> {
    match node_round_trip(node_address, Message::FetchUTXOs(pubkey.clone())).await? {
        Message::UTXOs(utxos) => Ok(utxos
            .into_iter()
            .map(|(output, marked)| (marked, output))
            .collect()),
        other => anyhow::bail!("unexpected response from node: {other:?}"),
    }
}

/// Sum of every UTXO not already spoken for by a pending transaction.
async fn confirmed_balance(node_address: &str, pubkey: &PublicKey) -> Result<u64> {
    Ok(spendable_utxos(node_address, pubkey)
        .await?
        .iter()
        .filter(|(marked, _)| !marked)
        .map(|(_, output)| output.value)
        .sum())
}

/// Polls until `pubkey` holds at least `at_least` confirmed, or gives up.
/// Nothing here is instant: every payment has to be mined, and at a 16s
/// target block time a couple of blocks is a normal wait.
async fn wait_for_balance(
    node_address: &str,
    pubkey: &PublicKey,
    at_least: u64,
    what: &str,
) -> Result<u64> {
    let mut balance = 0;
    for attempt in 1..=40 {
        balance = confirmed_balance(node_address, pubkey).await?;
        if balance >= at_least {
            return Ok(balance);
        }
        println!("  (attempt {attempt}: {what} not yet mined, waiting for the next block...)");
        tokio::time::sleep(Duration::from_secs(3)).await;
    }
    anyhow::bail!("{what} never confirmed: {pubkey} holds {balance}, expected at least {at_least}")
}

/// Fire-and-forget, mirroring the node protocol itself: a submitted
/// transaction is never acknowledged, so the only way to know it landed
/// is to watch the recipient's balance (see `wait_for_balance`).
async fn submit_transaction(node_address: &str, transaction: Transaction) -> Result<()> {
    let mut stream = TcpStream::connect(node_address).await?;
    btclib::network::perform_handshake_initiator(&mut stream)
        .await
        .map_err(|e| anyhow::anyhow!("handshake with {node_address} failed: {e}"))?;
    Message::SubmitTransaction(transaction)
        .send_async(&mut stream)
        .await?;
    Ok(())
}
