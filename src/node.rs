use anyhow::{Context, Result};
use bdk_bitcoind_rpc::{Emitter, NO_EXPECTED_MEMPOOL_TXS};
use bitcoin::{Transaction, Txid};
use bitcoincore_rpc::{Auth, Client, RpcApi};

use crate::wallet::{persist, WalletCtx};

pub fn rpc_client(url: &str, auth: Auth) -> Result<Client> {
    Client::new(url, auth).with_context(|| format!("connecting to Bitcoin Core RPC at {url}"))
}

/// Syncs chain state (blocks + mempool) from the node into the wallet, via
/// `bdk_bitcoind_rpc::Emitter`. Works against a wallet-disabled Core node since it only uses
/// block/mempool RPCs, never the node's own wallet API.
pub fn sync(ctx: &mut WalletCtx, client: &Client) -> Result<()> {
    let start_cp = ctx.wallet.latest_checkpoint();
    let start_height = start_cp.height();
    log::info!("starting sync from height {start_height}");

    let mut emitter = Emitter::new(client, start_cp, start_height, NO_EXPECTED_MEMPOOL_TXS);

    let mut blocks_applied = 0u32;
    while let Some(ev) = emitter.next_block().context("fetching next block from node")? {
        ctx.wallet
            .apply_block(&ev.block, ev.block_height())
            .context("applying block to wallet")?;
        blocks_applied += 1;
    }
    log::info!("applied {blocks_applied} new block(s)");

    let mempool_event = emitter.mempool().context("fetching mempool from node")?;
    ctx.wallet.apply_unconfirmed_txs(mempool_event.update);

    persist(ctx)?;
    Ok(())
}

pub fn broadcast(client: &Client, tx: &Transaction) -> Result<Txid> {
    client.send_raw_transaction(tx).context("broadcasting transaction via node RPC")
}
