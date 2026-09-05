use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use bitcoin::Network;
use bitcoincore_rpc::Auth;

pub struct Config {
    pub network: Network,
    pub data_dir: PathBuf,
    pub rpc_url: String,
    pub rpc_auth: Auth,
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

impl Config {
    pub fn from_env() -> Result<Self> {
        let network = parse_network(&std::env::var("NETWORK")?)?;
        // need to handle error here because PathBuf::from("") will return a valid path, but we want to error if the env var is not set
        let data_dir: PathBuf = std::env::var("DATA_DIR")?.into();

        let rpc_url = std::env::var("BITCOIN_RPC_URL")
            .context("BITCOIN_RPC_URL must be set in .env")?;

        let user = std::env::var("BITCOIN_RPC_USER").ok().filter(|s| !s.is_empty());
        let pass = std::env::var("BITCOIN_RPC_PASS").ok().filter(|s| !s.is_empty());
        let cookie = std::env::var("BITCOIN_RPC_COOKIE_FILE").ok().filter(|s| !s.is_empty());

        let rpc_auth = match (user, pass, cookie) {
            (Some(u), Some(p), _) => Auth::UserPass(u, p),
            (_, _, Some(cookie_path)) => Auth::CookieFile(PathBuf::from(cookie_path)),
            _ => bail!(
                "no RPC auth configured: set BITCOIN_RPC_USER + BITCOIN_RPC_PASS, or BITCOIN_RPC_COOKIE_FILE, in .env"
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

    pub fn kind_path(&self, name: &str) -> PathBuf {
        self.wallet_dir(name).join("kind.txt")
    }

    pub fn db_path(&self, name: &str) -> PathBuf {
        self.wallet_dir(name).join("wallet.sqlite")
    }

    /// Remembers which wallet name to default to when a command is run without `--wallet`.
    pub fn active_marker_path(&self) -> PathBuf {
        self.data_dir.join(".active")
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
