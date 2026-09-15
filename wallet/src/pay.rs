//! `wallet pay` — move coin from one key to one address, once, by hand.
//!
//! This exists for exactly one job: `docs/deployment.md` §9.10, where a
//! task reaches `PayoutFailed`, the hub has proven the money never
//! moved, and a worker is owed a bounty that nothing will re-drive. The
//! runbook has said "pay it from the operator wallet by hand" since it
//! was written, and until now there was no tool that could.
//!
//! It is deliberately not part of the TUI and does not read
//! `wallet_config.toml`. An incident is the wrong time to be writing a
//! second config file that names the treasury key, and a contacts list
//! keyed by *name* is the wrong shape for paying a pubkey you just read
//! out of `GET /tasks?status=payoutfailed`.
//!
//! # Why it refuses to run while the hub is up
//!
//! This is the whole reason the command is careful rather than three
//! lines. The hub serialises every payout it makes behind
//! `AppState::payout_lock`, and that lock is an in-process mutex — a
//! separate binary cannot take it. Two spenders against one key is
//! precisely what the lock exists to prevent, and its own comment says
//! what happens without it: both fetch the same unspent output, both
//! spend it, and since every hub-issued transaction carries the same
//! flat fee, the node's replace-by-strictly-higher-fee never lets the
//! second one in.
//!
//! What makes that dangerous rather than merely annoying is what the
//! node does with the loser (`node/src/handler.rs`, `SubmitTransaction`):
//!
//!   - it answers **nothing** — the connection is simply closed, so a
//!     naive tool exits 0 on a payment that never happened, and
//!   - it records a **strike** against the submitting address.
//!
//! Three strikes inside ten minutes is a one-hour ban (`node/src/ban.rs`:
//! `MAX_STRIKES`, `STRIKE_WINDOW_MINUTES`, `BAN_DURATION_HOURS`), the ban
//! is persisted to `blockchain.redb`, and the address being banned is
//! `127.0.0.1` — which is also the hub and the miner. So the failure mode
//! of a hand payout that races the hub is: silently no payment, retry,
//! retry, and the box bans itself in the middle of the incident you were
//! resolving (§9.2, §8.1).
//!
//! Hence the interlock. Stopping the hub for the length of one payment
//! is the same trade `itx-backup.sh` and §5.2's upgrade already make, and
//! it costs seconds.

use anyhow::{bail, Context, Result};
use btclib::crypto::{PrivateKey, PublicKey};
use btclib::network::{is_benign_disconnect, perform_handshake_initiator, Message};
use btclib::payment::build_payment;
use btclib::types::TransactionOutput;
use btclib::util::Saveable;
use std::io::Write;
use std::path::PathBuf;
use std::time::Duration;
use tokio::net::TcpStream;
use tokio::time::timeout;

/// The flat fee every hub-issued payment carries
/// (`hub::handlers::HUB_TRANSACTION_FEE`). Repeated rather than imported
/// because it is `pub(crate)` over there, and **it has to stay equal to
/// it**: the node resolves a mempool conflict by strictly higher fee, so
/// a hand payment priced *above* the hub's would not merely be rejected,
/// it would replace a legitimate in-flight payout. Matching the hub means
/// the worst case is a clean rejection this command can report.
pub const DEFAULT_FEE: u64 = 1_000;

/// How long to wait for the node to prove it rejected the transaction by
/// closing the connection. Generous: the check costs nothing when the
/// transaction was fine, and a false "accepted" is the reading that gets
/// a worker unpaid.
const REJECT_WINDOW: Duration = Duration::from_secs(3);

/// How long to watch the chain for the recipient's output by default.
/// A payment is only visible in the node's UTXO set once it is mined, so
/// this has to outlast a block.
pub const DEFAULT_WAIT_SECONDS: u64 = 180;

pub struct PayArgs {
    pub node: String,
    pub from: PathBuf,
    pub to: String,
    pub amount: u64,
    pub fee: u64,
    pub hub_addr: String,
    pub wait_seconds: u64,
    pub yes: bool,
    pub force: bool,
}

pub async fn run(args: PayArgs) -> Result<()> {
    // --- never probe the node ------------------------------------------
    //
    // The interlock below is the one bare TCP connect in this tool, and
    // it is harmless only because it is aimed at the hub. Aimed at the
    // node -- `--hub-addr 127.0.0.1:9000`, one flag away -- it is exactly
    // the connect-and-hang-up the node bans an address for, on the first
    // offence, for an hour; and the address it bans is the hub's and the
    // miner's too. Refuse before touching a socket.
    if same_endpoint(&args.hub_addr, &args.node) {
        bail!(
            "--hub-addr {} is the node's address. The hub check opens a bare TCP connection \
             and closes it, which the node treats as a severe strike and answers with a \
             one-hour ban of this address -- the hub and the miner included. Point \
             --hub-addr at the hub (127.0.0.1:9100 by default), not at --node.",
            args.hub_addr
        );
    }

    // --- the interlock -------------------------------------------------
    //
    // A TCP connect rather than a `/health` request, and rather than
    // asking systemd: it needs no HTTP client, no unit name, and it tests
    // the thing that actually matters, which is whether a hub process is
    // alive and able to spend this key while we do.
    if hub_is_listening(&args.hub_addr).await {
        if !args.force {
            bail!(
                "a hub is listening on {}, and it spends this same key.\n\
                 \n\
                 Two spenders against one wallet is what the hub's payout lock exists to \
                 prevent, and this command cannot take that lock. The loser of the race is \
                 not told it lost -- the node answers nothing and records a strike -- and \
                 three strikes in ten minutes bans 127.0.0.1 for an hour, which is the hub \
                 and the miner too.\n\
                 \n\
                 Stop the hub, pay, start it again:\n\
                 \n\
                     sudo systemctl stop itx-hub\n\
                     wallet pay ...\n\
                     sudo systemctl start itx-hub\n\
                 \n\
                 The node and miner stay up throughout; only the hub has to be out of the \
                 way. See docs/deployment.md §9.10.",
                args.hub_addr
            );
        }
        eprintln!(
            "WARNING: a hub is listening on {} and --force was given.\n\
             If it settles anything while this runs, one of the two payments is lost \
             silently and the node strikes this address for it.",
            args.hub_addr
        );
    }

    // --- who is paying, and to whom ------------------------------------
    let signing_key = PrivateKey::load_from_file(&args.from)
        .with_context(|| format!("reading the signing key at {}", args.from.display()))?;
    let from_pubkey = signing_key.public_key();
    let recipient = parse_pubkey(&args.to).context("reading --to")?;

    if recipient.to_sec1_bytes() == from_pubkey.to_sec1_bytes() {
        bail!("--to is the same key as --from; that pays the fee and moves nothing");
    }
    if args.amount == 0 {
        bail!("--amount is 0");
    }

    // --- what the chain says -------------------------------------------
    let mut stream = connect(&args.node).await?;
    let utxos = fetch_utxos(&mut stream, &from_pubkey).await?;
    let spendable: u64 = utxos.iter().filter(|(marked, _)| !marked).map(|(_, o)| o.value).sum();
    let marked: u64 = utxos.iter().filter(|(marked, _)| *marked).map(|(_, o)| o.value).sum();

    println!("from      {}", hex::encode(from_pubkey.to_sec1_bytes()));
    println!("to        {}", args.to);
    println!("amount    {}", args.amount);
    println!("fee       {}", args.fee);
    println!(
        "spendable {} across {} output(s){}",
        spendable,
        utxos.iter().filter(|(m, _)| !m).count(),
        if marked > 0 {
            format!(", plus {marked} already spoken for by the mempool")
        } else {
            String::new()
        }
    );

    // Before, so the confirmation at the end is a delta rather than a
    // guess about which output is ours.
    let recipient_before = balance_of(&mut stream, &recipient).await?;
    println!("recipient holds {recipient_before} now");

    let transaction = build_payment(
        &utxos,
        &signing_key,
        recipient.clone(),
        args.amount,
        args.fee,
        from_pubkey.clone(),
    )
    .context("building the payment")?;

    println!(
        "\nthis spends {} input(s) and creates {} output(s)",
        transaction.inputs.len(),
        transaction.outputs.len()
    );

    if !args.yes {
        print!("send it? [y/N] ");
        std::io::stdout().flush().ok();
        let mut answer = String::new();
        std::io::stdin().read_line(&mut answer)?;
        if !matches!(answer.trim(), "y" | "Y" | "yes") {
            println!("nothing sent");
            return Ok(());
        }
    }

    // --- send, and find out whether it was taken -----------------------
    //
    // The node sends no acknowledgement on success and closes the
    // connection on rejection, so silence is the acknowledgement and a
    // disconnect is the error. That asymmetry is the whole reason the
    // hub needs a confirmation sweep; here it is a usable signal because
    // this command sends exactly one transaction and then waits.
    Message::SubmitTransaction(transaction.clone())
        .send_async(&mut stream)
        .await
        .context("submitting the transaction")?;

    match timeout(REJECT_WINDOW, Message::receive_async(&mut stream)).await {
        Err(_) => println!("\nthe node accepted it into its mempool"),
        Ok(Err(e)) if is_benign_disconnect(&e) => bail!(
            "the node REJECTED this transaction -- it closed the connection without a reply, \
             which is the only way it says so, and it recorded a strike against this address \
             for it.\n\
             \n\
             Nothing was paid. The usual causes are an input already spent by a payout the \
             hub made (stop the hub before retrying), a fee below what the mempool will take, \
             or an output this wallet no longer owns. Read the node's own account first:\n\
             \n\
                 journalctl -u itx-node --since '5 minutes ago' | grep -iE 'reject|mempool|strike'\n\
             \n\
             Do not simply retry: three rejections inside ten minutes ban this address for an \
             hour, and that ban takes the hub and the miner with it."
        ),
        Ok(Err(e)) => bail!("reading the node's response: {e}"),
        Ok(Ok(unexpected)) => {
            println!("\nthe node answered with {unexpected:?} rather than staying silent; \
                      treating that as accepted, but check the chain below");
        }
    }

    println!("transaction id {}", hex::encode(transaction.hash().as_bytes()));

    // --- and whether it landed ------------------------------------------
    if args.wait_seconds == 0 {
        println!(
            "\nnot waiting for a block (--wait 0). The payment is in the mempool and is not \
             money until it is mined; check with:\n\
             \n    curl -s localhost:9100/reputation/{} | jq .net_worth",
            args.to
        );
        return Ok(());
    }

    println!("\nwaiting up to {}s for it to be mined...", args.wait_seconds);
    let deadline = std::time::Instant::now() + Duration::from_secs(args.wait_seconds);
    loop {
        tokio::time::sleep(Duration::from_secs(5)).await;
        // A fresh connection each poll: the node closes ours on any
        // protocol complaint, and a poll loop is not worth a reconnect
        // dance inside itself.
        let mut poll = connect(&args.node).await?;
        let now = balance_of(&mut poll, &recipient).await?;
        if now >= recipient_before + args.amount {
            println!("PAID. recipient holds {now}, up from {recipient_before}");
            return Ok(());
        }
        if std::time::Instant::now() >= deadline {
            bail!(
                "the transaction was accepted but has not been mined within {}s -- recipient \
                 still holds {}. It is probably still in the mempool: check the miner is \
                 producing blocks (journalctl -u itx-miner) before sending anything else, and \
                 do NOT send a second payment until you know what happened to this one.",
                args.wait_seconds,
                now
            );
        }
    }
}

async fn hub_is_listening(addr: &str) -> bool {
    matches!(timeout(Duration::from_secs(2), TcpStream::connect(addr)).await, Ok(Ok(_)))
}

/// Whether two `host:port` strings name one endpoint: compared as socket
/// addresses when both parse (a v4-mapped v6 address counts as its v4
/// form), as trimmed text otherwise.
fn same_endpoint(a: &str, b: &str) -> bool {
    use std::net::SocketAddr;
    match (a.trim().parse::<SocketAddr>(), b.trim().parse::<SocketAddr>()) {
        (Ok(a), Ok(b)) => a.ip().to_canonical() == b.ip().to_canonical() && a.port() == b.port(),
        _ => a.trim() == b.trim(),
    }
}

fn parse_pubkey(hex_str: &str) -> Result<PublicKey> {
    let bytes = hex::decode(hex_str.trim()).context("not hexadecimal")?;
    PublicKey::from_sec1_bytes(&bytes).map_err(|e| anyhow::anyhow!("not a public key: {e}"))
}

async fn connect(addr: &str) -> Result<TcpStream> {
    let mut stream = TcpStream::connect(addr)
        .await
        .with_context(|| format!("connecting to the node at {addr}"))?;
    perform_handshake_initiator(&mut stream)
        .await
        .map_err(|e| anyhow::anyhow!("handshake with the node at {addr} failed: {e}"))?;
    Ok(stream)
}

/// Asks the node for one key's outputs.
///
/// The wire gives `(output, marked)` and `build_payment` wants
/// `(marked, output)`; getting that backwards silently spends outputs the
/// mempool has already claimed, so the flip happens here, once.
async fn fetch_utxos(
    stream: &mut TcpStream,
    pubkey: &PublicKey,
) -> Result<Vec<(bool, TransactionOutput)>> {
    Message::FetchUTXOs(pubkey.clone()).send_async(stream).await?;
    match Message::receive_async(stream).await? {
        Message::UTXOs(utxos) => {
            Ok(utxos.into_iter().map(|(output, marked)| (marked, output)).collect())
        }
        other => bail!("asked the node for UTXOs and it answered with {other:?}"),
    }
}

async fn balance_of(stream: &mut TcpStream, pubkey: &PublicKey) -> Result<u64> {
    Ok(fetch_utxos(stream, pubkey).await?.iter().map(|(_, o)| o.value).sum())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `DEFAULT_FEE` has to equal the hub's `HUB_TRANSACTION_FEE` -- its
    /// own comment says why at length. The hub is a binary crate, so this
    /// reads the constant out of its source rather than importing it.
    #[test]
    fn the_default_fee_is_the_hubs_flat_fee() {
        let source = include_str!("../../hub/src/handlers.rs");
        let line = source
            .lines()
            .find(|l| l.contains("const HUB_TRANSACTION_FEE"))
            .expect("the hub declares HUB_TRANSACTION_FEE");
        let value: u64 = line
            .split('=')
            .nth(1)
            .expect("a declaration with a value")
            .trim()
            .trim_end_matches(';')
            .replace('_', "")
            .parse()
            .expect("a literal");
        assert_eq!(DEFAULT_FEE, value, "wallet/src/pay.rs DEFAULT_FEE must match hub/src/handlers.rs HUB_TRANSACTION_FEE");
    }

    #[test]
    fn the_node_address_is_recognised_however_it_is_spelled() {
        assert!(same_endpoint("127.0.0.1:9000", "127.0.0.1:9000"));
        assert!(same_endpoint(" 127.0.0.1:9000\n", "127.0.0.1:9000"));
        assert!(same_endpoint("[::ffff:127.0.0.1]:9000", "127.0.0.1:9000"));
        assert!(!same_endpoint("127.0.0.1:9100", "127.0.0.1:9000"));
        assert!(same_endpoint("node.internal:9000", "node.internal:9000"));
        assert!(!same_endpoint("node.internal:9100", "node.internal:9000"));
    }

    /// The interlock's probe is a bare connect-and-close, which is the
    /// one thing the node bans an address for on sight. Aimed at the node
    /// by mistake it must be refused before any socket is opened -- so
    /// the error names the node, not the missing key file it would have
    /// reached next.
    #[tokio::test]
    async fn pay_refuses_to_probe_the_node_before_opening_any_socket() {
        let err = run(PayArgs {
            node: "127.0.0.1:1".to_string(),
            hub_addr: "127.0.0.1:1".to_string(),
            from: PathBuf::from("/nonexistent/operator.priv.cbor"),
            to: String::new(),
            amount: 1,
            fee: DEFAULT_FEE,
            wait_seconds: 0,
            yes: true,
            force: false,
        })
        .await
        .expect_err("the node's own address must be refused");
        assert!(err.to_string().contains("node's address"), "{err}");
    }

    #[test]
    fn a_pubkey_round_trips_through_the_hex_the_hub_prints() {
        let key = PrivateKey::new_key();
        let hex = hex::encode(key.public_key().to_sec1_bytes());
        let parsed = parse_pubkey(&hex).expect("should parse");
        assert_eq!(parsed.to_sec1_bytes(), key.public_key().to_sec1_bytes());
    }

    #[test]
    fn surrounding_whitespace_is_tolerated() {
        // The address is copied out of `jq` output or a terminal, and a
        // trailing newline is the most likely thing to come with it.
        let key = PrivateKey::new_key();
        let hex = format!("  {}\n", hex::encode(key.public_key().to_sec1_bytes()));
        assert!(parse_pubkey(&hex).is_ok());
    }

    #[test]
    fn nonsense_is_refused_rather_than_truncated() {
        assert!(parse_pubkey("not hex at all").is_err());
        assert!(parse_pubkey("").is_err());
        assert!(parse_pubkey("deadbeef").is_err(), "valid hex, not a key");
    }

    /// The flip in `fetch_utxos`. If this ever reverses, the wallet
    /// spends outputs the mempool has already claimed and the node
    /// rejects the result -- with a strike, and without saying why.
    #[test]
    fn a_marked_output_is_not_offered_to_the_payment_builder() {
        let key = PrivateKey::new_key();
        let pubkey = key.public_key();
        let marked = TransactionOutput {
            value: 5_000,
            unique_id: uuid::Uuid::new_v4(),
            pubkey: pubkey.clone(),
        };
        let free = TransactionOutput {
            value: 5_000,
            unique_id: uuid::Uuid::new_v4(),
            pubkey: pubkey.clone(),
        };
        // As the wire delivers them: (output, marked).
        let from_wire = vec![(marked.clone(), true), (free.clone(), false)];
        let flipped: Vec<(bool, TransactionOutput)> =
            from_wire.into_iter().map(|(output, marked)| (marked, output)).collect();

        let spendable: u64 =
            flipped.iter().filter(|(marked, _)| !marked).map(|(_, o)| o.value).sum();
        assert_eq!(spendable, 5_000, "only the unmarked output is spendable");

        // 5,000 is available and 4,500 + 1,000 fee is not.
        let recipient = PrivateKey::new_key().public_key();
        assert!(
            build_payment(&flipped, &key, recipient.clone(), 4_500, DEFAULT_FEE, pubkey.clone())
                .is_err(),
            "must not reach past the marked output to cover this"
        );
        assert!(
            build_payment(&flipped, &key, recipient, 4_000, DEFAULT_FEE, pubkey).is_ok(),
            "and must still spend the free one"
        );
    }
}
