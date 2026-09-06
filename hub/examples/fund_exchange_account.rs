//! Throwaway helper: deposits on-chain funds into the caller's own
//! exchange ledger balance in one shot (reserve, send, confirm), for
//! manual testing without going through the full smoke-test flow.
//! Usage: cargo run -p hub --example fund_exchange_account -- <hub_base_url> <node_addr> <priv_key_file> <amount>

use anyhow::{Context, Result};
use btclib::crypto::{PrivateKey, PublicKey};
use btclib::network::Message;
use btclib::payment::build_payment;
use btclib::types::TransactionOutput;
use btclib::util::Saveable;
use sdk::build_envelope;
use serde::Serialize;
use serde_json::Value;
use tokio::net::TcpStream;

#[derive(Serialize)]
struct ConfirmExchangeDepositPayload {
    escrow_id: String,
}

#[tokio::main]
async fn main() -> Result<()> {
    let mut args = std::env::args().skip(1);
    let base_url = args.next().unwrap_or_else(|| "http://127.0.0.1:9100".to_string());
    let node_address = args.next().context("usage: fund_exchange_account <hub_base_url> <node_addr> <priv_key_file> <amount>")?;
    let key_file = args.next().context("priv_key_file required")?;
    let amount: u64 = args.next().context("amount required")?.parse()?;

    let key = PrivateKey::load_from_file(&key_file).map_err(|e| anyhow::anyhow!("{e}"))?;
    let client = reqwest::Client::new();

    let reservation: Value =
        client.post(format!("{base_url}/exchange/deposit")).json(&build_envelope(&key, "POST", "/exchange/deposit", ())).send().await?.json().await?;
    let escrow_id = reservation["escrow_id"].as_str().unwrap().to_string();
    let deposit_pubkey = PublicKey::from_sec1_bytes(&hex::decode(reservation["deposit_address"].as_str().unwrap())?)?;
    println!("reserved deposit {escrow_id}, address {}", reservation["deposit_address"]);

    let mut stream = TcpStream::connect(&node_address).await?;
    btclib::network::perform_handshake_initiator(&mut stream).await.map_err(|e| anyhow::anyhow!("{e}"))?;
    Message::FetchUTXOs(key.public_key()).send_async(&mut stream).await?;
    let utxos: Vec<(TransactionOutput, bool)> = match Message::receive_async(&mut stream).await? {
        Message::UTXOs(u) => u,
        other => anyhow::bail!("unexpected: {other:?}"),
    };
    let available: Vec<(bool, TransactionOutput)> = utxos.into_iter().map(|(o, m)| (m, o)).collect();
    let tx = build_payment(&available, &key, deposit_pubkey.clone(), amount, 1_000, key.public_key())?;
    Message::SubmitTransaction(tx).send_async(&mut stream).await?;
    println!("submitted {amount}, waiting for it to confirm...");

    for attempt in 1..=40 {
        let mut s = TcpStream::connect(&node_address).await?;
        btclib::network::perform_handshake_initiator(&mut s).await.map_err(|e| anyhow::anyhow!("{e}"))?;
        Message::FetchUTXOs(deposit_pubkey.clone()).send_async(&mut s).await?;
        let balance = match Message::receive_async(&mut s).await? {
            Message::UTXOs(u) => u.iter().filter(|(_, m)| !m).map(|(o, _)| o.value).sum::<u64>(),
            _ => 0,
        };
        if balance >= amount {
            break;
        }
        println!("  (attempt {attempt}: not confirmed yet)");
        tokio::time::sleep(std::time::Duration::from_secs(3)).await;
    }

    let account: Value = client
        .post(format!("{base_url}/exchange/deposit/{escrow_id}/confirm"))
        .json(&build_envelope(&key, "POST", &format!("/exchange/deposit/{escrow_id}/confirm"), ConfirmExchangeDepositPayload { escrow_id }))
        .send()
        .await?
        .json()
        .await?;
    println!("account after deposit: {account}");
    Ok(())
}
