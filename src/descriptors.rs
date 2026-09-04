use anyhow::Result;
use bitcoin::bip32::Xpriv;
use bitcoin::Network;

#[derive(Copy, Clone, Debug, PartialEq, Eq, clap::ValueEnum)]
pub enum DescriptorKind {
    Wpkh,
    Tr,
}

impl DescriptorKind {
    pub fn as_str(self) -> &'static str {
        match self {
            DescriptorKind::Wpkh => "wpkh",
            DescriptorKind::Tr => "tr",
        }
    }

    pub fn from_str(s: &str) -> Result<Self> {
        match s {
            "wpkh" => Ok(DescriptorKind::Wpkh),
            "tr" => Ok(DescriptorKind::Tr),
            other => anyhow::bail!("unknown descriptor kind '{other}' (expected wpkh or tr)"),
        }
    }

    /// BIP44-style purpose field: 84 (BIP84) for wpkh, 86 (BIP86) for taproot.
    pub fn purpose(self) -> u32 {
        match self {
            DescriptorKind::Wpkh => 84,
            DescriptorKind::Tr => 86,
        }
    }
}

pub struct DescriptorPair {
    pub external: String,
    pub internal: String,
}

/// Builds private (xprv-bearing) external/internal descriptor strings from a root key.
///
/// Path: m/{purpose}'/{coin_type}'/0'/{0=external,1=internal}/*, coin type 1' for any
/// non-mainnet network (regtest/testnet/signet), 0' for mainnet, per BIP44 convention.
pub fn build(root: &Xpriv, network: Network, kind: DescriptorKind) -> Result<DescriptorPair> {
    let coin_type = if network == Network::Bitcoin { 0 } else { 1 };
    let purpose = kind.purpose();
    let external = format!("{kind}({root}/{purpose}'/{coin_type}'/0'/0/*)", kind = kind.as_str());
    let internal = format!("{kind}({root}/{purpose}'/{coin_type}'/0'/1/*)", kind = kind.as_str());
    Ok(DescriptorPair { external, internal })
}
