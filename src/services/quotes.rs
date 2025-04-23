use num_bigint::BigUint;
use num_traits::cast::ToPrimitive;
use std::str::FromStr;
use tycho_simulation::tycho_core::Bytes;

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

    println!(
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

    let states = state.states.read().await;
    let current_block = *state.current_block.read().await;
    let mut results = Vec::new();
    let mut matching_pools = 0;
    let mut pools_with_quotes = 0;

    for (id, (state, comp)) in states.iter() {
        let pool_tokens: Vec<String> = comp
            .tokens
            .iter()
            .map(|t| {
                t.address
                    .to_string()
                    .trim_start_matches("0x")
                    .to_lowercase()
            })
            .collect();

        if pool_tokens.contains(&token_in_address) && pool_tokens.contains(&token_out_address) {
            matching_pools += 1;
            let mut amounts_out = Vec::new();
            let mut gas_used = Vec::new();

            for amount_in in amounts_in.iter() {
                match state.get_amount_out(amount_in.clone(), token_in, token_out) {
                    Ok(result) => {
                        amounts_out.push(result.amount.to_string());
                        gas_used.push(result.gas.to_u64().unwrap_or(0));
                    }
                    Err(_) => continue,
                }
            }

            if !amounts_out.is_empty() {
                pools_with_quotes += 1;
                let pool_name = format!("{:?}", state);
                let pool_name = pool_name
                    .split_whitespace()
                    .next()
                    .unwrap_or("Unknown")
                    .to_string();

                results.push(AmountOutResponse {
                    pool: id.clone(),
                    pool_name,
                    pool_address: comp.id.to_string(),
                    amounts_out,
                    gas_used,
                    block_number: current_block,
                });
            }
        }
    }

    println!(
        "Found {} matching pools, {} with valid quotes",
        matching_pools, pools_with_quotes
    );

    if results.is_empty() {
        return Err(format!(
            "No pools found for pair {}-{}",
            token_in_address, token_out_address
        ));
    }

    // Sort results by first amount_out (best to worst)
    results.sort_by(|a, b| {
        let a_amount = BigUint::from_str(&a.amounts_out[0]).unwrap_or_default();
        let b_amount = BigUint::from_str(&b.amounts_out[0]).unwrap_or_default();
        b_amount.cmp(&a_amount)
    });

    Ok(results)
}
