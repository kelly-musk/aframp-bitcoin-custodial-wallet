use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use bitcoin::Network;

/// How to authenticate to the configured `BITCOIN_RPC_URL`. Plain Bitcoin Core (or most
/// self-hosted nodes) uses `UserPass`/`CookieFile`; a hosted RPC proxy that gates access with a
/// header — e.g. BitRPC's `X-API-Key` — needs `ApiKey` instead, which bypasses
/// `bitcoincore_rpc`'s built-in transport (Basic Auth only) via a custom `jsonrpc` transport.
/// See `node::rpc_client`.
#[derive(Clone)]
pub enum RpcAuth {
    UserPass(String, String),
    CookieFile(PathBuf),
    ApiKey(String),
}

pub struct Config {
    pub network: Network,
    pub data_dir: PathBuf,
    pub rpc_url: String,
    pub rpc_auth: RpcAuth,
}

/// Parses a network name from `.env`, a CLI flag, or an interactive prompt.
pub fn parse_network(s: &str) -> Result<Network> {
    Ok(match s {
        "regtest" => Network::Regtest,
        "testnet" => Network::Testnet,
        "signet" => Network::Signet,
        "bitcoin" | "mainnet" => Network::Bitcoin,
        other => bail!("unknown network '{other}' (expected regtest, testnet, or signet)"),
    })
}

fn network_dir_name(network: Network) -> &'static str {
    match network {
        Network::Bitcoin => "bitcoin",
        Network::Testnet => "testnet",
        Network::Signet => "signet",
        Network::Regtest => "regtest",
        _ => "unknown",
    }
}

impl Config {
    pub fn from_env() -> Result<Self> {
        let network = parse_network(&std::env::var("NETWORK")?)?;
        
        let data_dir: PathBuf = std::env::var("DATA_DIR").context("DATA_DIR must be set in .env")?.into();

        let rpc_url = std::env::var("BITCOIN_RPC_URL")
            .context("BITCOIN_RPC_URL must be set in .env")?;

        let user = std::env::var("BITCOIN_RPC_USER").ok().filter(|s| !s.is_empty());
        let pass = std::env::var("BITCOIN_RPC_PASS").ok().filter(|s| !s.is_empty());
        let cookie = std::env::var("BITCOIN_RPC_COOKIE_FILE").ok().filter(|s| !s.is_empty());
        let api_key = std::env::var("BITCOIN_RPC_API_KEY").ok().filter(|s| !s.is_empty());

        let rpc_auth = match (user, pass, cookie, api_key) {
            (Some(u), Some(p), _, _) => RpcAuth::UserPass(u, p),
            (_, _, Some(cookie_path), _) => RpcAuth::CookieFile(PathBuf::from(cookie_path)),
            (_, _, _, Some(key)) => RpcAuth::ApiKey(key),
            _ => bail!(
                "no RPC auth configured: set BITCOIN_RPC_USER + BITCOIN_RPC_PASS, \
                 BITCOIN_RPC_COOKIE_FILE, or BITCOIN_RPC_API_KEY, in .env"
            ),
        };

        Ok(Self { network, data_dir, rpc_url, rpc_auth })
    }

    pub fn wallet_dir(&self, name: &str) -> PathBuf {
        self.data_dir.join(name)
    }

    pub fn seed_path(&self, name: &str) -> PathBuf {
        self.wallet_dir(name).join("seed.txt")
    }

    /// Chain state (kind + database) lives one level below the wallet's identity, scoped by
    /// network — mirrors Bitcoin Core's own `regtest/`/`testnet3/`/`signet/` subdirectories under
    /// one datadir. The seed is the only thing shared across networks for a given wallet name;
    /// UTXOs/checkpoints never can be, and the derivation path itself differs (coin type 0' vs
    /// 1'), so a wallet is really a distinct set of addresses per network even from one seed.
    pub fn network_dir(&self, name: &str, network: Network) -> PathBuf {
        self.wallet_dir(name).join(network_dir_name(network))
    }

    pub fn kind_path(&self, name: &str, network: Network) -> PathBuf {
        self.network_dir(name, network).join("kind.txt")
    }

    pub fn db_path(&self, name: &str, network: Network) -> PathBuf {
        self.network_dir(name, network).join("wallet.sqlite")
    }

    /// Remembers which wallet name to default to when a command is run without `--wallet`.
    pub fn active_marker_path(&self) -> PathBuf {
        self.data_dir.join(".active")
    }

    /// One-time migration from the pre-network-subdirectory layout (`data/<name>/kind.txt` and
    /// `wallet.sqlite` directly), so nothing already created — mainnet funds included — gets
    /// orphaned by moving to `data/<name>/<network>/...`. No-ops once already migrated, and never
    /// overwrites an already-existing new-layout file.
    ///
    /// `requested_network` is only a fallback: the legacy database itself records which network
    /// it was actually created under (in its `ChangeSet`), and that's what decides the target
    /// subdirectory. A flat-layout wallet could have been created under any network regardless of
    /// what `.env` currently says, so trusting the caller's network here would risk filing it
    /// under the wrong one — e.g. a regtest wallet getting migrated into `bitcoin/` just because
    /// `NETWORK` happened to be set to mainnet at the moment `migrate_legacy_layout` ran.
    pub fn migrate_legacy_layout(&self, name: &str, requested_network: Network) -> Result<()> {
        let old_kind = self.wallet_dir(name).join("kind.txt");
        let old_db = self.wallet_dir(name).join("wallet.sqlite");
        if !old_kind.exists() && !old_db.exists() {
            return Ok(());
        }

        let network = crate::wallet::network_of_existing_db(&old_db)?.unwrap_or(requested_network);

        let new_kind = self.kind_path(name, network);
        let new_db = self.db_path(name, network);
        if new_kind.exists() || new_db.exists() {
            return Ok(());
        }

        ensure_data_dir(&self.network_dir(name, network))?;
        for (old, new) in [(old_kind, new_kind), (old_db.clone(), new_db)] {
            if old.exists() {
                std::fs::rename(&old, &new).with_context(|| format!("migrating {old:?} to {new:?}"))?;
            }
        }
        // SQLite may leave WAL/shared-memory sidecar files alongside the main database file.
        for suffix in ["-wal", "-shm", "-journal"] {
            let old_side = self.wallet_dir(name).join(format!("wallet.sqlite{suffix}"));
            if old_side.exists() {
                let new_side = self.network_dir(name, network).join(format!("wallet.sqlite{suffix}"));
                std::fs::rename(&old_side, &new_side)
                    .with_context(|| format!("migrating {old_side:?} to {new_side:?}"))?;
            }
        }
        log::info!("migrated wallet '{name}' to data/{name}/{network:?}/ (its actual network)");
        Ok(())
    }

    /// Names of every wallet that has been initialized under `data_dir` (i.e. has a seed file).
    pub fn list_wallets(&self) -> Result<Vec<String>> {
        let mut names = Vec::new();
        let entries = match std::fs::read_dir(&self.data_dir) {
            Ok(entries) => entries,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(names),
            Err(e) => return Err(e).context("listing wallets"),
        };
        for entry in entries {
            let entry = entry.context("listing wallets")?;
            let name = entry.file_name().to_string_lossy().into_owned();
            if entry.path().is_dir() && entry.path().join("seed.txt").exists() {
                names.push(name);
            }
        }
        names.sort();
        Ok(names)
    }
}

/// A wallet name becomes a directory name, so keep it to a safe, unambiguous charset.
pub fn validate_wallet_name(name: &str) -> Result<()> {
    let ok = !name.is_empty()
        && name != "."
        && name != ".."
        && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_');
    if ok {
        Ok(())
    } else {
        bail!("wallet name must be non-empty and contain only letters, digits, '-', or '_'")
    }
}

pub fn ensure_data_dir(path: &Path) -> Result<()> {
    std::fs::create_dir_all(path).with_context(|| format!("creating data dir {path:?}"))
}
