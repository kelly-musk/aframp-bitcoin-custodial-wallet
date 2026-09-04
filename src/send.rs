use anyhow::{bail, Context, Result};
use bdk_wallet::SignOptions;
use bitcoin::address::NetworkChecked;
use bitcoin::{Address, Amount, FeeRate, Transaction};

use crate::wallet::{is_spendable_now, persist, WalletCtx};

pub fn build_and_sign(
    ctx: &mut WalletCtx,
    to: Address<NetworkChecked>,
    amount: Amount,
    fee_rate: Option<FeeRate>,
    manual_select: bool,
) -> Result<Transaction> {
    let fee_rate = fee_rate.unwrap_or(FeeRate::from_sat_per_vb(1).expect("1 sat/vB is a valid fee rate"));

    // Explicit largest-first coin selection instead of BDK's default algorithm. This is a
    // simplified heuristic (doesn't precisely account for fees before selecting) — finish()
    // still computes the real fee and errors with InsufficientFunds if it under-shot.
    let chosen = if manual_select {
        let mut utxos: Vec<_> =
            ctx.wallet.list_unspent().filter(|u| is_spendable_now(ctx, u)).collect();
        utxos.sort_by_key(|u| std::cmp::Reverse(u.txout.value));

        let mut accumulated = Amount::ZERO;
        let mut chosen = Vec::new();
        for utxo in utxos {
            if accumulated >= amount {
                break;
            }
            accumulated += utxo.txout.value;
            chosen.push(utxo.outpoint);
        }
        if accumulated < amount {
            bail!("insufficient funds: have {accumulated}, need at least {amount}");
        }
        Some(chosen)
    } else {
        None
    };

    let mut builder = ctx.wallet.build_tx();
    builder.add_recipient(to.script_pubkey(), amount);
    builder.fee_rate(fee_rate);

    if let Some(chosen) = chosen {
        builder.manually_selected_only();
        builder.add_utxos(&chosen).context("selecting manual UTXOs")?;
    }

    let mut psbt = builder.finish().context("building transaction")?;

    let finalized = ctx
        .wallet
        .sign(&mut psbt, SignOptions::default())
        .context("signing transaction")?;
    if !finalized {
        bail!("PSBT did not finalize — wallet may be missing signer information");
    }

    persist(ctx)?;

    psbt.extract_tx().context("extracting final transaction from PSBT")
}
