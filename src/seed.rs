use std::path::Path;

use anyhow::{Context, Result};
use bdk_wallet::keys::bip39::{Language, Mnemonic};
use bitcoin::bip32::Xpriv;
use bitcoin::Network;

pub struct Seed {
    pub mnemonic: Mnemonic,
}

/// 12-word mnemonic, no BIP39 passphrase (documented simplification, see README).
pub fn generate() -> Result<Seed> {
    let mnemonic = Mnemonic::generate_in(Language::English, 12)
        .map_err(|e| anyhow::anyhow!("failed to generate mnemonic: {e}"))?;
    Ok(Seed { mnemonic })
}

pub fn import(phrase: &str) -> Result<Seed> {
    let mnemonic = Mnemonic::parse_in_normalized(Language::English, phrase.trim())
        .context("mnemonic phrase is not valid BIP39")?;
    Ok(Seed { mnemonic })
}

/// Writes the phrase in plaintext, restricted to owner read/write only.
pub fn save(seed: &Seed, path: &Path) -> Result<()> {
    std::fs::write(path, seed.mnemonic.to_string())
        .with_context(|| format!("writing seed to {path:?}"))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
            .with_context(|| format!("chmod 600 on {path:?}"))?;
    }
    Ok(())
}

pub fn load(path: &Path) -> Result<Seed> {
    let phrase = std::fs::read_to_string(path)
        .with_context(|| format!("no wallet found at {path:?} — run `init` first"))?;
    import(&phrase)
}

pub fn to_root_xpriv(seed: &Seed, network: Network) -> Result<Xpriv> {
    let bytes = seed.mnemonic.to_seed("");
    Xpriv::new_master(network, &bytes).context("deriving root xpriv from seed")
}
