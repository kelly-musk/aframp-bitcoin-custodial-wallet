mod config;
mod descriptors;
mod node;
mod prompt;
mod rawdemo;
mod seed;
mod send;
mod wallet;

use anyhow::{Context, Result};
use bitcoin::{Address, Amount, FeeRate, Network};
use clap::{Parser, Subcommand};

use config::Config;
use descriptors::DescriptorKind;

#[derive(Parser)]
#[command(name = "aframp-wallet", about = "A regtest/testnet Bitcoin custodial wallet")]
struct Cli {
    /// Which named wallet to operate on. Defaults to the one last used with `init`, or the only
    /// one that exists; required if you have more than one.
    #[arg(long, global = true)]
    wallet: Option<String>,

    #[command(subcommand)]
    cmd: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Generate (or import) keys and create a wallet, or load an existing one.
    ///
    /// Run with no flags for an interactive prompt (create new / load existing, name, network).
    /// Pass --name to run non-interactively instead, e.g. for scripting.
    Init {
        /// Name for the wallet to create or load. Omit this to get interactive prompts.
        #[arg(long)]
        name: Option<String>,
        #[arg(long, value_enum)]
        kind: Option<DescriptorKind>,
        /// Import an existing BIP39 mnemonic instead of generating a new one.
        #[arg(long)]
        import: Option<String>,
        /// Overwrite an existing wallet of this name (WARNING: orphans any existing funds).
        /// Refused if that wallet currently holds a nonzero balance unless
        /// --force-confirm-loss is also given.
        #[arg(long)]
        force: bool,
        /// Required alongside --force when the wallet being overwritten holds a nonzero balance.
        #[arg(long)]
        force_confirm_loss: bool,
        /// Network to create the wallet on. Defaults to NETWORK from .env.
        #[arg(long)]
        network: Option<String>,
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

    if let Command::Init { name, kind, import, force, force_confirm_loss, network } = cli.cmd {
        return cmd_init(&cfg, name, kind, import, force, force_confirm_loss, network);
    }

    let name = resolve_wallet_name(&cfg, cli.wallet.as_deref())?;

    match cli.cmd {
        Command::Init { .. } => unreachable!("handled above"),
        Command::Address { change } => cmd_address(&cfg, &name, change),
        Command::Balance => cmd_balance(&cfg, &name),
        Command::Sync => cmd_sync(&cfg, &name),
        Command::Send { to, amount, fee_rate, manual_select } => {
            cmd_send(&cfg, &name, &to, amount, fee_rate, manual_select)
        }
        Command::ListUtxos => cmd_list_utxos(&cfg, &name),
        Command::RawdemoCheck => cmd_rawdemo_check(&cfg, &name),
    }
}

/// Picks which wallet a non-`init` command should act on: an explicit `--wallet` flag wins,
/// otherwise the wallet last used with `init`, otherwise the only wallet that exists.
fn resolve_wallet_name(cfg: &Config, cli_wallet: Option<&str>) -> Result<String> {
    if let Some(w) = cli_wallet {
        return Ok(w.to_string());
    }
    if let Ok(active) = std::fs::read_to_string(cfg.active_marker_path()) {
        let active = active.trim();
        if !active.is_empty() {
            return Ok(active.to_string());
        }
    }
    let wallets = cfg.list_wallets()?;
    match wallets.len() {
        0 => anyhow::bail!("no wallet found; run `aframp-wallet init` first"),
        1 => Ok(wallets.into_iter().next().expect("len == 1")),
        _ => anyhow::bail!(
            "multiple wallets exist ({}); specify one with --wallet <name>",
            wallets.join(", ")
        ),
    }
}

fn set_active_wallet(cfg: &Config, name: &str) -> Result<()> {
    std::fs::write(cfg.active_marker_path(), name).context("recording active wallet")
}

fn load_identity(cfg: &Config, name: &str, network: Network) -> Result<(bitcoin::bip32::Xpriv, DescriptorKind)> {
    cfg.migrate_legacy_layout(name, network)?;
    let seed = seed::load(&cfg.seed_path(name))?;
    let kind_str = std::fs::read_to_string(cfg.kind_path(name, network))
        .context("reading descriptor kind — run `init` first")?;
    let kind = DescriptorKind::from_str(kind_str.trim())?;
    let root = seed::to_root_xpriv(&seed, network)?;
    Ok((root, kind))
}

fn open_wallet(
    cfg: &Config,
    name: &str,
    network: Network,
) -> Result<(wallet::WalletCtx, bitcoin::bip32::Xpriv, DescriptorKind)> {
    let (root, kind) = load_identity(cfg, name, network)?;
    let desc = descriptors::build(&root, network, kind)?;
    let ctx = wallet::open_or_create(&cfg.db_path(name, network), network, &desc)?;
    Ok((ctx, root, kind))
}

enum InitAction {
    Create { name: String, kind: DescriptorKind, import: Option<String> },
    Load { name: String },
}

fn cmd_init(
    cfg: &Config,
    name: Option<String>,
    kind: Option<DescriptorKind>,
    import: Option<String>,
    force: bool,
    force_confirm_loss: bool,
    network: Option<String>,
) -> Result<()> {
    let interactive = name.is_none();

    let action = match name {
        Some(name) => {
            config::validate_wallet_name(&name)?;
            InitAction::Create { name, kind: kind.unwrap_or(DescriptorKind::Wpkh), import }
        }
        None => interactive_init(cfg)?,
    };

    let network = match network {
        Some(s) => config::parse_network(&s)?,
        None if interactive => prompt_network(cfg.network)?,
        None => cfg.network,
    };

    match action {
        InitAction::Create { name, kind, import } => {
            cfg.migrate_legacy_layout(&name, network)?;

            let seed_exists = cfg.seed_path(&name).exists();
            let network_exists = cfg.kind_path(&name, network).exists() || cfg.db_path(&name, network).exists();

            if network_exists && !force {
                anyhow::bail!(
                    "wallet '{name}' already exists on {network:?}; use --force to overwrite \
                     (WARNING: this orphans any existing funds on {network:?} only), or pick a \
                     different --name"
                );
            }
            if seed_exists && import.is_some() {
                anyhow::bail!(
                    "wallet '{name}' already has a seed (shared across networks); --import only \
                     applies the first time this name is created"
                );
            }

            if force && network_exists {
                let old_balance = wallet::balance_of_existing_db(&cfg.db_path(&name, network))?;
                if old_balance.total() != Amount::ZERO && !force_confirm_loss {
                    anyhow::bail!(
                        "wallet '{name}' on {network:?} still holds a balance ({}); refusing to \
                         overwrite it. Move those funds first, or pass --force-confirm-loss if \
                         you're certain you want to abandon them.",
                        old_balance.total()
                    );
                }
                // Only this network's chain state/kind is reset — the seed is shared across
                // networks for this name, so it's never touched here (regenerating it would
                // silently break every other network already using this identity).
                for path in [cfg.db_path(&name, network), cfg.kind_path(&name, network)] {
                    if path.exists() {
                        std::fs::remove_file(&path).with_context(|| format!("removing old {path:?}"))?;
                    }
                }
            }
            config::ensure_data_dir(&cfg.network_dir(&name, network))?;

            // Reuse the existing identity if this name already has one (e.g. first time using it
            // on a new network); otherwise generate or import a fresh seed.
            let seed = if seed_exists {
                seed::load(&cfg.seed_path(&name))?
            } else {
                match import {
                    Some(phrase) => seed::import(&phrase)?,
                    None => seed::generate()?,
                }
            };
            seed::save(&seed, &cfg.seed_path(&name))?;
            std::fs::write(cfg.kind_path(&name, network), kind.as_str()).context("writing descriptor kind")?;

            let root = seed::to_root_xpriv(&seed, network)?;
            let desc = descriptors::build(&root, network, kind)?;
            let mut ctx = wallet::open_or_create(&cfg.db_path(&name, network), network, &desc)?;
            let (addr, _index) = wallet::new_address(&mut ctx, false)?;

            set_active_wallet(cfg, &name)?;
            println!(
                "wallet '{name}' {} (kind={}, network={:?})",
                if seed_exists { "extended to a new network" } else { "created" },
                kind.as_str(),
                network
            );
            println!("first receive address: {addr}");
        }
        InitAction::Load { name } => {
            cfg.migrate_legacy_layout(&name, network)?;
            let network_exists = cfg.kind_path(&name, network).exists() || cfg.db_path(&name, network).exists();
            if !network_exists {
                println!("'{name}' hasn't been used on {network:?} yet — setting it up there now (same seed).");
                let kind =
                    DescriptorKind::from_str(prompt::choice("Descriptor type", &["wpkh", "tr"], "wpkh")?)?;
                config::ensure_data_dir(&cfg.network_dir(&name, network))?;
                std::fs::write(cfg.kind_path(&name, network), kind.as_str()).context("writing descriptor kind")?;
            }

            let (mut ctx, _root, kind) = open_wallet(cfg, &name, network)?;
            let (addr, index) = wallet::new_address(&mut ctx, false)?;
            set_active_wallet(cfg, &name)?;
            println!("wallet '{name}' loaded (kind={}, network={:?})", kind.as_str(), network);
            println!("next receive address: {addr} (index {index})");
        }
    }
    Ok(())
}

fn interactive_init(cfg: &Config) -> Result<InitAction> {
    let existing = cfg.list_wallets()?;

    let create = if existing.is_empty() {
        println!("No wallets found yet.");
        true
    } else {
        println!("Existing wallets: {}", existing.join(", "));
        prompt::choice("Create a new wallet or load an existing one?", &["create", "load"], "create")?
            == "create"
    };

    if create {
        let name = loop {
            let candidate = prompt::line("Name for the new wallet", None)?;
            match config::validate_wallet_name(&candidate) {
                Ok(()) if existing.contains(&candidate) => {
                    println!("a wallet named '{candidate}' already exists")
                }
                Ok(()) => break candidate,
                Err(e) => println!("{e}"),
            }
        };
        let kind = DescriptorKind::from_str(prompt::choice("Descriptor type", &["wpkh", "tr"], "wpkh")?)?;
        let phrase = prompt::line(
            "Import an existing BIP39 phrase? (leave blank to generate a new one)",
            Some(""),
        )?;
        let import = if phrase.is_empty() { None } else { Some(phrase) };
        Ok(InitAction::Create { name, kind, import })
    } else {
        let name = prompt::pick("Which wallet do you want to load?", &existing)?;
        Ok(InitAction::Load { name })
    }
}

fn prompt_network(default: Network) -> Result<Network> {
    let default_str = match default {
        Network::Regtest => "regtest",
        Network::Testnet => "testnet",
        Network::Signet => "signet",
        _ => "regtest",
    };
    let chosen =
        prompt::choice("Which network do you want to operate on?", &["regtest", "testnet", "signet"], default_str)?;
    config::parse_network(chosen)
}

fn cmd_address(cfg: &Config, name: &str, change: bool) -> Result<()> {
    let (mut ctx, _root, _kind) = open_wallet(cfg, name, cfg.network)?;
    let (addr, index) = wallet::new_address(&mut ctx, change)?;
    println!("{addr} (index {index}, {})", if change { "internal/change" } else { "external/receive" });
    Ok(())
}

fn cmd_balance(cfg: &Config, name: &str) -> Result<()> {
    let (ctx, _root, _kind) = open_wallet(cfg, name, cfg.network)?;
    println!("{}", wallet::balance(&ctx));
    Ok(())
}

fn cmd_sync(cfg: &Config, name: &str) -> Result<()> {
    let (mut ctx, _root, _kind) = open_wallet(cfg, name, cfg.network)?;
    let client = node::rpc_client(&cfg.rpc_url, cfg.rpc_auth.clone())?;
    node::sync(&mut ctx, &client)?;
    println!("synced. balance: {}", wallet::balance(&ctx));
    Ok(())
}

fn cmd_send(
    cfg: &Config,
    name: &str,
    to: &str,
    amount_sats: u64,
    fee_rate_sat_vb: Option<u64>,
    manual_select: bool,
) -> Result<()> {
    let (mut ctx, _root, _kind) = open_wallet(cfg, name, cfg.network)?;
    let client = node::rpc_client(&cfg.rpc_url, cfg.rpc_auth.clone())?;

    // Always sync before spending to avoid building a transaction against stale UTXOs.
    node::sync(&mut ctx, &client)?;

    let addr: Address<_> = to.parse().context("invalid destination address")?;
    let addr = addr.require_network(cfg.network).context("address is for the wrong network")?;
    let amount = Amount::from_sat(amount_sats);
    let fee_rate = fee_rate_sat_vb
        .map(|r| FeeRate::from_sat_per_vb(r).context("invalid fee rate"))
        .transpose()?;

    if cfg.network == Network::Bitcoin {
        confirm_mainnet_send(&addr, amount, fee_rate)?;
    }

    let tx = send::build_and_sign(&mut ctx, addr, amount, fee_rate, manual_select)?;
    let txid = node::broadcast(&client, &tx)?;
    println!("broadcast: {txid}");
    Ok(())
}

/// This is real money and a broadcast can't be undone, so mainnet sends get one last typed
/// confirmation showing exactly what's about to happen — regtest/testnet skip this entirely so
/// the documented scripted demo flow keeps working unattended.
fn confirm_mainnet_send(to: &bitcoin::Address, amount: Amount, fee_rate: Option<FeeRate>) -> Result<()> {
    println!("You are about to broadcast a MAINNET transaction:");
    println!("  to:        {to}");
    println!("  amount:    {amount} ({} sat)", amount.to_sat());
    match fee_rate {
        Some(r) => println!("  fee rate:  {r}"),
        None => println!("  fee rate:  1 sat/vB (default — check this is reasonable for mainnet)"),
    }
    let answer = prompt::line("Type 'send' to broadcast, anything else to cancel", Some("cancel"))?;
    if answer != "send" {
        anyhow::bail!("cancelled");
    }
    Ok(())
}

fn cmd_list_utxos(cfg: &Config, name: &str) -> Result<()> {
    let (ctx, _root, _kind) = open_wallet(cfg, name, cfg.network)?;
    for utxo in wallet::list_utxos(&ctx) {
        println!(
            "{} {}  keychain={:?} spent={}",
            utxo.outpoint, utxo.txout.value, utxo.keychain, utxo.is_spent
        );
    }
    Ok(())
}

fn cmd_rawdemo_check(cfg: &Config, name: &str) -> Result<()> {
    let (ctx, root, kind) = open_wallet(cfg, name, cfg.network)?;
    rawdemo::cross_check(&ctx, &root, cfg.network, kind)
}
