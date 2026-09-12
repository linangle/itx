mod core;
mod pay;
mod tasks;
mod ui;
mod util;

use anyhow::Result;
use clap::{Parser, Subcommand};
use core::Core;
use cursive::views::TextContent;
use std::path::PathBuf;
use std::sync::Arc;
use tasks::{ui_task, update_balance, update_utxos};
use tracing::*;
use util::{big_mode_btc, generate_dummy_config, setup_panic_hook, setup_tracing};

#[derive(Parser)]
#[command(author, version, about, long_about = None)]
struct Cli {
    #[command(subcommand)]
    command: Option<Commands>,
    #[arg(short, long, value_name = "FILE", default_value_os_t = PathBuf::from("wallet_config.toml"))]
    config: PathBuf,
    #[arg(short, long, value_name = "ADDRESS")]
    node: Option<String>,
}

#[derive(Subcommand)]
enum Commands {
    GenerateConfig {
        #[arg(short, long, value_name = "FILE", default_value_os_t = PathBuf::from("wallet_config.toml"))]
        output: PathBuf,
    },
    /// Pay one address from one key, once, without the TUI.
    ///
    /// For `docs/deployment.md` §9.10: a task reached `PayoutFailed`, the
    /// hub proved the money never moved, and a worker is owed a bounty
    /// nothing will re-drive. It refuses to run while a hub is listening
    /// -- see the module comment in `pay.rs` for why that is a refusal
    /// rather than a warning.
    Pay {
        /// Recipient's public key, hex, as `GET /tasks` reports it.
        #[arg(long, value_name = "PUBKEY_HEX")]
        to: String,
        /// Amount in the chain's smallest unit.
        #[arg(long)]
        amount: u64,
        /// Private key to spend from -- for a bounty, the operator's:
        /// /var/lib/itx/secrets/hub_operator.priv.cbor
        #[arg(long, value_name = "FILE")]
        from: PathBuf,
        /// The node to talk to.
        ///
        /// Its own flag rather than the global `--node`, which clap
        /// requires *before* the subcommand. `wallet --node X pay ...`
        /// is not the order anyone types under pressure, and the node
        /// address is the single most likely thing to need setting here.
        #[arg(long, value_name = "ADDR", default_value = "127.0.0.1:9000")]
        node: String,
        /// Must match the hub's flat fee; see `pay::DEFAULT_FEE`.
        #[arg(long, default_value_t = pay::DEFAULT_FEE)]
        fee: u64,
        /// Where the hub would be listening. Checked, not used: if
        /// anything answers here, this command refuses to run.
        #[arg(long, value_name = "ADDR", default_value = "127.0.0.1:9100")]
        hub_addr: String,
        /// Seconds to watch the chain for the payment to be mined.
        /// 0 reports the mempool state and exits.
        #[arg(long, default_value_t = pay::DEFAULT_WAIT_SECONDS)]
        wait: u64,
        /// Skip the confirmation prompt.
        #[arg(long)]
        yes: bool,
        /// Pay even though a hub is listening. Read pay.rs first.
        #[arg(long)]
        force: bool,
    },
}

#[tokio::main]
async fn main() -> Result<()> {
    // Parsed before tracing is set up, because `setup_tracing` creates a
    // `logs/` directory in the working directory for the TUI's rolling
    // file. `pay` is a one-shot command that reports on stdout and is
    // typically run from somewhere like /root during an incident; it has
    // no business leaving a log directory behind there.
    let cli = Cli::parse();
    if let Some(Commands::Pay { .. }) = cli.command {
        // fall through to the dispatch below without touching tracing
    } else {
        setup_tracing()?;
        setup_panic_hook();
        info!("Starting wallet application");
    }

    match cli.command {
        Some(Commands::GenerateConfig { ref output }) => {
            debug!("Generating dummy config at: {:?}", output);
            return generate_dummy_config(output);
        }
        Some(Commands::Pay { to, amount, from, node, fee, hub_addr, wait, yes, force }) => {
            return pay::run(pay::PayArgs {
                // The global `--node` still wins if someone passed it
                // before the subcommand, so both orders do what they look
                // like they do.
                node: cli.node.clone().unwrap_or(node),
                from,
                to,
                amount,
                fee,
                hub_addr,
                wait_seconds: wait,
                yes,
                force,
            })
            .await;
        }
        None => (),
    }

    info!("Loading config from: {:?}", cli.config);
    let mut core = Core::load(cli.config.clone()).await?;
    if let Some(node) = cli.node {
        info!("Overriding default node with: {}", node);
        core.config.default_node = node;
    }

    let core = Arc::new(core);

    info!("Starting background tasks");
    let balance_content = TextContent::new(big_mode_btc(&core));
    tokio::select! {
        _ = ui_task(core.clone(), balance_content.clone()) => (),
        _ = update_utxos(core.clone()) => (),
        _ = update_balance(core.clone(), balance_content) => (),
    }

    info!("Application shutting down");
    Ok(())
}

// --- CLI implementation (disabled; swap in place of TUI startup below if needed) ---
//
// use core::Recipient;
// use btclib::types::Transaction;
// use std::io::{self, Write};
// use tokio::time::{self, Duration};
//
// async fn update_utxos_cli(core: Arc<Core>) {
//     let mut interval = time::interval(Duration::from_secs(20));
//     loop {
//         interval.tick().await;
//         if let Err(e) = core.fetch_utxos().await {
//             eprintln!("Failed to update UTXOs: {}", e);
//         }
//     }
// }
//
// async fn handle_transactions_cli(
//     rx: kanal::AsyncReceiver<Transaction>,
//     core: Arc<Core>,
// ) {
//     while let Ok(transaction) = rx.recv().await {
//         if let Err(e) = core.send_transaction(transaction).await {
//             eprintln!("Failed to send transaction: {}", e);
//         }
//     }
// }
//
// async fn run_cli(core: Arc<Core>) -> Result<()> {
//     loop {
//         print!("> ");
//         io::stdout().flush()?;
//         let mut input = String::new();
//         io::stdin().read_line(&mut input)?;
//         let parts: Vec<&str> = input.trim().split_whitespace().collect();
//         if parts.is_empty() {
//             continue;
//         }
//         match parts[0] {
//             "balance" => {
//                 println!("Current balance: {} satoshis", core.get_balance());
//             }
//             "send" => {
//                 if parts.len() != 3 {
//                     println!("Usage: send <recipient> <amount>");
//                     continue;
//                 }
//                 let recipient = parts[1];
//                 let amount: u64 = parts[2].parse()?;
//                 let recipient_key = core
//                     .config
//                     .contacts
//                     .iter()
//                     .find(|r| r.name == recipient)
//                     .ok_or_else(|| anyhow::anyhow!("Recipient not found"))?
//                     .load()?
//                     .key;
//                 if let Err(e) = core.fetch_utxos().await {
//                     println!("failed to fetch utxos: {e}");
//                 }
//                 let transaction = core.create_transaction(&recipient_key, amount)?;
//                 core.tx_sender.send(transaction)?;
//                 println!("Transaction sent successfully");
//                 core.fetch_utxos().await?;
//             }
//             "exit" => break,
//             _ => println!("Unknown command"),
//         }
//     }
//     Ok(())
// }
//
// Replace the TUI block in main with:
//
//     tokio::spawn(update_utxos_cli(core.clone()));
//     tokio::spawn(handle_transactions_cli(
//         tx_receiver.clone_async(),
//         core.clone(),
//     ));
//     run_cli(core).await?;
//     Ok(())
//
// fn generate_dummy_config(path: &PathBuf) -> Result<()> {
//     use core::{Config, FeeConfig, FeeType, Recipient};
//     let dummy_config = Config {
//         my_keys: vec![],
//         contacts: vec![
//             Recipient {
//                 name: "Alice".to_string(),
//                 key: PathBuf::from("alice.pub.pem"),
//             },
//             Recipient {
//                 name: "Bob".to_string(),
//                 key: PathBuf::from("bob.pub.pem"),
//             },
//         ],
//         default_node: "127.0.0.1:9000".to_string(),
//         fee_config: FeeConfig {
//             fee_type: FeeType::Percent,
//             value: 0.1,
//         },
//     };
//     let config_str = toml::to_string_pretty(&dummy_config)?;
//     std::fs::write(path, config_str)?;
//     println!("Dummy config generated at: {}", path.display());
//     Ok(())
// }
