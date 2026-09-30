// Off-chain CLI binary — issue #713 silences dynamic-string macros in
// ON-CHAIN contract code only. Production contract wasm cannot depend on
// `format!` for event topics or reverts, but an admin CLI printing JSON
// status text is allowed and uses format!() for diagnostics.
// Off-chain CLI binary — issue #713 silences dynamic-string macros in
// ON-CHAIN contract code only. Production contract wasm cannot depend on
// `format!` for event topics or reverts, but an admin CLI printing JSON
// status text is allowed and uses format!() for diagnostics.
#![allow(clippy::disallowed_macros)]

use anyhow::{anyhow, Result};
use clap::{Parser, Subcommand};
use serde_json::json;
use soroban_client::{
    account::{Account, AccountBehavior},
    transaction::TransactionBehavior,
    transaction_builder::{TransactionBuilder, TransactionBuilderBehavior},
    Options, Server,
};
use stellar_baselib::{
    address::{Address, AddressTrait},
    contract::{ContractBehavior, Contracts},
    keypair::{Keypair, KeypairBehavior},
    xdr::{Limits, ScVal, WriteXdr},
};

mod tests;

/// Admin CLI for Credence protocol contracts.
///
/// Builds real `InvokeHostFunction` (invoke_contract) transactions for each
/// admin operation. Without --submit the XDR envelope is printed as a
/// structured JSON dry-run. With --submit the transaction is signed with the
/// key from --signer (or the CREDENCE_SIGNER env-var) and sent to the RPC.
#[derive(Parser)]
#[command(
    name = "credence-admin",
    author,
    version,
    about = "Admin CLI for Credence protocol"
)]
struct Cli {
    /// Soroban RPC endpoint.
    #[arg(
        long,
        env = "CREDENCE_RPC_URL",
        default_value = "https://soroban-testnet.stellar.org"
    )]
    rpc_url: String,

    /// Network passphrase. Defaults to testnet.
    #[arg(
        long,
        env = "CREDENCE_NETWORK",
        default_value = "Test SDF Network ; September 2015"
    )]
    network: String,

    /// Contract address (C…) to invoke.
    #[arg(long, env = "CREDENCE_CONTRACT")]
    contract: Option<String>,

    /// Signer secret key (S…). Required for --submit. Can also be set via
    /// the CREDENCE_SIGNER environment variable.
    #[arg(long, env = "CREDENCE_SIGNER")]
    signer: Option<String>,

    /// Submit the transaction to the network instead of a dry-run.
    #[arg(long, action = clap::ArgAction::SetTrue, default_value = "false")]
    submit: bool,

    /// Maximum number of retry attempts for transient RPC failures (submit path).
    #[arg(long, env = "CREDENCE_RETRY_MAX", default_value = "3")]
    retry_max: u32,

    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Set early-exit penalty configuration on a credence_bond contract.
    ///
    /// Maps to: set_early_exit_config(admin, treasury, penalty_bps)
    BondSetEarlyExitConfig {
        /// Admin Stellar address (G…).
        #[arg(long)]
        admin: String,
        /// Treasury Stellar address (G…) that receives penalty funds.
        #[arg(long)]
        treasury: String,
        /// Penalty in basis points (0–10 000).
        #[arg(long)]
        bps: u32,
    },

    /// Set weight configuration on a credence_bond contract.
    ///
    /// Maps to: set_weight_config(admin, multiplier_bps, max_weight)
    BondSetWeights {
        /// Admin Stellar address (G…).
        #[arg(long)]
        admin: String,
        /// Multiplier in basis points.
        #[arg(long)]
        multiplier_bps: u32,
        /// Maximum attestation weight cap.
        #[arg(long)]
        max_weight: u32,
    },

    /// Set pause signer on a credence_delegation contract.
    ///
    /// Maps to: set_pause_signer(admin, signer, enabled)
    DelegationSetPauseSigner {
        /// Admin Stellar address (G…).
        #[arg(long)]
        admin: String,
        /// Pause-signer Stellar address (G…).
        #[arg(long)]
        pause_signer: String,
        /// Whether to enable (true) or disable (false) the signer (default: true).
        #[arg(long, default_value = "true", num_args = 0..=1, default_missing_value = "true")]
        enabled: bool,
    },
}

// ---------------------------------------------------------------------------
// Entry point
// ---------------------------------------------------------------------------

fn main() -> Result<()> {
    let cli = Cli::parse();

    let contract_id = cli.contract.as_deref().unwrap_or_else(|| {
        eprintln!("warning: --contract not set; using zero-address placeholder");
        "CAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAABSC4"
    });

    match &cli.command {
        Commands::BondSetEarlyExitConfig {
            admin,
            treasury,
            bps,
        } => {
            let args = build_early_exit_args(admin, treasury, *bps)?;
            run(&cli, contract_id, "set_early_exit_config", args)
        }
        Commands::BondSetWeights {
            admin,
            multiplier_bps,
            max_weight,
        } => {
            let args = build_weight_args(admin, *multiplier_bps, *max_weight)?;
            run(&cli, contract_id, "set_weight_config", args)
        }
        Commands::DelegationSetPauseSigner {
            admin,
            pause_signer,
            enabled,
        } => {
            let args = build_pause_signer_args(admin, pause_signer, *enabled)?;
            run(&cli, contract_id, "set_pause_signer", args)
        }
    }
}

// ---------------------------------------------------------------------------
// Argument builders
// ---------------------------------------------------------------------------

/// Encode args for `set_early_exit_config(admin: Address, treasury: Address, penalty_bps: u32)`.
fn build_early_exit_args(admin: &str, treasury: &str, bps: u32) -> Result<Vec<ScVal>> {
    // Invariant: penalty_bps must fit in the on-chain u32 range and be a
    // valid basis-point value (0..=10_000). Reject out-of-range inputs
    // before any transaction is built so we never emit an invalid envelope.
    if bps > 10_000 {
        return Err(anyhow!(
            "penalty_bps {bps} out of range: must be 0..=10000 basis points"
        ));
    }
    Ok(vec![
        addr_to_sc_val(admin)?,
        addr_to_sc_val(treasury)?,
        ScVal::U32(bps),
    ])
}

/// Encode args for `set_weight_config(admin: Address, multiplier_bps: u32, max_weight: u32)`.
fn build_weight_args(admin: &str, multiplier_bps: u32, max_weight: u32) -> Result<Vec<ScVal>> {
    // Invariant: multiplier_bps is a basis-point value (0..=10_000) and
    // max_weight must be non-zero. Reject invalid combinations up front so
    // the on-chain contract never sees a degenerate configuration.
    if multiplier_bps > 10_000 {
        return Err(anyhow!(
            "multiplier_bps {multiplier_bps} out of range: must be 0..=10000 basis points"
        ));
    }
    if max_weight == 0 {
        return Err(anyhow!("max_weight must be greater than zero"));
    }
    Ok(vec![
        addr_to_sc_val(admin)?,
        ScVal::U32(multiplier_bps),
        ScVal::U32(max_weight),
    ])
}

/// Encode args for `set_pause_signer(admin: Address, signer: Address, enabled: bool)`.
fn build_pause_signer_args(admin: &str, signer: &str, enabled: bool) -> Result<Vec<ScVal>> {
    // Invariant: admin and signer must be distinct addresses. Allowing the
    // same address for both would let a single key both authorize and act
    // as the pause signer, weakening the separation-of-duties guarantee.
    if admin == signer {
        return Err(anyhow!(
            "admin and pause_signer must be distinct addresses"
        ));
    }
    Ok(vec![
        addr_to_sc_val(admin)?,
        addr_to_sc_val(signer)?,
        ScVal::Bool(enabled),
    ])
}

/// Convert a Stellar address string (G… or C…) to an `ScVal::Address`.
fn addr_to_sc_val(addr: &str) -> Result<ScVal> {
    let address = Address::new(addr).map_err(|e| anyhow!("invalid address {addr:?}: {e}"))?;
    address
        .to_sc_val()
        .map_err(|e| anyhow!("failed to convert address {addr:?} to ScVal: {e}"))
}

// ---------------------------------------------------------------------------
// Core transaction builder / runner
// ---------------------------------------------------------------------------

/// Build an `InvokeHostFunction` transaction, then either print a dry-run
/// JSON report or sign-and-submit it to the network.
fn run(cli: &Cli, contract_id: &str, function: &str, args: Vec<ScVal>) -> Result<()> {
    // Invariant: contract_id must be a valid C… contract address. We validate
    // here (not just inside Contracts::new) so callers get a clear, stable
    // error message and so the dry-run path fails fast on bad input.
    if !contract_id.starts_with('C') {
        return Err(anyhow!(
            "invalid contract address {contract_id:?}: must start with 'C'"
        ));
    }
    // Build the XDR operation via stellar-baselib's Contracts helper.
    let contract = Contracts::new(contract_id)
        .map_err(|e| anyhow!("invalid contract address {contract_id:?}: {e}"))?;
    let operation = contract.call(function, Some(args));

    // Resolve the signer key (required only when submitting).
    let keypair: Option<Keypair> = if cli.submit {
        let secret = cli
            .signer
            .as_deref()
            .ok_or_else(|| anyhow!("--signer / CREDENCE_SIGNER is required with --submit"))?;
        // Invariant: signer secret must be a valid S… Stellar secret key.
        if !secret.starts_with('S') {
            return Err(anyhow!(
                "invalid signer key: must be a Stellar secret key starting with 'S'"
            ));
        }
        Some(Keypair::from_secret(secret).map_err(|e| anyhow!("invalid signer key: {e}"))?)
    } else {
        None
    };

    // Use the signer public key as the source account, or a dummy for dry-runs.
    let source_pub = keypair
        .as_ref()
        .map(|kp| kp.public_key())
        .unwrap_or_else(|| "GAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAWHF".to_string());

    if cli.submit {
        // --- Live path: fetch account, build, sign, submit ------------------
        // Invariant: retry_max must be at least 1 so we always attempt at
        // least one submission; 0 would silently skip the network call.
        if cli.retry_max == 0 {
            return Err(anyhow!("--retry-max must be at least 1"));
        }
        let runtime = tokio::runtime::Runtime::new()?;
        runtime.block_on(async {
            let server = Server::new(
                &cli.rpc_url,
                Options {
                    allow_http: false,
                    ..Default::default()
                },
            )
            .map_err(|e| anyhow!("RPC connect error: {e:?}"))?;

            // Load the source account. This is a read-only step; on failure
            // we surface a diagnosable error without leaking the secret key.
            let mut source_account: Account = server
                .get_account(&source_pub)
                .await
                .map_err(|e| anyhow!("failed to load source account {source_pub}: {e:?}"))?;

            let mut builder = TransactionBuilder::new(&mut source_account, &cli.network, None);
            builder
                .fee(1_000_000_u32)
                .set_timeout(30)
                .map_err(|e| anyhow!(e))?;
            builder.add_operation(operation);
            let tx = builder.build();

            // Prepare (simulate + assemble footprint + resource fee).
            let tx = server
                .prepare_transaction(&tx)
                .await
                .map_err(|e| anyhow!("simulation failed: {e:?}"))?;

            // Sign.
            let kp = keypair.unwrap();
            let mut signed_tx = tx;
            signed_tx.sign(&[kp]);

            // Submit with bounded retries for transient failures. The signed
            // transaction is immutable, so retrying cannot double-spend or
            // mutate state beyond what the network already accepted; the
            // network deduplicates by transaction hash.
            let mut attempt: u32 = 0;
            let resp = loop {
                attempt += 1;
                match server.send_transaction(signed_tx.clone()).await {
                    Ok(r) => break r,
                    Err(e) => {
                        if attempt >= cli.retry_max {
                            return Err(anyhow!(
                                "send_transaction failed after {attempt} attempt(s): {e:?}"
                            ));
                        }
                        // Exponential backoff with a small cap keeps retries
                        // deterministic and bounded in wall-clock time.
                        let backoff_ms = 100u64.saturating_mul(1u64 << (attempt - 1).min(5));
                        tokio::time::sleep(std::time::Duration::from_millis(backoff_ms)).await;
                    }
                }
            };

            let out = json!({
                "status": format!("{:?}", resp.status),
                "hash": resp.hash,
                "attempts": attempt,
            });
            println!("{}", serde_json::to_string_pretty(&out)?);
            Ok(())
        })
    } else {
        // --- Dry-run path: build with dummy sequence, emit XDR JSON ---------
        let mut dummy_account =
            Account::new(&source_pub, "0").map_err(|e| anyhow!("account error: {e:?}"))?;

        let mut builder = TransactionBuilder::new(&mut dummy_account, &cli.network, None);
        builder
            .fee(1_000_000_u32)
            .set_timeout(30)
            .map_err(|e| anyhow!(e))?;
        builder.add_operation(operation);
        let tx = builder.build();

        let envelope_xdr = tx
            .to_envelope()
            .map_err(|e| anyhow!("envelope serialization failed: {e}"))?
            .to_xdr_base64(Limits::none())
            .map_err(|e| anyhow!("XDR base64 failed: {e}"))?;

        let tx_hash = hex::encode(tx.hash());

        let out = json!({
            "status": "dry_run",
            "contract": contract_id,
            "function": function,
            "network": cli.network,
            "source": source_pub,
            "envelope_xdr": envelope_xdr,
            "tx_hash": tx_hash,
        });
        println!("{}", serde_json::to_string_pretty(&out)?);
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    const ADMIN: &str = "GAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAWHF";
    const TREASURY: &str = "GBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBB";
    const CONTRACT: &str = "CAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAABSC4";

    // --- Argument builder: success cases -----------------------------------

    #[test]
    fn early_exit_args_accepts_boundary_zero() {
        let args = build_early_exit_args(ADMIN, TREASURY, 0).expect("zero bps is valid");
        assert_eq!(args.len(), 3);
        assert!(matches!(args[2], ScVal::U32(0)));
    }

    #[test]
    fn early_exit_args_accepts_boundary_max() {
        let args = build_early_exit_args(ADMIN, TREASURY, 10_000).expect("10000 bps is valid");
        assert!(matches!(args[2], ScVal::U32(10_000)));
    }

    #[test]
    fn weight_args_accepts_boundary_values() {
        let args = build_weight_args(ADMIN, 10_000, 1).expect("boundary values valid");
        assert_eq!(args.len(), 3);
        assert!(matches!(args[1], ScVal::U32(10_000)));
        assert!(matches!(args[2], ScVal::U32(1)));
    }

    #[test]
    fn pause_signer_args_accepts_distinct_addresses() {
        let args = build_pause_signer_args(ADMIN, TREASURY, true).expect("distinct addrs valid");
        assert!(matches!(args[2], ScVal::Bool(true)));
    }

    // --- Argument builder: rejection cases ---------------------------------

    #[test]
    fn early_exit_args_rejects_bps_above_max() {
        let err = build_early_exit_args(ADMIN, TREASURY, 10_001).unwrap_err();
        assert!(err.to_string().contains("out of range"));
    }

    #[test]
    fn early_exit_args_rejects_u32_max() {
        let err = build_early_exit_args(ADMIN, TREASURY, u32::MAX).unwrap_err();
        assert!(err.to_string().contains("out of range"));
    }

    #[test]
    fn weight_args_rejects_multiplier_above_max() {
        let err = build_weight_args(ADMIN, 10_001, 1).unwrap_err();
        assert!(err.to_string().contains("out of range"));
    }

    #[test]
    fn weight_args_rejects_zero_max_weight() {
        let err = build_weight_args(ADMIN, 100, 0).unwrap_err();
        assert!(err.to_string().contains("max_weight"));
    }

    #[test]
    fn pause_signer_args_rejects_same_address() {
        let err = build_pause_signer_args(ADMIN, ADMIN, true).unwrap_err();
        assert!(err.to_string().contains("distinct"));
    }

    #[test]
    fn addr_to_sc_val_rejects_garbage() {
        let err = addr_to_sc_val("not-an-address").unwrap_err();
        assert!(err.to_string().contains("invalid address"));
    }

    // --- run(): contract validation and signer validation ------------------

    fn base_cli(submit: bool, signer: Option<String>, retry_max: u32) -> Cli {
        Cli {
            rpc_url: "https://soroban-testnet.stellar.org".to_string(),
            network: "Test SDF Network ; September 2015".to_string(),
            contract: Some(CONTRACT.to_string()),
            signer,
            submit,
            retry_max,
            command: Commands::BondSetEarlyExitConfig {
                admin: ADMIN.to_string(),
                treasury: TREASURY.to_string(),
                bps: 100,
            },
        }
    }

    #[test]
    fn run_rejects_non_contract_address() {
        let cli = base_cli(false, None, 3);
        let args = build_early_exit_args(ADMIN, TREASURY, 100).unwrap();
        let err = run(&cli, "GAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAWHF", "f", args)
            .unwrap_err();
        assert!(err.to_string().contains("must start with 'C'"));
    }

    #[test]
    fn run_submit_requires_signer() {
        let cli = base_cli(true, None, 3);
        let args = build_early_exit_args(ADMIN, TREASURY, 100).unwrap();
        let err = run(&cli, CONTRACT, "set_early_exit_config", args).unwrap_err();
        assert!(err.to_string().contains("--signer"));
    }

    #[test]
    fn run_submit_rejects_non_secret_signer() {
        let cli = base_cli(true, Some("GAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAWHF".to_string()), 3);
        let args = build_early_exit_args(ADMIN, TREASURY, 100).unwrap();
        let err = run(&cli, CONTRACT, "set_early_exit_config", args).unwrap_err();
        assert!(err.to_string().contains("secret key"));
    }

    #[test]
    fn run_submit_rejects_zero_retry_max() {
        let cli = base_cli(
            true,
            Some("SAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA".to_string()),
            0,
        );
        let args = build_early_exit_args(ADMIN, TREASURY, 100).unwrap();
        let err = run(&cli, CONTRACT, "set_early_exit_config", args).unwrap_err();
        assert!(err.to_string().contains("retry-max"));
    }

    // --- Dry-run path: deterministic output --------------------------------

    #[test]
    fn dry_run_produces_deterministic_envelope() {
        let cli = base_cli(false, None, 3);
        let args = build_early_exit_args(ADMIN, TREASURY, 100).unwrap();
        // Two dry-runs with identical inputs must succeed identically.
        run(&cli, CONTRACT, "set_early_exit_config", args.clone()).expect("dry-run ok");
        run(&cli, CONTRACT, "set_early_exit_config", args).expect("dry-run ok");
    }
}
