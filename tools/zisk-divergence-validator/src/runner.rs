//! Run one scenario on both engines: native ZKsync OS through the test rig,
//! then the ZiSK guest over the witness that run produced.

use std::collections::HashMap;

use alloy::consensus::TxEip1559;
use alloy::dyn_abi::{DynSolType, DynSolValue, Specifier};
use alloy::primitives::{Address, TxKind, U256 as AlloyU256};
use alloy::signers::local::PrivateKeySigner;
use anyhow::{bail, Context};
use rig::chain::RunConfig;
use rig::TestingFramework;
use zksync_os_tests_common::zksync_tx::ZKsyncTxEnvelope;
use zksync_os_zisk_test_utils::{
    build_batch_input, check_against_native, Conversion, HeaderHashCheck, NativeCheck,
    StateDumpBundle,
};

use crate::compiler::CompiledContract;
use crate::harvest::DumpHarvester;
use crate::report::{BlockSummary, StepResult};
use crate::scenario::{BlockDef, Scenario, Step};

/// Chain id of the rig's default chain.
const CHAIN_ID: u64 = 37;
const DEFAULT_GAS_LIMIT: u64 = 5_000_000;
const DEFAULT_MAX_FEE: u128 = 1_000;
const DEFAULT_PRIORITY_FEE: u128 = 1_000;

/// What one scenario produced on both engines.
pub struct ScenarioOutcome {
    pub block: BlockSummary,
    pub steps: Vec<StepResult>,
    pub check: NativeCheck,
    pub conversion: Conversion,
}

/// Run the guest over a state dump and compare it against the native
/// reference values the dump carries.
///
/// The conversion asserts the dump is self-consistent, and those assertions
/// are its rejection mechanism, so a panic here means the dump is malformed
/// rather than that the two engines disagree.
pub fn check_bundle(bundle: &StateDumpBundle) -> anyhow::Result<(NativeCheck, Conversion)> {
    let conversion = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        build_batch_input(bundle, HeaderHashCheck::Armed)
    }))
    .map_err(|payload| {
        anyhow::anyhow!(
            "the state dump did not convert to a batch input: {}",
            zksync_os_zisk_test_utils::panic_message(payload.as_ref())
        )
    })?;
    let check = check_against_native(bundle, &conversion.batch_input);
    Ok((check, conversion))
}

pub fn run_scenario(
    scenario: &Scenario,
    artifacts: &HashMap<String, CompiledContract>,
    harvester: &DumpHarvester,
) -> anyhow::Result<ScenarioOutcome> {
    // Create signers for each named account.
    let mut signers: HashMap<String, PrivateKeySigner> = HashMap::new();
    for name in scenario.accounts.keys() {
        signers.insert(name.clone(), PrivateKeySigner::random());
    }
    // Also create signers for any "from" references not in accounts.
    for step in &scenario.steps {
        let from = match step {
            Step::Deploy { from, .. } | Step::Call { from, .. } | Step::SendRaw { from, .. } => {
                from
            }
        };
        signers
            .entry(from.clone())
            .or_insert_with(PrivateKeySigner::random);
    }

    // Set up the testing framework. The rig's own REVM consistency check is a
    // different comparison against a different adapted REVM; this tool runs
    // the guest instead. Note that `without_riscv_run()` alone doesn't disable
    // it (it can still be enabled via ZKSYNC_REVM_CONSISTENCY_CHECK).
    let mut run_config = RunConfig::without_riscv_run();
    run_config.disable_revm_consistency_check();

    let mut tester = TestingFramework::new()
        .with_system_contracts(true, true)
        .with_run_config(run_config);

    // Fund accounts.
    for (name, def) in &scenario.accounts {
        let signer = &signers[name];
        let balance = parse_u256(&def.balance)
            .with_context(|| format!("invalid balance for account '{name}'"))?;
        tester.set_balance(signer.address(), balance);
    }
    // Ensure all senders have at least some balance.
    for (name, signer) in &signers {
        if !scenario.accounts.contains_key(name) {
            tester.set_balance(
                signer.address(),
                ruint::aliases::U256::from(1_000_000_000_000_000_u64),
            );
        }
    }

    // Predeploy contracts with raw bytecode at specified addresses.
    let mut predeployed_addresses: HashMap<String, Address> = HashMap::new();
    for (name, def) in &scenario.contracts {
        if let Some(ref bytecode_hex) = def.bytecode {
            let addr_str = def.address.as_ref().with_context(|| {
                format!("contract '{name}' has 'bytecode' but no 'address' for predeployment")
            })?;
            let addr: Address = addr_str
                .parse()
                .with_context(|| format!("invalid address for contract '{name}': {addr_str}"))?;
            let hex_str = bytecode_hex.strip_prefix("0x").unwrap_or(bytecode_hex);
            let bytecode = hex::decode(hex_str)
                .with_context(|| format!("invalid bytecode hex for '{name}'"))?;
            tester.set_evm_contract(addr, &bytecode);
            predeployed_addresses.insert(name.clone(), addr);
        }
    }

    // Apply block context if specified.
    if let Some(ref block) = scenario.block {
        tester.set_block_context(Some(make_block_context(block)));
    }

    let (transactions, step_descriptions) =
        build_transactions(scenario, artifacts, &signers, &predeployed_addresses)?;

    // Execute the block on native ZKsync OS. The rig's state-dump hook writes
    // the witness and the native reference commitments for this block.
    let block_number = tester.chain().next_block_number();
    let block_output = tester
        .execute_block_no_panic(transactions)
        .map_err(|err| anyhow::anyhow!("native ZKsync OS could not execute the block: {err:?}"))?;

    let mut steps = Vec::new();
    for (index, description) in step_descriptions.iter().enumerate() {
        let (success, gas_used) = match block_output.tx_results.get(index) {
            Some(Ok(output)) => (output.is_success(), Some(output.gas_used)),
            Some(Err(_)) | None => (false, None),
        };
        steps.push(StepResult {
            description: description.clone(),
            success,
            gas_used,
        });
    }

    let bundle = harvester.take(block_number)?;
    let (check, conversion) = check_bundle(&bundle)?;

    Ok(ScenarioOutcome {
        block: BlockSummary {
            number: block_number,
            transactions: step_descriptions.len(),
        },
        steps,
        check,
        conversion,
    })
}

/// Turn the scenario's steps into the block's signed transactions, with one
/// description per step for the report.
fn build_transactions(
    scenario: &Scenario,
    artifacts: &HashMap<String, CompiledContract>,
    signers: &HashMap<String, PrivateKeySigner>,
    predeployed_addresses: &HashMap<String, Address>,
) -> anyhow::Result<(Vec<ZKsyncTxEnvelope>, Vec<String>)> {
    let mut transactions = Vec::new();
    let mut nonces: HashMap<String, u64> = HashMap::new();
    let mut deployed_addresses: Vec<Address> = Vec::new();
    let mut step_descriptions: Vec<String> = Vec::new();

    for step in &scenario.steps {
        match step {
            Step::Deploy {
                contract,
                from,
                args,
                gas,
                value,
            } => {
                let signer = signers
                    .get(from)
                    .with_context(|| format!("unknown sender '{from}'"))?;
                let artifact = artifacts
                    .get(contract)
                    .with_context(|| format!("no compiled artifact for contract '{contract}'"))?;

                let nonce = nonces.entry(from.clone()).or_insert(0);
                let deploy_addr = signer.address().create(*nonce);

                let mut init_code = artifact.bytecode.clone();
                if !args.is_empty() {
                    let constructor = artifact.abi.constructor().with_context(|| {
                        format!("contract '{contract}' has no constructor but args were given")
                    })?;
                    let encoded_args = encode_args(&constructor.inputs, args)?;
                    init_code.extend_from_slice(&encoded_args);
                }

                let value = match value {
                    Some(v) => parse_alloy_u256(v)?,
                    None => AlloyU256::ZERO,
                };

                let tx = TxEip1559 {
                    chain_id: CHAIN_ID,
                    nonce: *nonce,
                    max_fee_per_gas: DEFAULT_MAX_FEE,
                    max_priority_fee_per_gas: DEFAULT_PRIORITY_FEE,
                    gas_limit: gas.unwrap_or(DEFAULT_GAS_LIMIT),
                    to: TxKind::Create,
                    value,
                    access_list: Default::default(),
                    input: init_code.into(),
                };

                transactions.push(ZKsyncTxEnvelope::from_eth_tx(tx, signer.clone()));
                deployed_addresses.push(deploy_addr);
                step_descriptions.push(format!("deploy {contract} -> {deploy_addr}"));
                *nonce += 1;
            }
            Step::Call {
                to,
                from,
                function,
                args,
                gas,
                value,
            } => {
                let signer = signers
                    .get(from)
                    .with_context(|| format!("unknown sender '{from}'"))?;
                let nonce = nonces.entry(from.clone()).or_insert(0);

                let target =
                    resolve_address(to, &deployed_addresses, signers, predeployed_addresses)?;
                let calldata = encode_function_call(function, args)?;

                let value = match value {
                    Some(v) => parse_alloy_u256(v)?,
                    None => AlloyU256::ZERO,
                };

                let tx = TxEip1559 {
                    chain_id: CHAIN_ID,
                    nonce: *nonce,
                    max_fee_per_gas: DEFAULT_MAX_FEE,
                    max_priority_fee_per_gas: DEFAULT_PRIORITY_FEE,
                    gas_limit: gas.unwrap_or(DEFAULT_GAS_LIMIT),
                    to: TxKind::Call(target),
                    value,
                    access_list: Default::default(),
                    input: calldata.into(),
                };

                transactions.push(ZKsyncTxEnvelope::from_eth_tx(tx, signer.clone()));
                step_descriptions.push(format!("call {function} on {target}"));
                *nonce += 1;
            }
            Step::SendRaw {
                to,
                from,
                data,
                gas,
                value,
            } => {
                let signer = signers
                    .get(from)
                    .with_context(|| format!("unknown sender '{from}'"))?;
                let nonce = nonces.entry(from.clone()).or_insert(0);

                let to_kind = match to {
                    Some(addr) => {
                        let target = resolve_address(
                            addr,
                            &deployed_addresses,
                            signers,
                            predeployed_addresses,
                        )?;
                        TxKind::Call(target)
                    }
                    None => TxKind::Create,
                };

                let raw_data = data
                    .as_deref()
                    .map(|d| {
                        let d = d.strip_prefix("0x").unwrap_or(d);
                        hex::decode(d).context("invalid hex in 'data' field")
                    })
                    .transpose()?
                    .unwrap_or_default();

                let value = match value {
                    Some(v) => parse_alloy_u256(v)?,
                    None => AlloyU256::ZERO,
                };

                let tx = TxEip1559 {
                    chain_id: CHAIN_ID,
                    nonce: *nonce,
                    max_fee_per_gas: DEFAULT_MAX_FEE,
                    max_priority_fee_per_gas: DEFAULT_PRIORITY_FEE,
                    gas_limit: gas.unwrap_or(DEFAULT_GAS_LIMIT),
                    to: to_kind,
                    value,
                    access_list: Default::default(),
                    input: raw_data.into(),
                };

                let desc = match to {
                    Some(addr) => format!("send_raw to {addr}"),
                    None => "send_raw CREATE".to_string(),
                };

                transactions.push(ZKsyncTxEnvelope::from_eth_tx(tx, signer.clone()));
                step_descriptions.push(desc);
                if to.is_none() {
                    deployed_addresses.push(signer.address().create(*nonce));
                }
                *nonce += 1;
            }
        }
    }

    Ok((transactions, step_descriptions))
}

/// Resolve a target address from a string like "$deployed:0", a contract name, a named account,
/// or a hex address.
fn resolve_address(
    target: &str,
    deployed: &[Address],
    signers: &HashMap<String, PrivateKeySigner>,
    predeployed: &HashMap<String, Address>,
) -> anyhow::Result<Address> {
    if let Some(idx_str) = target.strip_prefix("$deployed:") {
        let idx: usize = idx_str
            .parse()
            .with_context(|| format!("invalid deploy index: {idx_str}"))?;
        deployed.get(idx).copied().with_context(|| {
            format!(
                "no deploy at index {idx} (only {} deploys so far)",
                deployed.len()
            )
        })
    } else if let Some(addr) = predeployed.get(target) {
        Ok(*addr)
    } else if let Some(signer) = signers.get(target) {
        Ok(signer.address())
    } else if target.starts_with("0x") || target.starts_with("0X") {
        target
            .parse::<Address>()
            .with_context(|| format!("invalid address: {target}"))
    } else {
        bail!(
            "cannot resolve target '{target}': not a $deployed ref, contract name, named account, or hex address"
        );
    }
}

/// Encode function call from a signature like "transfer(address,uint256)" and JSON args.
fn encode_function_call(sig: &str, args: &[serde_json::Value]) -> anyhow::Result<Vec<u8>> {
    let func = alloy::json_abi::Function::parse(sig)
        .with_context(|| format!("failed to parse function signature: {sig}"))?;

    let selector = func.selector();
    let encoded_args = encode_args(&func.inputs, args)?;

    let mut calldata = Vec::with_capacity(4 + encoded_args.len());
    calldata.extend_from_slice(selector.as_slice());
    calldata.extend_from_slice(&encoded_args);
    Ok(calldata)
}

/// ABI-encode arguments given param definitions and JSON values.
fn encode_args(
    params: &[alloy::json_abi::Param],
    args: &[serde_json::Value],
) -> anyhow::Result<Vec<u8>> {
    if params.len() != args.len() {
        bail!(
            "argument count mismatch: expected {}, got {}",
            params.len(),
            args.len()
        );
    }

    let mut values = Vec::with_capacity(args.len());
    for (param, arg) in params.iter().zip(args) {
        let sol_type: DynSolType = param
            .resolve()
            .with_context(|| format!("failed to resolve type for param '{}'", param.name))?;
        let value = json_to_dyn_sol_value(&sol_type, arg)
            .with_context(|| format!("failed to convert arg for param '{}'", param.name))?;
        values.push(value);
    }

    Ok(DynSolValue::Tuple(values).abi_encode_params())
}

/// Convert a JSON value to a DynSolValue based on the expected Solidity type.
fn json_to_dyn_sol_value(
    sol_type: &DynSolType,
    value: &serde_json::Value,
) -> anyhow::Result<DynSolValue> {
    match sol_type {
        DynSolType::Address => {
            let s = value.as_str().context("expected string for address")?;
            let addr: Address = s.parse().context("invalid address")?;
            Ok(DynSolValue::Address(addr))
        }
        DynSolType::Bool => {
            let b = value.as_bool().context("expected bool")?;
            Ok(DynSolValue::Bool(b))
        }
        DynSolType::Uint(bits) => {
            let s = match value {
                serde_json::Value::Number(n) => n.to_string(),
                serde_json::Value::String(s) => s.clone(),
                _ => bail!("expected number or string for uint"),
            };
            let v: AlloyU256 = if s.starts_with("0x") || s.starts_with("0X") {
                AlloyU256::from_str_radix(&s[2..], 16)?
            } else {
                AlloyU256::from_str_radix(&s, 10)?
            };
            Ok(DynSolValue::Uint(v, *bits))
        }
        DynSolType::Int(bits) => {
            let s = match value {
                serde_json::Value::Number(n) => n.to_string(),
                serde_json::Value::String(s) => s.clone(),
                _ => bail!("expected number or string for int"),
            };
            let v: alloy::primitives::I256 = s.parse().context("invalid int")?;
            Ok(DynSolValue::Int(v, *bits))
        }
        DynSolType::Bytes => {
            let s = value.as_str().context("expected hex string for bytes")?;
            let s = s.strip_prefix("0x").unwrap_or(s);
            let bytes = hex::decode(s).context("invalid hex for bytes")?;
            Ok(DynSolValue::Bytes(bytes))
        }
        DynSolType::FixedBytes(len) => {
            let s = value.as_str().context("expected hex string for bytes")?;
            let s = s.strip_prefix("0x").unwrap_or(s);
            let bytes = hex::decode(s).context("invalid hex for fixed bytes")?;
            if bytes.len() != *len {
                bail!("expected {len} bytes, got {}", bytes.len());
            }
            Ok(DynSolValue::FixedBytes(
                alloy::primitives::FixedBytes::from_slice(&bytes),
                *len,
            ))
        }
        DynSolType::String => {
            let s = value.as_str().context("expected string")?;
            Ok(DynSolValue::String(s.to_string()))
        }
        DynSolType::Array(inner) => {
            let arr = value.as_array().context("expected array")?;
            let vals: Vec<DynSolValue> = arr
                .iter()
                .map(|v| json_to_dyn_sol_value(inner, v))
                .collect::<anyhow::Result<_>>()?;
            Ok(DynSolValue::Array(vals))
        }
        DynSolType::Tuple(types) => {
            let arr = value.as_array().context("expected array for tuple")?;
            if arr.len() != types.len() {
                bail!(
                    "tuple length mismatch: expected {}, got {}",
                    types.len(),
                    arr.len()
                );
            }
            let vals: Vec<DynSolValue> = types
                .iter()
                .zip(arr)
                .map(|(t, v)| json_to_dyn_sol_value(t, v))
                .collect::<anyhow::Result<_>>()?;
            Ok(DynSolValue::Tuple(vals))
        }
        _ => bail!("unsupported Solidity type: {sol_type:?}"),
    }
}

fn parse_u256(s: &str) -> anyhow::Result<ruint::aliases::U256> {
    if s.starts_with("0x") || s.starts_with("0X") {
        ruint::aliases::U256::from_str_radix(&s[2..], 16).context("invalid hex U256")
    } else {
        ruint::aliases::U256::from_str_radix(s, 10).context("invalid decimal U256")
    }
}

fn parse_alloy_u256(s: &str) -> anyhow::Result<AlloyU256> {
    if s.starts_with("0x") || s.starts_with("0X") {
        AlloyU256::from_str_radix(&s[2..], 16).context("invalid hex U256")
    } else {
        AlloyU256::from_str_radix(s, 10).context("invalid decimal U256")
    }
}

fn make_block_context(block: &BlockDef) -> rig::BlockContext {
    rig::BlockContext {
        eip1559_basefee: ruint::aliases::U256::from(block.basefee.unwrap_or(DEFAULT_MAX_FEE)),
        native_price: ruint::aliases::U256::from(0u64), // native resources will be unlimited
        pubdata_price: Default::default(),
        timestamp: block.timestamp.unwrap_or(1_700_000_000),
        gas_limit: block.gas_limit.unwrap_or(30_000_000),
        pubdata_limit: u64::MAX,
        coinbase: Default::default(),
        mix_hash: Default::default(),
        blob_fee: Default::default(),
    }
}
