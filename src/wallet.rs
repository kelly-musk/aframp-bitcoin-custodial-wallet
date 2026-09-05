use std::path::Path;

use anyhow::{Context, Result};
use bdk_wallet::chain::ChainPosition;
use bdk_wallet::rusqlite::Connection;
use bdk_wallet::{Balance, KeychainKind, LoadParams, LocalOutput, PersistedWallet, Wallet, WalletPersister};
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

/// Reads which network a wallet database was actually created under, without needing its seed,
/// descriptor kind, or an assumed network in advance — used so migrating a legacy flat-layout
/// wallet files it under the network it actually belongs to, not whatever network happens to be
/// configured in `.env` right now. Returns `None` if there's no database yet.
pub fn network_of_existing_db(db_path: &Path) -> Result<Option<Network>> {
    if !db_path.exists() {
        return Ok(None);
    }
    let mut conn = Connection::open(db_path).with_context(|| format!("opening {db_path:?}"))?;
    let changeset = Connection::initialize(&mut conn).context("reading existing wallet database")?;
    Ok(changeset.network)
}

/// Reads the balance of whatever wallet database already exists at `db_path`, without needing to
/// know its seed, descriptor kind, or network in advance — used to check "does this hold funds"
/// before `init --force` deletes it. Returns a zero balance if there's no database yet.
pub fn balance_of_existing_db(db_path: &Path) -> Result<Balance> {
    if !db_path.exists() {
        return Ok(Balance::default());
    }
    let mut conn = Connection::open(db_path).with_context(|| format!("opening {db_path:?}"))?;
    let changeset = Connection::initialize(&mut conn).context("reading existing wallet database")?;
    let wallet = Wallet::load_with_params(changeset, LoadParams::new())
        .context("reading existing wallet database")?;
    Ok(wallet.map(|w| w.balance()).unwrap_or_default())
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
