# aframp-bitcoin-custodial-wallet

A regtest/testnet Bitcoin custodial wallet in Rust, built on `bitcoin`, `bdk_wallet`, `bdk_bitcoind_rpc`, and `bitcoincore-rpc`.

## What it does

- Generates (or imports) a BIP39 seed and derives a descriptor-based wallet (BIP84 `wpkh` by default, BIP86 `tr`/Taproot as an option).
- Separate external (receive) and internal (change) keychains.
- Tracks UTXOs and balance by syncing block + mempool data from a Bitcoin Core-compatible node.
- Persists all chain state (UTXOs, checkpoints, transactions) to SQLite, so the wallet survives being closed and reopened without a rescan from genesis.
- Builds, signs, and broadcasts transactions, with an optional explicit largest-first coin-selection mode.

Built and tested against regtest/testnet, per the assignment this was written for — see the [Proof](#proof-of-a-working-transaction-regtest-self-test) section below. `NETWORK=mainnet` is accepted by `Config::from_env` (see [`src/config.rs`](src/config.rs)), but this is still a small, unaudited toy wallet, not production custody software — read [Using this with real funds](#using-this-with-real-mainnet-funds) before pointing it at real money.

## Setup

0. **Install the CLI** (optional — `cargo run --` below works without this):
   ```
   cargo install --path .
   ```
   This installs `aframp-wallet` to `~/.cargo/bin`. All commands below can be run as either `cargo run -- <cmd>` from the repo, or `aframp-wallet <cmd>` from anywhere once installed (it reads `.env` from the current directory either way).

1. **A Bitcoin Core-compatible node** — either:
   - a local `bitcoind -regtest` for development/self-testing (no faucet, no network delay — instant `generatetoaddress`), or
   - a testnet node you have RPC access to (mainnet is rejected by the wallet).

   The node does **not** need its own wallet enabled — this project talks to it purely through block/mempool RPCs (`getblock`, `getrawmempool`, `getrawtransaction`, `sendrawtransaction`), via `bdk_bitcoind_rpc::Emitter`, so it works against a wallet-disabled node.

2. **Configure**: `cp .env.example .env` and fill in:
   ```
   NETWORK=regtest            # or testnet / signet
   DATA_DIR=./data
   BITCOIN_RPC_URL=http://127.0.0.1:18443
   BITCOIN_RPC_USER=...       # or BITCOIN_RPC_COOKIE_FILE=~/.bitcoin/regtest/.cookie
   BITCOIN_RPC_PASS=...
   ```

3. **Initialize a wallet** — the CLI supports multiple named wallets side by side under `DATA_DIR`:
   ```
   aframp-wallet init                              # interactive: create new or load existing, pick a name, kind, network
   aframp-wallet init --name alice                 # non-interactive, BIP84 wpkh, generates a fresh seed
   aframp-wallet init --name alice --kind tr        # BIP86 taproot instead
   aframp-wallet init --name alice --import "<12-word phrase>"
   aframp-wallet init --name alice --network testnet
   ```
   Running `init` with no `--name` drops into prompts (create vs. load, name, descriptor kind, network) — see the recorded session below. Passing `--name` skips all prompts, for scripting.

   Each named wallet gets its own `data/<name>/seed.txt` (mode `0600`, plaintext BIP39 phrase — **never commit this**) and `data/<name>/wallet.sqlite`. Whichever wallet you last created or loaded becomes the default for every other command (recorded in `data/.active`) — pass `--wallet <name>` to override it, which you'll need once more than one wallet exists.

4. **Use it**:
   ```
   aframp-wallet address [--change]
   aframp-wallet sync
   aframp-wallet balance
   aframp-wallet list-utxos
   aframp-wallet send --to <addr> --amount <sats> [--fee-rate <sat/vB>] [--manual-select]
   aframp-wallet rawdemo-check
   ```
   Add `--wallet <name>` to any of these to target a specific wallet instead of the default. `send` always runs a `sync` first, so it never builds against stale UTXOs. (`cargo run --` works identically in place of `aframp-wallet` if you skipped step 0.)

   Example interactive `init` session (creating a wallet named `alice`, then later reloading it):
   ```
   $ aframp-wallet init
   No wallets found yet.
   Name for the new wallet: alice
   Descriptor type (wpkh/tr) [wpkh]:
   Import an existing BIP39 phrase? (leave blank to generate a new one) []:
   Which network do you want to operate on? (regtest/testnet/signet) [regtest]:
   wallet 'alice' created (kind=wpkh, network=Regtest)
   first receive address: bcrt1q...

   $ aframp-wallet init
   Existing wallets: alice
   Create a new wallet or load an existing one? (create/load) [create]: load
     1) alice
   Which wallet do you want to load?: 1
   Which network do you want to operate on? (regtest/testnet/signet) [regtest]:
   wallet 'alice' loaded (kind=wpkh, network=Regtest)
   next receive address: bcrt1q...

   $ aframp-wallet balance          # no --wallet needed — alice is still the active one
   ```

## Project / descriptor structure

Each named wallet lives in its own directory, `DATA_DIR/<name>/`:
```
data/
  alice/   seed.txt  kind.txt  wallet.sqlite
  bob/     seed.txt  kind.txt  wallet.sqlite
  .active  # name of the wallet other commands default to when --wallet is omitted
```

Only **chain state** goes in SQLite (`bdk_wallet`'s `ChangeSet` — UTXOs, checkpoints, transactions). The private descriptor strings themselves are **not** stored; they're re-derived on every CLI invocation from that wallet's BIP39 seed (cheap and deterministic — `seed → Xpriv::new_master → "wpkh(tprv.../84'/1'/0'/{0,1}/*)"`, see [`src/seed.rs`](src/seed.rs) and [`src/descriptors.rs`](src/descriptors.rs)). This keeps the on-disk database free of key material beyond what `seed.txt` already holds, and means `Wallet::load()` is given the descriptor with `.extract_keys()` on every run to re-attach the signer.

A wallet's network is fixed at creation (stored inside its own `ChangeSet`, not a separate file) — `Wallet::load()`'s `.check_network(...)` call cleanly rejects opening it under a different network than it was created for, which is what you'll see if `.env`'s `NETWORK` doesn't match a given wallet.

Derivation follows BIP44-style paths: `m/{84 or 86}'/{0 or 1}'/0'/{0=external,1=internal}/*` — coin type `0'` for mainnet (unreachable in this wallet), `1'` for regtest/testnet/signet, purpose `84'` for `wpkh` or `86'` for `tr`.

## Which library was used where, and why

| Library | Used for | Why |
|---|---|---|
| `bdk_wallet` | Descriptor parsing, address derivation, UTXO/balance tracking, SQLite persistence (`ChangeSet`), PSBT building (`TxBuilder`), signing | This is the actual "wallet" — indexing, coin tracking, and transaction construction are exactly what BDK is for, and reimplementing UTXO-set bookkeeping and coin selection by hand would just be a worse copy of it. |
| `bdk_bitcoind_rpc` | Bridging node chain state into the wallet ([`src/node.rs`](src/node.rs), `Emitter::next_block` / `Emitter::mempool`) | Purpose-built glue between `bitcoincore-rpc` and `bdk_wallet` — walks the node's blocks/mempool and hands them to `Wallet::apply_block` / `apply_unconfirmed_txs` without needing the node's own wallet. |
| `bitcoincore-rpc` | The actual RPC client/auth (`Client`, `Auth`), and `send_raw_transaction` for broadcast | Talking to Bitcoin Core's RPC interface directly for connectivity and broadcast is exactly its job; `bdk_bitcoind_rpc::Emitter` is generic over anything implementing `RpcApi`, and this is the concrete implementation of that. |
| `rust-bitcoin` (raw, no BDK) | [`src/rawdemo.rs`](src/rawdemo.rs) — deriving an address straight from `bip32::Xpriv` + `secp256k1` + `Address::p2wpkh`/`p2tr`, no descriptor or wallet APIs | See "Where I reached for raw `rust-bitcoin`" below. |
| `clap` | CLI argument parsing | Standard, declarative subcommands (`init`, `address`, `sync`, `send`, ...) instead of hand-rolled `std::env::args()` parsing. |
| `dotenvy` | Loading `.env` at startup | Keeps RPC credentials and network choice out of source, per the assignment's constraint. |
| `anyhow` | Error propagation/context throughout | Every fallible path returns `anyhow::Result` with `.context(...)`, so failures surface as one readable message instead of a panic — see `main()`'s `Err(e) => eprintln!("Error: {e:#}")`. |

## Where I reached for raw `rust-bitcoin` instead of BDK

[`src/rawdemo.rs`](src/rawdemo.rs) derives a receive address at `m/{84,86}'/coin'/0'/0/0` **without touching any BDK descriptor or wallet API** — just `bitcoin::bip32::Xpriv::derive_priv`, `secp256k1::Secp256k1`, and `Address::p2wpkh` / `Address::p2tr`:

```rust
let child = root.derive_priv(&secp, &path)?;
let compressed = CompressedPublicKey::from_private_key(&secp, &child.to_priv())?;
let addr = Address::p2wpkh(&compressed, network);
```

`cargo run -- rawdemo-check` derives this way and cross-checks it against `wallet.peek_address(KeychainKind::External, 0)` from BDK's own descriptor-based derivation — they must match bit-for-bit, since both are walking the same BIP32 path over the same key.

**Why reach for raw `rust-bitcoin` here at all**, given BDK already does derivation correctly? Two reasons that generalize past this demo:
1. **Trust boundary / auditability.** A descriptor string is opaque — to be sure BDK's descriptor parser is deriving what I think it's deriving (right path, right script type, right network), the only way to actually *verify* that from outside BDK's own code is to derive the same key independently with a different code path and compare. That's what this demo does.
2. **When you need a key, not a wallet.** Anything that isn't "manage a UTXO set and build transactions" — e.g. signing an arbitrary message with a specific derived key, building a raw multisig/timelock script by hand, or deriving a key for a protocol that isn't expressible as a BDK descriptor at all — doesn't need a `Wallet`. Pulling in descriptor/PSBT machinery for that would be the wrong tool; `rust-bitcoin`'s primitives are the right layer.

## Using this with real (mainnet) funds

This was built and verified against regtest; if you point it at mainnet, know what you're getting into:

- **Back up the seed phrase before funding the wallet.** `data/<name>/seed.txt` (mode `0600`, plaintext) is the *only* copy — write the 12 words down somewhere durable and offline. If that file is lost, the funds are unrecoverable; if it leaks, they're stolen.
- **Test with a trivial amount first.** Send yourself a few thousand sats, confirm it round-trips (`sync`, `balance`, a `send` back out) before trusting it with anything larger.
- **`send` requires a typed confirmation on mainnet** (`Network::Bitcoin`) — it prints the destination, amount, and fee rate and requires typing `send` to actually broadcast. Regtest/testnet skip this (so the scripted demo above stays non-interactive); this is the one thing that isn't optional on mainnet.
- **`init --force` refuses to overwrite a wallet that still holds a balance** unless you also pass `--force-confirm-loss` — it reads the existing database's balance before touching anything, so a `--force` typo on a funded wallet errors instead of orphaning funds.
- **Pick a real fee rate.** The default is 1 sat/vB, which is fine on regtest and often fine on testnet, but can leave a mainnet transaction unconfirmed indefinitely during congestion. Check a fee estimator and pass `--fee-rate` explicitly.
- Everything under [Known limitations](#known-limitations--what-id-improve-with-more-time) below (no RBF/CPFP, no passphrase, simplified coin selection, single-sig only) applies just as much on mainnet as on regtest — none of it was written with real value in mind.

## Known limitations / what I'd improve with more time

- **No BIP39 passphrase** — `seed::generate`/`import` use an empty passphrase. Easy to add (`init --passphrase`), left out to keep `init` simple.
- **Simplified fee-rate precision** — `--fee-rate` is whole sat/vB (`bitcoin::FeeRate::from_sat_per_vb` takes `u64`), no sub-satoshi rates.
- **`--manual-select` is a simplified heuristic** — largest-first, and doesn't iterate toward an optimal fee/change tradeoff the way `bdk_wallet`'s default coin selection algorithm does; it does correctly exclude immature coinbase UTXOs (`wallet::is_spendable_now`, verified against a real `bad-txns-premature-spend-of-coinbase` rejection during testing — see below), but for anything more sophisticated the default (non-manual) path is the better choice.
- **`Emitter` mempool eviction tracking starts empty on every process run** (`bdk_bitcoind_rpc::NO_EXPECTED_MEMPOOL_TXS` in `node::sync`) rather than being seeded from the wallet's own previously-known unconfirmed transactions. This means eviction detection for transactions that were already unconfirmed *before* this process started is incomplete on the very first `sync` of a run; new unconfirmed activity during the run is tracked correctly.
- **No RBF/CPFP support**, no label/metadata storage, no watch-only mode (import always expects to also sign). Multiple named wallets are supported (`--wallet <name>`), but a single CLI invocation still only ever acts on one of them.
- **Single-signature only** — no multisig descriptors.

## Proof of a working transaction (regtest self-test)

Recorded before the CLI grew named multi-wallet support, so `data/` here is what a single wallet's files (`data/<name>/...`) look like today — the paths moved one level deeper, nothing else about this run changed.

Full walkthrough (also see the two-stage script in the plan this was built from): local `bitcoind -regtest`, `cargo run -- init`, mined 150 blocks to the wallet's first address, `sync`, `send` to the wallet's own change address, confirmed on-chain, verified independently via the node's `getrawtransaction`, then confirmed persistence survives a process restart.

```
$ cargo run -- init
wallet initialized (kind=wpkh, network=Regtest)
first receive address: bcrt1qks6sp3rylsa4cnp5xk6fafy0pwvzz6dannazln

$ bitcoin-cli -regtest generatetoaddress 150 bcrt1qks6sp3rylsa4cnp5xk6fafy0pwvzz6dannazln

$ RUST_LOG=info cargo run -- sync
[INFO  ...::node] starting sync from height 0
[INFO  ...::node] applied 150 new block(s)
synced. balance: { immature: 4925 BTC, trusted_pending: 0 BTC, untrusted_pending: 0 BTC, confirmed: 2550 BTC }

$ cargo run -- address --change
bcrt1qa3qr8rwtj6mxrudf85kssgrsmj9zsspu6tvzhx (index 0, internal/change)

$ cargo run -- send --to bcrt1qa3qr8rwtj6mxrudf85kssgrsmj9zsspu6tvzhx --amount 100000 --fee-rate 2
broadcast: 0334929630cef5daa44903bc1ce37c5e9bba7c5efad890255d1bd3ad7e53fdc2

$ bitcoin-cli -regtest generatetoaddress 1 bcrt1qks6sp3rylsa4cnp5xk6fafy0pwvzz6dannazln
$ bitcoin-cli -regtest getrawtransaction 0334929630cef5daa44903bc1ce37c5e9bba7c5efad890255d1bd3ad7e53fdc2 true <blockhash>
{
  "in_active_chain": true,
  "txid": "0334929630cef5daa44903bc1ce37c5e9bba7c5efad890255d1bd3ad7e53fdc2",
  "vout": [ { "value": 0.00100000, "scriptPubKey": { "address": "bcrt1qa3qr8rwtj6mxrudf85kssgrsmj9zsspu6tvzhx" } }, ... ]
}

# Persistence check: fresh process, same DB — resumes instead of rescanning from genesis
$ RUST_LOG=info cargo run -- sync
[INFO  ...::node] starting sync from height 151
[INFO  ...::node] applied 0 new block(s)

# --manual-select, correctly rejects immature coinbase inputs, spends only mature ones
$ cargo run -- send --to bcrt1qee0nfw9efl69jqqc0hew0d64rpxp5p4643pszt --amount 50000 --fee-rate 2 --manual-select
broadcast: 2ae9d2bd0ce6a69ef36cdc3947dcdaee2fa2555ea277b2c31400e99b8896daff

# Raw rust-bitcoin vs BDK descriptor derivation cross-check (both wpkh and tr)
$ cargo run -- rawdemo-check
cross-check OK — BDK and raw rust-bitcoin agree: bcrt1qks6sp3rylsa4cnp5xk6fafy0pwvzz6dannazln
```

No seed phrase or private descriptor appears above or anywhere in this repo — only addresses, txids, and public transaction data.
