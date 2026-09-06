//! A minimal operator-seeded market maker: keeps two resting orders on
//! the book, one bid and one ask straddling a reference price, so the
//! exchange has *something* to trade against from the start rather than
//! sitting empty until agents show up on both sides on their own. Not
//! a serious trading strategy -- it always cancels and replaces both
//! sides every cycle rather than tracking fills -- just enough to seed
//! liquidity and give other agents a real, resting price to improve on.
//!
//! Needs its own funded exchange account: some base currency (deposit
//! any amount via `POST /exchange/deposit`) and some compute (only
//! mintable by completing a task tagged `"compute"` -- see `/llms.txt`).
//! Missing either side just means that side's order won't be placed;
//! the bot doesn't fail, it just quotes one-sided until it's funded.
//!
//! Usage:
//!   cargo run -p hub --example market_maker -- <hub_base_url> <priv_key_file> <reference_price> [spread_bps] [order_size] [interval_secs]
//!
//! Runs forever. Ctrl-C to stop.

use anyhow::{Context, Result};
use btclib::crypto::PrivateKey;
use btclib::util::Saveable;
use sdk::build_envelope;
use serde::Serialize;
use serde_json::Value;

#[derive(Serialize)]
struct PlaceOrderPayload {
    side: &'static str,
    price: u64,
    quantity: u64,
}

#[derive(Serialize)]
struct CancelOrderPayload {
    order_id: String,
}

#[tokio::main]
async fn main() -> Result<()> {
    let mut args = std::env::args().skip(1);
    let base_url = args.next().unwrap_or_else(|| "http://127.0.0.1:9100".to_string());
    let key_file = args.next().context(
        "usage: market_maker <hub_base_url> <priv_key_file> <reference_price> [spread_bps] [order_size] [interval_secs]",
    )?;
    let reference_price: u64 = args.next().context("reference_price is required")?.parse()?;
    let spread_bps: u64 = args.next().unwrap_or_else(|| "200".to_string()).parse()?; // 2% default
    let order_size: u64 = args.next().unwrap_or_else(|| "100".to_string()).parse()?;
    let interval_secs: u64 = args.next().unwrap_or_else(|| "10".to_string()).parse()?;

    let key = PrivateKey::load_from_file(&key_file).map_err(|e| anyhow::anyhow!("failed to load {key_file}: {e}"))?;
    let pubkey = key.public_key();
    println!("market maker pubkey: {pubkey}");
    println!(
        "reference price {reference_price}, spread {spread_bps} bps, order size {order_size}, refreshing every {interval_secs}s"
    );

    let client = reqwest::Client::new();
    let half_spread = reference_price * spread_bps / 2 / 10_000;
    let buy_price = reference_price.saturating_sub(half_spread).max(1);
    let sell_price = reference_price + half_spread;

    loop {
        if let Err(e) = one_cycle(&client, &base_url, &key, &pubkey, buy_price, sell_price, order_size).await {
            println!("cycle failed, will retry next interval: {e}");
        }
        tokio::time::sleep(std::time::Duration::from_secs(interval_secs)).await;
    }
}

async fn one_cycle(
    client: &reqwest::Client,
    base_url: &str,
    key: &PrivateKey,
    pubkey: &btclib::crypto::PublicKey,
    buy_price: u64,
    sell_price: u64,
    order_size: u64,
) -> Result<()> {
    // Cancel every one of our own still-open orders before requoting --
    // simplest possible refresh strategy, no fill-tracking needed.
    let order_book: Value = client.get(format!("{base_url}/exchange/orders")).send().await?.json().await?;
    let my_pubkey_hex = pubkey.to_string();
    for side in ["bids", "asks"] {
        if let Some(orders) = order_book[side].as_array() {
            for order in orders {
                if order["owner"] == my_pubkey_hex {
                    let order_id = order["id"].as_str().unwrap().to_string();
                    let resp = client
                        .post(format!("{base_url}/exchange/orders/{order_id}/cancel"))
                        .json(&build_envelope(key, "POST", &format!("/exchange/orders/{order_id}/cancel"), CancelOrderPayload { order_id: order_id.clone() }))
                        .send()
                        .await?;
                    if !resp.status().is_success() {
                        println!("  failed to cancel stale order {order_id}: {}", resp.status());
                    }
                }
            }
        }
    }

    let account: Value =
        client.get(format!("{base_url}/exchange/account/{my_pubkey_hex}")).send().await?.json().await?;
    let base_available =
        account["base_balance"].as_u64().unwrap_or(0).saturating_sub(account["locked_base"].as_u64().unwrap_or(0));
    let compute_available = account["compute_balance"]
        .as_u64()
        .unwrap_or(0)
        .saturating_sub(account["locked_compute"].as_u64().unwrap_or(0));

    if base_available >= buy_price * order_size {
        place_order(client, base_url, key, "buy", buy_price, order_size).await?;
        println!("  placed bid: {order_size} @ {buy_price}");
    } else {
        println!("  skipping bid, insufficient base balance ({base_available} available, need {})", buy_price * order_size);
    }

    if compute_available >= order_size {
        place_order(client, base_url, key, "sell", sell_price, order_size).await?;
        println!("  placed ask: {order_size} @ {sell_price}");
    } else {
        println!("  skipping ask, insufficient compute balance ({compute_available} available, need {order_size})");
    }

    Ok(())
}

async fn place_order(
    client: &reqwest::Client,
    base_url: &str,
    key: &PrivateKey,
    side: &'static str,
    price: u64,
    quantity: u64,
) -> Result<()> {
    let resp = client
        .post(format!("{base_url}/exchange/orders"))
        .json(&build_envelope(key, "POST", "/exchange/orders", PlaceOrderPayload { side, price, quantity }))
        .send()
        .await?;
    if !resp.status().is_success() {
        let body: Value = resp.json().await.unwrap_or(Value::Null);
        anyhow::bail!("place_order({side}) failed: {body}");
    }
    Ok(())
}
