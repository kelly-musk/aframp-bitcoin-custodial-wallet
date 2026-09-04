use std::path::Path;

use anyhow::{Context, Result};
use bdk_wallet::chain::ChainPosition;
use bdk_wallet::rusqlite::Connection;
use bdk_wallet::{Balance, KeychainKind, LocalOutput, PersistedWallet, Wallet};
use bitcoin::constants::COINBASE_MATURITY;
use bitcoin::{Address, Network};

use crate::descriptors::DescriptorPair;

pub struct WalletCtx {
    pub wallet: PersistedWallet<Connection>,
    pub conn: Connection,
}

/// Loads the wallet from `db_path` if it already has persisted data, otherwise creates it.
/// The private descriptor strings are re-derived from the local seed on every invocation (cheap,
/// deterministic); only chain state (UTXOs/checkpoints/txs) actually needs to survive in SQLite.
pub fn open_or_create(db_path: &Path, network: Network, desc: &DescriptorPair) -> Result<WalletCtx> {
    let mut conn = Connection::open(db_path)
        .with_context(|| format!("opening wallet database at {db_path:?}"))?;

    let loaded = Wallet::load()
        .descriptor(KeychainKind::External, Some(desc.external.clone()))
        .descriptor(KeychainKind::Internal, Some(desc.internal.clone()))
        .extract_keys()
        .check_network(network)
        .load_wallet(&mut conn)
        .context("loading existing wallet from database")?;

    let wallet = match loaded {
        Some(w) => w,
        None => Wallet::create(desc.external.clone(), desc.internal.clone())
            .network(network)
            .create_wallet(&mut conn)
            .context("creating new wallet in database")?,
    };

    Ok(WalletCtx { wallet, conn })
}

pub fn persist(ctx: &mut WalletCtx) -> Result<()> {
    ctx.wallet.persist(&mut ctx.conn).context("persisting wallet state to database")?;
    Ok(())
}

pub fn new_address(ctx: &mut WalletCtx, change: bool) -> Result<(Address, u32)> {
    let kind = if change { KeychainKind::Internal } else { KeychainKind::External };
    let info = ctx.wallet.reveal_next_address(kind);
    let result = (info.address.clone(), info.index);
    persist(ctx)?;
    Ok(result)
}

pub fn balance(ctx: &WalletCtx) -> Balance {
    ctx.wallet.balance()
}

pub fn list_utxos(ctx: &WalletCtx) -> Vec<LocalOutput> {
    ctx.wallet.list_unspent().collect()
}

/// `list_unspent()` includes immature coinbase outputs (Core will reject a tx spending one with
/// `bad-txns-premature-spend-of-coinbase`); BDK's own default coin selection filters these out
/// internally, but that filtering isn't exposed, so manual selection needs its own check.
pub fn is_spendable_now(ctx: &WalletCtx, utxo: &LocalOutput) -> bool {
    let Some(tx) = ctx.wallet.get_tx(utxo.outpoint.txid) else {
        return false;
    };
    if !tx.tx_node.tx.is_coinbase() {
        return true;
    }
    let ChainPosition::Confirmed { anchor, .. } = utxo.chain_position else {
        return false;
    };
    let tip_height = ctx.wallet.latest_checkpoint().height();
    tip_height.saturating_sub(anchor.block_id.height) + 1 >= COINBASE_MATURITY
}
