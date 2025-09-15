use num_bigint::BigUint;
use num_traits::cast::ToPrimitive;
use std::str::FromStr;
use tracing::{debug, info, warn};
use tycho_simulation::tycho_core::Bytes;
use tycho_simulation::protocol::state::ProtocolSim;

use crate::models::{
    messages::{AmountOutRequest, AmountOutResponse},
    state::AppState,
};

pub async fn get_amounts_out(
    state: AppState,
    request: AmountOutRequest,
) -> Result<Vec<AmountOutResponse>, String> {
    let token_in_address = request.token_in.trim_start_matches("0x").to_lowercase();
    let token_out_address = request.token_out.trim_start_matches("0x").to_lowercase();

    let token_in_bytes = Bytes::from_str(&token_in_address)
        .map_err(|e| format!("Invalid token_in address: {}", e))?;
    let token_out_bytes = Bytes::from_str(&token_out_address)
        .map_err(|e| format!("Invalid token_out address: {}", e))?;

    let token_in = state
        .tokens
        .get(&token_in_bytes)
        .ok_or_else(|| format!("Token not found: {}", token_in_address))?;

    let token_out = state
        .tokens
        .get(&token_out_bytes)
        .ok_or_else(|| format!("Token not found: {}", token_out_address))?;

    info!(
        "Processing quote: {} ({}) -> {} ({})",
        token_in.symbol, token_in_address, token_out.symbol, token_out_address
    );

    let amounts_in: Result<Vec<BigUint>, String> = request
        .amounts
        .iter()
        .map(|amount| {
            BigUint::from_str(amount).map_err(|e| format!("Invalid amount {}: {}", amount, e))
        })
        .collect();
    let amounts_in = amounts_in?;

    let states_read = state.states.read().await;
    let current_block = *state.current_block.read().await;
    debug!(
        "Current block: {}, total pools: {}",
        current_block,
        states_read.len()
    );

    // Guard: service not ready yet
    if current_block == 0 || states_read.is_empty() {
        let warmup_msg = format!(
            "Service warming up: block={}, pools={}",
            current_block,
            states_read.len()
        );
        warn!("{}", warmup_msg);
        return Err(warmup_msg);
    }

    let mut results = Vec::new();
    let mut matching_pools = 0; // pools whose declared tokens include both sides
    // pools we attempted to quote against (set after candidate discovery)
    let mut pools_with_quotes = 0;
    let mut first_error: Option<String> = None; // capture first failure reason

    let mut candidates: Vec<(String, Box<dyn ProtocolSim>, String)> = Vec::new();
    for (id, (pool_state, comp)) in states_read.iter() {
        let pool_tokens: Vec<String> = comp
            .tokens
            .iter()
            .map(|t| t.address.to_string().trim_start_matches("0x").to_lowercase())
            .collect();

        if !pool_tokens.is_empty()
            && pool_tokens.contains(&token_in_address)
            && pool_tokens.contains(&token_out_address)
        {
            matching_pools += 1;
            candidates.push((
                id.clone(),
                pool_state.clone(),
                comp.id.to_string(),
            ));
            debug!("Found matching pool: {}", id);
        }
    }
    drop(states_read);

    let candidate_pools = matching_pools;
    info!(
        "Quote candidates prepared: matching_pools={} amounts_per_pool={}",
        matching_pools,
        amounts_in.len()
    );

    for (id, pool_state, pool_addr) in candidates.into_iter() {
        let mut amounts_out = Vec::new();
        let mut gas_used = Vec::new();

        for amount_in in amounts_in.iter() {
            match pool_state.get_amount_out(amount_in.clone(), token_in, token_out) {
                Ok(result) => {
                    debug!(
                        "Got quote result: amount={}, gas={}",
                        result.amount, result.gas
                    );
                    amounts_out.push(result.amount.to_string());
                    gas_used.push(result.gas.to_u64().unwrap_or(0));
                }
                Err(e) => {
                    let msg = format!("{}", e);
                    if first_error.is_none() {
                        first_error = Some(msg.clone());
                    }
                    debug!("Failed to get quote: {}", msg);
                    continue;
                }
            }
        }

        if !amounts_out.is_empty() {
            pools_with_quotes += 1;
            let pool_name = format!("{:?}", pool_state);
            let pool_name = pool_name
                .split_whitespace()
                .next()
                .unwrap_or("Unknown")
                .to_string();

            debug!("Adding valid quote for pool: {}", pool_name);

            results.push(AmountOutResponse {
                pool: id.clone(),
                pool_name,
                pool_address: pool_addr,
                amounts_out,
                gas_used,
                block_number: current_block,
            });
        }
    }

    info!(
        "Found {} matching pools, {} candidate pools, {} with valid quotes",
        matching_pools, candidate_pools, pools_with_quotes
    );

    if results.is_empty() {
        if matching_pools == 0 {
            let err_msg = format!(
                "No matching pools found for pair {}-{}",
                token_in_address, token_out_address
            );
            warn!("{}", err_msg);
            return Err(err_msg);
        } else {
            match &first_error {
                Some(e) => info!(
                    "Matched {} pools but all quotes failed for pair {}-{}; example error: {}",
                    matching_pools, token_in_address, token_out_address, e
                ),
                None => info!(
                    "Matched {} pools but all quotes failed for pair {}-{}",
                    matching_pools, token_in_address, token_out_address
                ),
            }
            // Supported edge case: return empty results so clients can handle gracefully
            return Ok(results);
        }
    }

    // Sort results by first amount_out (best to worst)
    results.sort_by(|a, b| {
        let a_amount = BigUint::from_str(&a.amounts_out[0]).unwrap_or_default();
        let b_amount = BigUint::from_str(&b.amounts_out[0]).unwrap_or_default();
        b_amount.cmp(&a_amount)
    });

    debug!("Returning {} sorted quotes", results.len());
    // Info log summarizing the returned quotes (top entry shown for brevity)
    let top = &results[0];
    info!(
        "Quote response: total_results={} top_pool={} address={} first_amount_out={} block={}",
        results.len(),
        top.pool_name,
        top.pool_address,
        top.amounts_out.get(0).cloned().unwrap_or_else(|| "0".to_string()),
        top.block_number
    );
    Ok(results)
}
