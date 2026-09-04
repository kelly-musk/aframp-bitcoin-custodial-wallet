//! Demonstrates deriving an address straight from `rust-bitcoin` primitives
//! (bip32 + secp256k1 + Address), with no BDK wallet/descriptor APIs involved, then
//! cross-checks it against BDK's own descriptor-based derivation at the same index.
//! See the README for why this path exists alongside BDK.

use anyhow::{ensure, Context, Result};
use bitcoin::bip32::{DerivationPath, Xpriv};
use bitcoin::key::CompressedPublicKey;
use bitcoin::secp256k1::Secp256k1;
use bitcoin::{Address, Network};

use crate::descriptors::DescriptorKind;
use crate::wallet::WalletCtx;
use bdk_wallet::KeychainKind;

pub fn derive_address_raw(
    root: &Xpriv,
    network: Network,
    kind: DescriptorKind,
    chain: u32,
    index: u32,
) -> Result<Address> {
    let secp = Secp256k1::new();
    let coin_type = if network == Network::Bitcoin { 0 } else { 1 };
    let path: DerivationPath = format!("m/{}'/{}'/0'/{}/{}", kind.purpose(), coin_type, chain, index)
        .parse()
        .context("building derivation path")?;

    let child = root.derive_priv(&secp, &path).context("deriving child key")?;

    match kind {
        DescriptorKind::Wpkh => {
            let compressed = CompressedPublicKey::from_private_key(&secp, &child.to_priv())
                .context("deriving compressed pubkey")?;
            Ok(Address::p2wpkh(&compressed, network))
        }
        DescriptorKind::Tr => {
            let keypair = child.to_keypair(&secp);
            let (xonly, _parity) = keypair.x_only_public_key();
            Ok(Address::p2tr(&secp, xonly, None, network))
        }
    }
}

/// Confirms BDK's descriptor-derived external address at index 0 matches the address derived
/// independently via raw rust-bitcoin above.
pub fn cross_check(ctx: &WalletCtx, root: &Xpriv, network: Network, kind: DescriptorKind) -> Result<()> {
    let bdk_addr = ctx.wallet.peek_address(KeychainKind::External, 0).address;
    let raw_addr = derive_address_raw(root, network, kind, 0, 0)?;
    ensure!(bdk_addr == raw_addr, "mismatch: bdk={bdk_addr} raw={raw_addr}");
    println!("cross-check OK — BDK and raw rust-bitcoin agree: {bdk_addr}");
    Ok(())
}
