//! The harness command line.
//!
//! Two subcommands, matching the two halves. `load` is pointed at a stack
//! somebody else is running; `drill` brings up its own, because it has to
//! be able to kill it.

use anyhow::{Context, Result};
use argh::FromArgs;
use harness::drills;
use harness::load::{self, LoadConfig};
use harness::report::{Environment, Report};
use harness::stack::{Stack, StackConfig};
use std::path::PathBuf;
use std::time::Duration;

#[derive(FromArgs)]
/// itx load and chaos harness -- drives a hub at scale, and breaks it on
/// purpose.
struct Args {
    #[argh(subcommand)]
    command: Command,
}

#[derive(FromArgs)]
#[argh(subcommand)]
enum Command {
    Load(LoadArgs),
    Drill(DrillArgs),
    Stack(StackArgs),
    Compare(CompareArgs),
}

#[derive(FromArgs)]
/// Diff a fresh report against a checked-in baseline.
///
/// Exits non-zero if anything got worse: a verdict that turned into a
/// refutation, or a finding that was not there before. A finding going
/// away is somebody's fix landing and is not a failure.
#[argh(subcommand, name = "compare")]
struct CompareArgs {
    #[argh(option)]
    /// the checked-in baseline report
    baseline: PathBuf,
    #[argh(option)]
    /// the report from the run being judged
    against: PathBuf,
}

#[derive(FromArgs)]
/// Drive a running hub with simulated agents and report latency.
#[argh(subcommand, name = "load")]
struct LoadArgs {
    #[argh(option, default = "String::from(\"http://127.0.0.1:9140\")")]
    /// base URL of the hub to drive
    hub: String,
    #[argh(option)]
    /// the node behind that hub, needed only to fund exchange makers
    node: Option<String>,
    #[argh(option, default = "1000")]
    /// how many agents to simulate
    agents: usize,
    #[argh(option, default = "60")]
    /// how long to run, in seconds
    seconds: u64,
    #[argh(option, default = "1000")]
    /// milliseconds an agent waits between actions; agents/tick is the
    /// offered request rate
    tick_ms: u64,
    #[argh(option, default = "8")]
    /// how many agents also trade on the exchange
    makers: usize,
    #[argh(option, default = "200")]
    /// how many open tasks to make sure exist first
    task_pool: usize,
    #[argh(option)]
    /// a private key with coin, used to seed tasks and fund makers
    funding_key: Option<PathBuf>,
    #[argh(switch)]
    /// give each agent its own synthetic source address. Requires a hub
    /// started with --trusted-proxies 127.0.0.1; without this a thousand
    /// agents share one address and the run measures the rate limiter
    distinct_sources: bool,
    #[argh(option)]
    /// where to write the JSON report
    out: Option<PathBuf>,
}

#[derive(FromArgs)]
/// Run a chaos drill against a stack the harness brings up itself.
#[argh(subcommand, name = "drill")]
struct DrillArgs {
    #[argh(positional)]
    /// which drill, or `all`
    name: String,
    #[argh(option, default = "default_build_dir()")]
    /// the build directory to take node/miner/hub from
    build_dir: PathBuf,
    #[argh(option, default = "default_work_root()")]
    /// where to put chain data, stores and logs. Wiped per drill
    work_root: PathBuf,
    #[argh(option, default = "PathBuf::from(\".\")")]
    /// the repository, used to record the commit a run measured
    repo: PathBuf,
    #[argh(option)]
    /// directory to write JSON reports into
    out: Option<PathBuf>,
}

#[derive(FromArgs)]
/// Bring up a node, miner and hub and hold them there, for the load half
/// to be pointed at.
///
/// The drills start their own stacks because they have to kill them; load
/// runs against one somebody else is running, and this is the somebody
/// else. It prints the operator key's path, which is what `load
/// --funding-key` wants.
#[argh(subcommand, name = "stack")]
struct StackArgs {
    #[argh(option, default = "default_build_dir()")]
    /// the build directory to take node/miner/hub from
    build_dir: PathBuf,
    #[argh(option, default = "default_work_root().join(\"stack\")")]
    /// where to put chain data, stores and logs. Wiped on start
    work_dir: PathBuf,
    #[argh(switch)]
    /// start the hub trusting X-Forwarded-For from 127.0.0.1, which is
    /// what `load --distinct-sources` needs
    trust_local_proxy: bool,
}

fn default_build_dir() -> PathBuf {
    PathBuf::from("target/release")
}

fn default_work_root() -> PathBuf {
    std::env::temp_dir().join("itx-harness")
}

#[tokio::main]
async fn main() -> Result<()> {
    let args: Args = argh::from_env();
    match args.command {
        Command::Load(load_args) => run_load(load_args).await,
        Command::Drill(drill_args) => run_drills(drill_args).await,
        Command::Stack(stack_args) => run_stack(stack_args).await,
        Command::Compare(compare_args) => run_compare(compare_args),
    }
}

fn run_compare(args: CompareArgs) -> Result<()> {
    let read = |path: &PathBuf| -> Result<serde_json::Value> {
        let text = std::fs::read_to_string(path)
            .with_context(|| format!("reading {}", path.display()))?;
        Ok(serde_json::from_str(&text)?)
    };
    let (rendered, worse) = harness::report::compare(&read(&args.baseline)?, &read(&args.against)?);
    println!("{rendered}");
    if worse {
        std::process::exit(1);
    }
    Ok(())
}

async fn run_stack(args: StackArgs) -> Result<()> {
    let bin_dir = args.work_dir.join("bin");
    if args.work_dir.exists() {
        std::fs::remove_dir_all(&args.work_dir)?;
    }
    Stack::snapshot(&args.build_dir, &bin_dir)?;

    let mut config = StackConfig::new(&bin_dir, &args.work_dir);
    if args.trust_local_proxy {
        config = config.trusting_local_proxy();
    }
    let mut stack = Stack::prepare(config)?;
    stack.start_all().await?;

    println!("hub          {}", stack.hub_url());
    println!("node         {}", stack.node_address());
    println!("operator key {}", stack.work_dir().join("operator.priv.cbor").display());
    println!("logs         {}", stack.work_dir().join("logs").display());
    println!("\nrunning; ctrl-c to stop");

    // Children are `kill_on_drop`, so waiting here is what keeps them
    // alive -- returning from `main` would take the whole stack with it.
    tokio::signal::ctrl_c().await?;
    stack.shutdown().await;
    println!("stopped");
    Ok(())
}

async fn run_load(args: LoadArgs) -> Result<()> {
    let mut config = LoadConfig::new(args.hub);
    config.node_address = args.node;
    config.agents = args.agents;
    config.duration = Duration::from_secs(args.seconds);
    config.tick = Duration::from_millis(args.tick_ms);
    config.makers = args.makers;
    config.task_pool = args.task_pool;
    config.funding_key = args.funding_key;
    config.distinct_sources = args.distinct_sources;

    let report = load::run_report(
        &config,
        Report::new(
            format!("load: {} agents", config.agents),
            Environment::capture(&PathBuf::from(".")),
        ),
    )
    .await?;

    println!("{}", report.render());
    if let Some(path) = args.out {
        report.write_json(&path)?;
        println!("\nwrote {}", path.display());
    }
    Ok(())
}

async fn run_drills(args: DrillArgs) -> Result<()> {
    // Pin the binaries before anything runs. `target/release/hub` is one
    // path for every worktree on this machine and three concurrent
    // workstreams are changing hub source; a drill that read the path
    // fresh each time could measure two different hubs in one run.
    let bin_dir = args.work_root.join("bin");
    Stack::snapshot(&args.build_dir, &bin_dir)
        .context("pinning the binaries this run will measure")?;

    let names: Vec<String> = if args.name == "all" {
        drills::ALL.iter().map(|s| s.to_string()).collect()
    } else {
        vec![args.name.clone()]
    };

    let mut failed = false;
    for name in &names {
        println!("\n########## {name} ##########");
        let report = drills::run(name, &args.repo, &bin_dir, &args.work_root).await?;
        println!("{}", report.render());
        if let Some(dir) = &args.out {
            let path = dir.join(format!("{name}.json"));
            report.write_json(&path)?;
            println!("wrote {}", path.display());
        }
        failed |= report.needs_attention();
    }

    if failed {
        // A drill that refuted the plan or found a bug must not exit 0.
        // The point of a re-runnable harness is that someone can put it in
        // front of a change and be told, rather than have to read.
        std::process::exit(1);
    }
    Ok(())
}
