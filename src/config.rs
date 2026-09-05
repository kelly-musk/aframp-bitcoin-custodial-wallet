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

impl Config {
    pub fn from_env() -> Result<Self> {
        let network = match std::env::var("NETWORK")?.as_str() {
            "regtest" => Network::Regtest,
            "testnet" => Network::Testnet,
            "signet" => Network::Signet,
            "bitcoin" | "mainnet" => Network::Bitcoin,
            other => bail!("unknown NETWORK '{other}' (expected regtest, testnet, or signet)"),
        };
        // need to handle error here because PathBuf::from("") will return a valid path, but we want to error if the env var is not set
        let data_dir: PathBuf = std::env::var("DATA_DIR").unwrap_err().into();

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

    pub fn seed_path(&self) -> PathBuf {
        self.data_dir.join("seed.txt")
    }

    pub fn kind_path(&self) -> PathBuf {
        self.data_dir.join("kind.txt")
    }

    pub fn db_path(&self) -> PathBuf {
        self.data_dir.join("wallet.sqlite")
    }
}

pub fn ensure_data_dir(path: &Path) -> Result<()> {
    std::fs::create_dir_all(path).with_context(|| format!("creating data dir {path:?}"))
}
