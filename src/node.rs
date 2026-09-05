use std::fmt;
use std::time::Duration;

use anyhow::{Context, Result};
use bdk_bitcoind_rpc::{Emitter, NO_EXPECTED_MEMPOOL_TXS};
use bitcoin::{Transaction, Txid};
use bitcoincore_rpc::jsonrpc::{self, client::Transport};
use bitcoincore_rpc::{Auth, Client, RpcApi};

use crate::config::RpcAuth;
use crate::wallet::{persist, WalletCtx};

const TIMEOUT: Duration = Duration::from_secs(30);

pub fn rpc_client(url: &str, auth: RpcAuth) -> Result<Client> {
    match auth {
        RpcAuth::UserPass(user, pass) => Client::new(url, Auth::UserPass(user, pass))
            .with_context(|| format!("connecting to Bitcoin Core RPC at {url}")),
        RpcAuth::CookieFile(path) => Client::new(url, Auth::CookieFile(path))
            .with_context(|| format!("connecting to Bitcoin Core RPC at {url}")),
        RpcAuth::ApiKey(key) => {
            // bitcoincore_rpc's built-in transport only speaks HTTP Basic Auth, but a hosted
            // proxy like BitRPC gates access with an `X-API-Key` header instead — so this
            // bypasses Client::new() and hands it a jsonrpc client built on our own transport.
            let transport = ApiKeyTransport { url: url.to_string(), api_key: key, timeout: TIMEOUT };
            Ok(Client::from_jsonrpc(jsonrpc::client::Client::with_transport(transport)))
        }
    }
}

struct ApiKeyTransport {
    url: String,
    api_key: String,
    timeout: Duration,
}

impl ApiKeyTransport {
    fn request<Req, Resp>(&self, body: Req) -> Result<Resp, jsonrpc::Error>
    where
        Req: serde::Serialize,
        Resp: for<'a> serde::de::Deserialize<'a>,
    {
        let resp = minreq::Request::new(minreq::Method::Post, &self.url)
            .with_timeout(self.timeout.as_secs())
            .with_header("X-API-Key", &self.api_key)
            .with_json(&body)
            .map_err(|e| jsonrpc::Error::Transport(Box::new(e)))?
            .send()
            .map_err(|e| jsonrpc::Error::Transport(Box::new(e)))?;

        if resp.status_code != 200 {
            return Err(jsonrpc::Error::Transport(Box::new(HttpStatusError {
                status_code: resp.status_code,
                body: resp.as_str().unwrap_or("<non-utf8 body>").to_string(),
            })));
        }
        resp.json().map_err(|e| jsonrpc::Error::Transport(Box::new(e)))
    }
}

impl Transport for ApiKeyTransport {
    fn send_request(&self, req: jsonrpc::Request) -> Result<jsonrpc::Response, jsonrpc::Error> {
        self.request(&req)
    }

    fn send_batch(&self, reqs: &[jsonrpc::Request]) -> Result<Vec<jsonrpc::Response>, jsonrpc::Error> {
        self.request(reqs)
    }

    fn fmt_target(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "{}", self.url)
    }
}

#[derive(Debug)]
struct HttpStatusError {
    status_code: i32,
    body: String,
}

impl fmt::Display for HttpStatusError {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        // BitRPC (and similar proxies) return e.g. 401 (missing key), 403 (invalid key or a
        // method outside its allowlist), 429 (rate limited), 502 (upstream node unreachable).
        write!(f, "HTTP {}: {}", self.status_code, self.body)
    }
}

impl std::error::Error for HttpStatusError {}

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

    // Some RPC endpoints (e.g. BitRPC's proxy allowlist) don't permit getrawmempool, which this
    // needs. Rather than fail the whole sync over it, skip mempool tracking for this run — block
    // sync (and therefore balance/UTXOs once a tx confirms) is unaffected either way.
    match emitter.mempool() {
        Ok(mempool_event) => ctx.wallet.apply_unconfirmed_txs(mempool_event.update),
        Err(e) => log::warn!("skipping mempool sync (unconfirmed txs won't be tracked this run): {e}"),
    }

    persist(ctx)?;
    Ok(())
}

pub fn broadcast(client: &Client, tx: &Transaction) -> Result<Txid> {
    client.send_raw_transaction(tx).context("broadcasting transaction via node RPC")
}
