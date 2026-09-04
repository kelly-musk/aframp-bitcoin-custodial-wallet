mod config;
mod descriptors;
mod node;
mod rawdemo;
mod seed;
mod send;
mod wallet;

use anyhow::{Context, Result};
use bitcoin::{Address, Amount, FeeRate};
use clap::{Parser, Subcommand};

use config::Config;
use descriptors::DescriptorKind;

#[derive(Parser)]
#[command(name = "aframp-wallet", about = "A regtest/testnet Bitcoin custodial wallet")]
struct Cli {
    #[command(subcommand)]
    cmd: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Generate (or import) keys and create the wallet.
    Init {
        #[arg(long, value_enum, default_value = "wpkh")]
        kind: DescriptorKind,
        /// Import an existing BIP39 mnemonic instead of generating a new one.
        #[arg(long)]
        import: Option<String>,
        /// Overwrite an existing wallet at this data dir (WARNING: orphans any existing funds).
        #[arg(long)]
        force: bool,
    },
    /// Print a receive (or change) address.
    Address {
        #[arg(long)]
        change: bool,
    },
    /// Print the wallet's current balance.
    Balance,
    /// Sync chain state from the configured Bitcoin node.
    Sync,
    /// Build, sign, and broadcast a transaction.
    Send {
        #[arg(long)]
        to: String,
        /// Amount in satoshis.
        #[arg(long)]
        amount: u64,
        /// Fee rate in sat/vB (default: 1).
        #[arg(long)]
        fee_rate: Option<u64>,
        /// Use explicit largest-first coin selection instead of BDK's default.
        #[arg(long)]
        manual_select: bool,
    },
    /// List current UTXOs.
    ListUtxos,
    /// Cross-check a raw-rust-bitcoin-derived address against BDK's descriptor-derived one.
    RawdemoCheck,
}

fn main() {
    if let Err(e) = run() {
        eprintln!("Error: {e:#}");
        std::process::exit(1);
    }
}

fn run() -> Result<()> {
    dotenvy::dotenv().ok();
    env_logger::init();

    let cli = Cli::parse();
    let cfg = Config::from_env()?;
    config::ensure_data_dir(&cfg.data_dir)?;

    match cli.cmd {
        Command::Init { kind, import, force } => cmd_init(&cfg, kind, import, force),
        Command::Address { change } => cmd_address(&cfg, change),
        Command::Balance => cmd_balance(&cfg),
        Command::Sync => cmd_sync(&cfg),
        Command::Send { to, amount, fee_rate, manual_select } => {
            cmd_send(&cfg, &to, amount, fee_rate, manual_select)
        }
        Command::ListUtxos => cmd_list_utxos(&cfg),
        Command::RawdemoCheck => cmd_rawdemo_check(&cfg),
    }
}

fn load_identity(cfg: &Config) -> Result<(bitcoin::bip32::Xpriv, DescriptorKind)> {
    let seed = seed::load(&cfg.seed_path())?;
    let kind_str = std::fs::read_to_string(cfg.kind_path())
        .context("reading descriptor kind — run `init` first")?;
    let kind = DescriptorKind::from_str(kind_str.trim())?;
    let root = seed::to_root_xpriv(&seed, cfg.network)?;
    Ok((root, kind))
}

fn open_wallet(cfg: &Config) -> Result<(wallet::WalletCtx, bitcoin::bip32::Xpriv, DescriptorKind)> {
    let (root, kind) = load_identity(cfg)?;
    let desc = descriptors::build(&root, cfg.network, kind)?;
    let ctx = wallet::open_or_create(&cfg.db_path(), cfg.network, &desc)?;
    Ok((ctx, root, kind))
}

fn cmd_init(cfg: &Config, kind: DescriptorKind, import: Option<String>, force: bool) -> Result<()> {
    if cfg.seed_path().exists() && !force {
        anyhow::bail!(
            "wallet already initialized at {:?}; use --force to overwrite (WARNING: this orphans any existing funds)",
            cfg.data_dir
        );
    }

    let seed = match import {
        Some(phrase) => seed::import(&phrase)?,
        None => seed::generate()?,
    };
    seed::save(&seed, &cfg.seed_path())?;
    std::fs::write(cfg.kind_path(), kind.as_str()).context("writing descriptor kind")?;

    let root = seed::to_root_xpriv(&seed, cfg.network)?;
    let desc = descriptors::build(&root, cfg.network, kind)?;
    let mut ctx = wallet::open_or_create(&cfg.db_path(), cfg.network, &desc)?;
    let (addr, _index) = wallet::new_address(&mut ctx, false)?;

    println!("wallet initialized (kind={}, network={:?})", kind.as_str(), cfg.network);
    println!("first receive address: {addr}");
    Ok(())
}

fn cmd_address(cfg: &Config, change: bool) -> Result<()> {
    let (mut ctx, _root, _kind) = open_wallet(cfg)?;
    let (addr, index) = wallet::new_address(&mut ctx, change)?;
    println!("{addr} (index {index}, {})", if change { "internal/change" } else { "external/receive" });
    Ok(())
}

fn cmd_balance(cfg: &Config) -> Result<()> {
    let (ctx, _root, _kind) = open_wallet(cfg)?;
    println!("{}", wallet::balance(&ctx));
    Ok(())
}

fn cmd_sync(cfg: &Config) -> Result<()> {
    let (mut ctx, _root, _kind) = open_wallet(cfg)?;
    let client = node::rpc_client(&cfg.rpc_url, clone_auth(&cfg.rpc_auth))?;
    node::sync(&mut ctx, &client)?;
    println!("synced. balance: {}", wallet::balance(&ctx));
    Ok(())
}

fn cmd_send(
    cfg: &Config,
    to: &str,
    amount_sats: u64,
    fee_rate_sat_vb: Option<u64>,
    manual_select: bool,
) -> Result<()> {
    let (mut ctx, _root, _kind) = open_wallet(cfg)?;
    let client = node::rpc_client(&cfg.rpc_url, clone_auth(&cfg.rpc_auth))?;

    // Always sync before spending to avoid building a transaction against stale UTXOs.
    node::sync(&mut ctx, &client)?;

    let addr: Address<_> = to.parse().context("invalid destination address")?;
    let addr = addr.require_network(cfg.network).context("address is for the wrong network")?;
    let amount = Amount::from_sat(amount_sats);
    let fee_rate = fee_rate_sat_vb
        .map(|r| FeeRate::from_sat_per_vb(r).context("invalid fee rate"))
        .transpose()?;

    let tx = send::build_and_sign(&mut ctx, addr, amount, fee_rate, manual_select)?;
    let txid = node::broadcast(&client, &tx)?;
    println!("broadcast: {txid}");
    Ok(())
}

fn cmd_list_utxos(cfg: &Config) -> Result<()> {
    let (ctx, _root, _kind) = open_wallet(cfg)?;
    for utxo in wallet::list_utxos(&ctx) {
        println!(
            "{} {}  keychain={:?} spent={}",
            utxo.outpoint, utxo.txout.value, utxo.keychain, utxo.is_spent
        );
    }
    Ok(())
}

fn cmd_rawdemo_check(cfg: &Config) -> Result<()> {
    let (ctx, root, kind) = open_wallet(cfg)?;
    rawdemo::cross_check(&ctx, &root, cfg.network, kind)
}

fn clone_auth(auth: &bitcoincore_rpc::Auth) -> bitcoincore_rpc::Auth {
    use bitcoincore_rpc::Auth;
    match auth {
        Auth::None => Auth::None,
        Auth::UserPass(u, p) => Auth::UserPass(u.clone(), p.clone()),
        Auth::CookieFile(p) => Auth::CookieFile(p.clone()),
    }
}
