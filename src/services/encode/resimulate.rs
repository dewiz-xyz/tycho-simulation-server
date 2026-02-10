use std::collections::HashMap;
use std::sync::Arc;

use num_bigint::BigUint;
use num_traits::Zero;
use tokio::task::spawn_blocking;
use tokio::time::sleep;
use tycho_simulation::{
    protocol::models::ProtocolComponent,
    tycho_common::{
        models::{token::Token, Chain},
        simulation::protocol_sim::ProtocolSim,
        Bytes,
    },
};

use crate::models::state::AppState;

use super::allocation::allocate_swaps_by_bps;
use super::model::{
    NormalizedRouteInternal, ResimulatedHopInternal, ResimulatedRouteInternal,
    ResimulatedSegmentInternal, ResimulatedSwapInternal,
};
use super::EncodeError;

pub(super) async fn resimulate_route(
    state: &AppState,
    normalized: &NormalizedRouteInternal,
    chain: Chain,
    request_token_in: &Bytes,
    request_token_out: &Bytes,
) -> Result<ResimulatedRouteInternal, EncodeError> {
    let mut token_cache = TokenCache::new(state);
    let mut pool_cache: HashMap<String, (Arc<dyn ProtocolSim>, Arc<ProtocolComponent>)> =
        HashMap::new();

    struct SegmentSimState {
        next_amount_in: BigUint,
        hops: Vec<Option<ResimulatedHopInternal>>,
    }

    let mut segment_states = normalized
        .segments
        .iter()
        .map(|segment| SegmentSimState {
            next_amount_in: segment.amount_in.clone(),
            hops: (0..segment.hops.len()).map(|_| None).collect(),
        })
        .collect::<Vec<_>>();
    let max_hops = normalized
        .segments
        .iter()
        .map(|segment| segment.hops.len())
        .max()
        .unwrap_or(0);

    // Resimulate in hop-depth order to match build_route_swaps execution order.
    for hop_index in 0..max_hops {
        for (segment_index, segment) in normalized.segments.iter().enumerate() {
            let Some(hop) = segment.hops.get(hop_index) else {
                continue;
            };
            let segment_state = segment_states
                .get_mut(segment_index)
                .ok_or_else(|| EncodeError::internal("Segment state missing after resimulation"))?;
            let hop_amount_in = segment_state.next_amount_in.clone();
            if hop_amount_in.is_zero() {
                return Err(EncodeError::invalid(format!(
                    "segment[{}].hop[{}] amountIn is zero",
                    segment_index, hop_index
                )));
            }

            let allocated_swaps =
                allocate_swaps_by_bps(hop_amount_in.clone(), &hop.swaps, segment_index, hop_index)?;
            let mut swap_results = Vec::with_capacity(allocated_swaps.len());
            let mut hop_expected = BigUint::zero();

            for (swap_index, allocated) in allocated_swaps.into_iter().enumerate() {
                let pool_entry = if let Some(entry) = pool_cache.get(&allocated.pool.component_id) {
                    (Arc::clone(&entry.0), Arc::clone(&entry.1))
                } else {
                    let entry = state
                        .pool_by_id(&allocated.pool.component_id)
                        .await
                        .ok_or_else(|| {
                            EncodeError::not_found(format!(
                                "Pool {} not found",
                                allocated.pool.component_id
                            ))
                        })?;
                    pool_cache.insert(allocated.pool.component_id.clone(), entry.clone());
                    entry
                };

                let sim_token_in = map_swap_token(&allocated.token_in, chain);
                let sim_token_out = map_swap_token(&allocated.token_out, chain);
                let token_in = token_cache.get(&sim_token_in).await?;
                let token_out = token_cache.get(&sim_token_out).await?;

                // Offload pool simulation to the blocking pool (VM pools can be CPU-heavy).
                //
                // Mirror `/simulate` guardrails: hold a semaphore permit inside the blocking task
                // so timeouts/cancellation don't accidentally uncap concurrent CPU-bound work.
                let is_vm = pool_entry.1.protocol_system.starts_with("vm:");
                let permit = if is_vm {
                    state
                        .vm_sim_semaphore()
                        .acquire_owned()
                        .await
                        .map_err(|_| EncodeError::internal("VM pool semaphore closed"))?
                } else {
                    state
                        .native_sim_semaphore()
                        .acquire_owned()
                        .await
                        .map_err(|_| EncodeError::internal("Native pool semaphore closed"))?
                };
                let pool_state = Arc::clone(&pool_entry.0);
                let amount_in_for_sim = allocated.amount_in.clone();
                let pool_timeout = if is_vm {
                    state.pool_timeout_vm()
                } else {
                    state.pool_timeout_native()
                };

                let handle = spawn_blocking(move || {
                    // Hold the permit until the blocking work exits.
                    let _permit = permit;
                    // Move the pool state into the blocking task and return it so the caller can
                    // attach it to the response without an extra clone.
                    let pre_state = pool_state;
                    let result = pre_state.get_amount_out(amount_in_for_sim, &token_in, &token_out);
                    (pre_state, result)
                });
                tokio::pin!(handle);

                let (pre_state, result) = tokio::select! {
                    res = handle.as_mut() => {
                        res.map_err(|join_err| {
                            let reason = if join_err.is_panic() {
                                "panicked"
                            } else if join_err.is_cancelled() {
                                "was cancelled"
                            } else {
                                "failed"
                            };
                            EncodeError::internal(format!(
                                "Pool {} simulation task {}: {}",
                                allocated.pool.component_id, reason, join_err
                            ))
                        })?
                    }
                    _ = sleep(pool_timeout) => {
                        // Best-effort attempt to stop waiting on the blocking task after a pool-level
                        // timeout; this doesn't reliably prevent `spawn_blocking` work from running
                        // once scheduled. The semaphore permit is held inside the closure to cap
                        // concurrent CPU usage even if the task keeps running.
                        handle.as_mut().abort();
                        return Err(EncodeError::simulation(format!(
                            "Pool {} simulation timed out after {}ms",
                            allocated.pool.component_id,
                            pool_timeout.as_millis()
                        )));
                    }
                };
                let result = result.map_err(|err| {
                    EncodeError::simulation(format!(
                        "Pool {} simulation failed: {}",
                        allocated.pool.component_id, err
                    ))
                })?;
                let expected_out = result.amount;
                if expected_out.is_zero() {
                    return Err(EncodeError::simulation(format!(
                        "Pool {} returned zero amountOut",
                        allocated.pool.component_id
                    )));
                }

                // Advance cached pool state so repeated-pool swaps simulate sequentially.
                pool_cache.insert(
                    allocated.pool.component_id.clone(),
                    (Arc::from(result.new_state), Arc::clone(&pool_entry.1)),
                );

                hop_expected += expected_out.clone();

                swap_results.push(ResimulatedSwapInternal {
                    pool: allocated.pool,
                    token_in: allocated.token_in,
                    token_out: allocated.token_out,
                    split_bps: allocated.split_bps,
                    amount_in: allocated.amount_in,
                    expected_amount_out: expected_out,
                    pool_state: pre_state,
                    component: Arc::clone(&pool_entry.1),
                });

                if swap_results.len() - 1 != swap_index {
                    return Err(EncodeError::internal("Swap allocation ordering mismatch"));
                }
            }

            if hop_expected.is_zero() {
                return Err(EncodeError::simulation(format!(
                    "segment[{}].hop[{}] produced zero amountOut",
                    segment_index, hop_index
                )));
            }

            let resim_hop = ResimulatedHopInternal {
                token_in: hop.token_in.clone(),
                token_out: hop.token_out.clone(),
                amount_in: hop_amount_in.clone(),
                expected_amount_out: hop_expected.clone(),
                swaps: swap_results,
            };

            segment_state.hops[hop_index] = Some(resim_hop);
            segment_state.next_amount_in = hop_expected;
        }
    }

    let mut resim_segments = Vec::with_capacity(normalized.segments.len());
    for (segment_index, segment) in normalized.segments.iter().enumerate() {
        let mut resim_hops = Vec::with_capacity(segment.hops.len());
        let segment_state = segment_states
            .get_mut(segment_index)
            .ok_or_else(|| EncodeError::internal("Segment state missing after resimulation"))?;
        for hop_index in 0..segment.hops.len() {
            let resim_hop = segment_state.hops[hop_index]
                .take()
                .ok_or_else(|| EncodeError::internal("Segment hops missing after resimulation"))?;
            resim_hops.push(resim_hop);
        }

        let last_hop = resim_hops
            .last()
            .ok_or_else(|| EncodeError::internal("Segment hops missing after resimulation"))?;

        resim_segments.push(ResimulatedSegmentInternal {
            share_bps: segment.share_bps,
            amount_in: segment.amount_in.clone(),
            expected_amount_out: last_hop.expected_amount_out.clone(),
            hops: resim_hops,
        });
    }

    if request_token_in.is_empty() || request_token_out.is_empty() {
        return Err(EncodeError::internal(
            "Request tokens missing after resimulation",
        ));
    }

    Ok(ResimulatedRouteInternal {
        segments: resim_segments,
    })
}

fn map_swap_token(address: &Bytes, chain: Chain) -> Bytes {
    if *address == chain.native_token().address {
        chain.wrapped_native_token().address
    } else {
        address.clone()
    }
}

struct TokenCache<'a> {
    state: &'a AppState,
    cache: HashMap<Bytes, Token>,
}

impl<'a> TokenCache<'a> {
    fn new(state: &'a AppState) -> Self {
        Self {
            state,
            cache: HashMap::new(),
        }
    }

    async fn get(&mut self, address: &Bytes) -> Result<Token, EncodeError> {
        if let Some(token) = self.cache.get(address) {
            return Ok(token.clone());
        }
        let token = self
            .state
            .tokens
            .ensure(address)
            .await
            .map_err(|err| EncodeError::simulation(format!("Token lookup failed: {}", err)))?
            .ok_or_else(|| EncodeError::invalid("Token not found"))?;
        self.cache.insert(address.clone(), token.clone());
        Ok(token)
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::str::FromStr;
    use std::time::Duration;

    use chrono::NaiveDateTime;
    use tokio::sync::Semaphore;
    use tycho_simulation::protocol::models::{ProtocolComponent, Update};
    use tycho_simulation::tycho_common::models::Chain;

    use super::*;
    use crate::models::state::{StateStore, VmStreamStatus};
    use crate::models::stream_health::StreamHealth;
    use crate::models::tokens::TokenStore;
    use crate::services::encode::model::{
        NormalizedHopInternal, NormalizedSegmentInternal, NormalizedSwapDraftInternal,
    };
    use crate::services::encode::test_support::{
        component_with_tokens, dummy_token, pool_ref, step_multiplier, StepProtocolSim,
    };

    #[derive(Debug, Clone, serde::Deserialize, serde::Serialize)]
    struct SlowProtocolSim {
        sleep_for: Duration,
    }

    #[typetag::serde]
    impl ProtocolSim for SlowProtocolSim {
        fn fee(&self) -> f64 {
            0.0
        }

        fn spot_price(
            &self,
            _base: &Token,
            _quote: &Token,
        ) -> Result<f64, tycho_simulation::tycho_common::simulation::errors::SimulationError>
        {
            Ok(0.0)
        }

        fn get_amount_out(
            &self,
            amount_in: BigUint,
            _token_in: &Token,
            _token_out: &Token,
        ) -> Result<
            tycho_simulation::tycho_common::simulation::protocol_sim::GetAmountOutResult,
            tycho_simulation::tycho_common::simulation::errors::SimulationError,
        > {
            std::thread::sleep(self.sleep_for);
            Ok(
                tycho_simulation::tycho_common::simulation::protocol_sim::GetAmountOutResult::new(
                    amount_in,
                    BigUint::zero(),
                    self.clone_box(),
                ),
            )
        }

        fn get_limits(
            &self,
            _sell_token: Bytes,
            _buy_token: Bytes,
        ) -> Result<
            (BigUint, BigUint),
            tycho_simulation::tycho_common::simulation::errors::SimulationError,
        > {
            Ok((BigUint::zero(), BigUint::zero()))
        }

        fn delta_transition(
            &mut self,
            _delta: tycho_simulation::tycho_common::dto::ProtocolStateDelta,
            _tokens: &HashMap<Bytes, Token>,
            _balances: &tycho_simulation::tycho_common::simulation::protocol_sim::Balances,
        ) -> Result<(), tycho_simulation::tycho_common::simulation::errors::TransitionError<String>>
        {
            Ok(())
        }

        fn clone_box(&self) -> Box<dyn ProtocolSim> {
            Box::new(Self {
                sleep_for: self.sleep_for,
            })
        }

        fn as_any(&self) -> &dyn std::any::Any {
            self
        }

        fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
            self
        }

        fn eq(&self, other: &dyn ProtocolSim) -> bool {
            other
                .as_any()
                .downcast_ref::<SlowProtocolSim>()
                .is_some_and(|rhs| rhs.sleep_for == self.sleep_for)
        }
    }

    #[tokio::test]
    async fn resimulate_route_advances_pool_state_for_repeated_swaps() {
        let token_in = dummy_token("0x0000000000000000000000000000000000000001");
        let token_out = dummy_token("0x0000000000000000000000000000000000000002");
        let mut tokens = HashMap::new();
        tokens.insert(token_in.address.clone(), token_in.clone());
        tokens.insert(token_out.address.clone(), token_out.clone());

        let tokens_store = Arc::new(TokenStore::new(
            tokens,
            "http://localhost".to_string(),
            "test".to_string(),
            Chain::Ethereum,
            Duration::from_millis(10),
        ));
        let native_state_store = Arc::new(StateStore::new(Arc::clone(&tokens_store)));
        let vm_state_store = Arc::new(StateStore::new(Arc::clone(&tokens_store)));

        let component = ProtocolComponent::new(
            Bytes::from_str("0x0000000000000000000000000000000000000009").unwrap(),
            "uniswap_v2".to_string(),
            "uniswap_v2".to_string(),
            Chain::Ethereum,
            vec![token_in.clone(), token_out.clone()],
            Vec::new(),
            HashMap::new(),
            Bytes::default(),
            NaiveDateTime::default(),
        );
        let mut states = HashMap::new();
        states.insert(
            "pool-1".to_string(),
            Box::new(StepProtocolSim { multiplier: 1 }) as Box<dyn ProtocolSim>,
        );
        let mut new_pairs = HashMap::new();
        new_pairs.insert("pool-1".to_string(), component);
        let update = Update::new(1, states, new_pairs);
        native_state_store.apply_update(update).await;

        let app_state = AppState {
            tokens: Arc::clone(&tokens_store),
            native_state_store: Arc::clone(&native_state_store),
            vm_state_store: Arc::clone(&vm_state_store),
            native_stream_health: Arc::new(StreamHealth::new()),
            vm_stream_health: Arc::new(StreamHealth::new()),
            vm_stream: Arc::new(tokio::sync::RwLock::new(VmStreamStatus::default())),
            enable_vm_pools: false,
            readiness_stale: Duration::from_secs(120),
            quote_timeout: Duration::from_millis(10),
            pool_timeout_native: Duration::from_millis(10),
            pool_timeout_vm: Duration::from_millis(10),
            request_timeout: Duration::from_millis(10),
            native_sim_semaphore: Arc::new(Semaphore::new(1)),
            vm_sim_semaphore: Arc::new(Semaphore::new(1)),
            reset_allowance_tokens: Arc::new(HashMap::new()),
            native_sim_concurrency: 1,
            vm_sim_concurrency: 1,
        };

        let normalized = NormalizedRouteInternal {
            segments: vec![NormalizedSegmentInternal {
                share_bps: 10_000,
                amount_in: BigUint::from(10u32),
                hops: vec![NormalizedHopInternal {
                    token_in: token_in.address.clone(),
                    token_out: token_out.address.clone(),
                    swaps: vec![
                        NormalizedSwapDraftInternal {
                            pool: pool_ref("pool-1"),
                            token_in: token_in.address.clone(),
                            token_out: token_out.address.clone(),
                            split_bps: 5000,
                        },
                        NormalizedSwapDraftInternal {
                            pool: pool_ref("pool-1"),
                            token_in: token_in.address.clone(),
                            token_out: token_out.address.clone(),
                            split_bps: 0,
                        },
                    ],
                }],
            }],
        };

        let resimulated = resimulate_route(
            &app_state,
            &normalized,
            Chain::Ethereum,
            &token_in.address,
            &token_out.address,
        )
        .await
        .unwrap();

        let swaps = &resimulated.segments[0].hops[0].swaps;
        assert_eq!(swaps[0].expected_amount_out, BigUint::from(5u32));
        assert_eq!(swaps[1].expected_amount_out, BigUint::from(10u32));
        assert_eq!(step_multiplier(&swaps[0].pool_state), 1);
        assert_eq!(step_multiplier(&swaps[1].pool_state), 2);
    }

    #[tokio::test]
    async fn resimulate_route_orders_by_hop_depth_across_segments() {
        let token_a = dummy_token("0x0000000000000000000000000000000000000001");
        let token_b = dummy_token("0x0000000000000000000000000000000000000002");
        let token_c = dummy_token("0x0000000000000000000000000000000000000003");
        let mut tokens = HashMap::new();
        tokens.insert(token_a.address.clone(), token_a.clone());
        tokens.insert(token_b.address.clone(), token_b.clone());
        tokens.insert(token_c.address.clone(), token_c.clone());

        let tokens_store = Arc::new(TokenStore::new(
            tokens,
            "http://localhost".to_string(),
            "test".to_string(),
            Chain::Ethereum,
            Duration::from_millis(10),
        ));
        let native_state_store = Arc::new(StateStore::new(Arc::clone(&tokens_store)));
        let vm_state_store = Arc::new(StateStore::new(Arc::clone(&tokens_store)));

        let component_a = component_with_tokens(
            "0x0000000000000000000000000000000000000009",
            vec![token_a.clone(), token_b.clone()],
        );
        let component_shared = component_with_tokens(
            "0x000000000000000000000000000000000000000a",
            vec![token_a.clone(), token_b.clone(), token_c.clone()],
        );

        let mut states = HashMap::new();
        states.insert(
            "pool-a".to_string(),
            Box::new(StepProtocolSim { multiplier: 1 }) as Box<dyn ProtocolSim>,
        );
        states.insert(
            "pool-shared".to_string(),
            Box::new(StepProtocolSim { multiplier: 1 }) as Box<dyn ProtocolSim>,
        );
        let mut new_pairs = HashMap::new();
        new_pairs.insert("pool-a".to_string(), component_a);
        new_pairs.insert("pool-shared".to_string(), component_shared);
        let update = Update::new(1, states, new_pairs);
        native_state_store.apply_update(update).await;

        let app_state = AppState {
            tokens: Arc::clone(&tokens_store),
            native_state_store: Arc::clone(&native_state_store),
            vm_state_store: Arc::clone(&vm_state_store),
            native_stream_health: Arc::new(StreamHealth::new()),
            vm_stream_health: Arc::new(StreamHealth::new()),
            vm_stream: Arc::new(tokio::sync::RwLock::new(VmStreamStatus::default())),
            enable_vm_pools: false,
            readiness_stale: Duration::from_secs(120),
            quote_timeout: Duration::from_millis(10),
            pool_timeout_native: Duration::from_millis(10),
            pool_timeout_vm: Duration::from_millis(10),
            request_timeout: Duration::from_millis(10),
            native_sim_semaphore: Arc::new(Semaphore::new(1)),
            vm_sim_semaphore: Arc::new(Semaphore::new(1)),
            reset_allowance_tokens: Arc::new(HashMap::new()),
            native_sim_concurrency: 1,
            vm_sim_concurrency: 1,
        };

        let normalized = NormalizedRouteInternal {
            segments: vec![
                NormalizedSegmentInternal {
                    share_bps: 6_000,
                    amount_in: BigUint::from(100u32),
                    hops: vec![
                        NormalizedHopInternal {
                            token_in: token_a.address.clone(),
                            token_out: token_b.address.clone(),
                            swaps: vec![NormalizedSwapDraftInternal {
                                pool: pool_ref("pool-a"),
                                token_in: token_a.address.clone(),
                                token_out: token_b.address.clone(),
                                split_bps: 0,
                            }],
                        },
                        NormalizedHopInternal {
                            token_in: token_b.address.clone(),
                            token_out: token_c.address.clone(),
                            swaps: vec![NormalizedSwapDraftInternal {
                                pool: pool_ref("pool-shared"),
                                token_in: token_b.address.clone(),
                                token_out: token_c.address.clone(),
                                split_bps: 0,
                            }],
                        },
                    ],
                },
                NormalizedSegmentInternal {
                    share_bps: 4_000,
                    amount_in: BigUint::from(50u32),
                    hops: vec![NormalizedHopInternal {
                        token_in: token_a.address.clone(),
                        token_out: token_b.address.clone(),
                        swaps: vec![NormalizedSwapDraftInternal {
                            pool: pool_ref("pool-shared"),
                            token_in: token_a.address.clone(),
                            token_out: token_b.address.clone(),
                            split_bps: 0,
                        }],
                    }],
                },
            ],
        };

        let resimulated = resimulate_route(
            &app_state,
            &normalized,
            Chain::Ethereum,
            &token_a.address,
            &token_c.address,
        )
        .await
        .unwrap();

        let seg_a_hop1_swap = &resimulated.segments[0].hops[1].swaps[0];
        let seg_b_hop0_swap = &resimulated.segments[1].hops[0].swaps[0];

        assert_eq!(seg_b_hop0_swap.expected_amount_out, BigUint::from(50u32));
        assert_eq!(step_multiplier(&seg_b_hop0_swap.pool_state), 1);
        assert_eq!(seg_a_hop1_swap.expected_amount_out, BigUint::from(200u32));
        assert_eq!(step_multiplier(&seg_a_hop1_swap.pool_state), 2);
    }

    #[tokio::test]
    async fn resimulate_route_times_out_native_pool_simulation() {
        let token_in = dummy_token("0x0000000000000000000000000000000000000001");
        let token_out = dummy_token("0x0000000000000000000000000000000000000002");
        let mut tokens = HashMap::new();
        tokens.insert(token_in.address.clone(), token_in.clone());
        tokens.insert(token_out.address.clone(), token_out.clone());

        let tokens_store = Arc::new(TokenStore::new(
            tokens,
            "http://localhost".to_string(),
            "test".to_string(),
            Chain::Ethereum,
            Duration::from_millis(10),
        ));
        let native_state_store = Arc::new(StateStore::new(Arc::clone(&tokens_store)));
        let vm_state_store = Arc::new(StateStore::new(Arc::clone(&tokens_store)));

        let component = ProtocolComponent::new(
            Bytes::from_str("0x0000000000000000000000000000000000000009").unwrap(),
            "uniswap_v2".to_string(),
            "uniswap_v2".to_string(),
            Chain::Ethereum,
            vec![token_in.clone(), token_out.clone()],
            Vec::new(),
            HashMap::new(),
            Bytes::default(),
            NaiveDateTime::default(),
        );
        let mut states = HashMap::new();
        states.insert(
            "pool-slow".to_string(),
            Box::new(SlowProtocolSim {
                sleep_for: Duration::from_millis(80),
            }) as Box<dyn ProtocolSim>,
        );
        let mut new_pairs = HashMap::new();
        new_pairs.insert("pool-slow".to_string(), component);
        native_state_store
            .apply_update(Update::new(1, states, new_pairs))
            .await;

        let app_state = AppState {
            tokens: Arc::clone(&tokens_store),
            native_state_store: Arc::clone(&native_state_store),
            vm_state_store: Arc::clone(&vm_state_store),
            native_stream_health: Arc::new(StreamHealth::new()),
            vm_stream_health: Arc::new(StreamHealth::new()),
            vm_stream: Arc::new(tokio::sync::RwLock::new(VmStreamStatus::default())),
            enable_vm_pools: false,
            readiness_stale: Duration::from_secs(120),
            quote_timeout: Duration::from_millis(1000),
            pool_timeout_native: Duration::from_millis(10),
            pool_timeout_vm: Duration::from_millis(1000),
            request_timeout: Duration::from_millis(1000),
            native_sim_semaphore: Arc::new(Semaphore::new(1)),
            vm_sim_semaphore: Arc::new(Semaphore::new(1)),
            reset_allowance_tokens: Arc::new(HashMap::new()),
            native_sim_concurrency: 1,
            vm_sim_concurrency: 1,
        };

        let normalized = NormalizedRouteInternal {
            segments: vec![NormalizedSegmentInternal {
                share_bps: 10_000,
                amount_in: BigUint::from(10u32),
                hops: vec![NormalizedHopInternal {
                    token_in: token_in.address.clone(),
                    token_out: token_out.address.clone(),
                    swaps: vec![NormalizedSwapDraftInternal {
                        pool: pool_ref("pool-slow"),
                        token_in: token_in.address.clone(),
                        token_out: token_out.address.clone(),
                        split_bps: 0,
                    }],
                }],
            }],
        };

        let err = match resimulate_route(
            &app_state,
            &normalized,
            Chain::Ethereum,
            &token_in.address,
            &token_out.address,
        )
        .await
        {
            Ok(_) => panic!("Expected pool simulation timeout"),
            Err(err) => err,
        };

        assert_eq!(
            err.kind(),
            crate::services::encode::EncodeErrorKind::Simulation
        );
        assert!(
            err.message().contains("pool-slow") && err.message().contains("timed out"),
            "unexpected error: {}",
            err.message()
        );
    }

    #[tokio::test]
    async fn resimulate_route_times_out_vm_pool_simulation() {
        let token_in = dummy_token("0x0000000000000000000000000000000000000001");
        let token_out = dummy_token("0x0000000000000000000000000000000000000002");
        let mut tokens = HashMap::new();
        tokens.insert(token_in.address.clone(), token_in.clone());
        tokens.insert(token_out.address.clone(), token_out.clone());

        let tokens_store = Arc::new(TokenStore::new(
            tokens,
            "http://localhost".to_string(),
            "test".to_string(),
            Chain::Ethereum,
            Duration::from_millis(10),
        ));
        let native_state_store = Arc::new(StateStore::new(Arc::clone(&tokens_store)));
        let vm_state_store = Arc::new(StateStore::new(Arc::clone(&tokens_store)));

        let component = ProtocolComponent::new(
            Bytes::from_str("0x0000000000000000000000000000000000000009").unwrap(),
            "vm:curve".to_string(),
            "curve_pool".to_string(),
            Chain::Ethereum,
            vec![token_in.clone(), token_out.clone()],
            Vec::new(),
            HashMap::new(),
            Bytes::default(),
            NaiveDateTime::default(),
        );

        let mut states = HashMap::new();
        states.insert(
            "pool-vm-slow".to_string(),
            Box::new(SlowProtocolSim {
                sleep_for: Duration::from_millis(80),
            }) as Box<dyn ProtocolSim>,
        );
        let mut new_pairs = HashMap::new();
        new_pairs.insert("pool-vm-slow".to_string(), component);
        vm_state_store
            .apply_update(Update::new(1, states, new_pairs))
            .await;

        let app_state = AppState {
            tokens: Arc::clone(&tokens_store),
            native_state_store: Arc::clone(&native_state_store),
            vm_state_store: Arc::clone(&vm_state_store),
            native_stream_health: Arc::new(StreamHealth::new()),
            vm_stream_health: Arc::new(StreamHealth::new()),
            vm_stream: Arc::new(tokio::sync::RwLock::new(VmStreamStatus::default())),
            enable_vm_pools: true,
            readiness_stale: Duration::from_secs(120),
            quote_timeout: Duration::from_millis(1000),
            pool_timeout_native: Duration::from_millis(1000),
            pool_timeout_vm: Duration::from_millis(10),
            request_timeout: Duration::from_millis(1000),
            native_sim_semaphore: Arc::new(Semaphore::new(1)),
            vm_sim_semaphore: Arc::new(Semaphore::new(1)),
            reset_allowance_tokens: Arc::new(HashMap::new()),
            native_sim_concurrency: 1,
            vm_sim_concurrency: 1,
        };

        let normalized = NormalizedRouteInternal {
            segments: vec![NormalizedSegmentInternal {
                share_bps: 10_000,
                amount_in: BigUint::from(10u32),
                hops: vec![NormalizedHopInternal {
                    token_in: token_in.address.clone(),
                    token_out: token_out.address.clone(),
                    swaps: vec![NormalizedSwapDraftInternal {
                        pool: pool_ref("pool-vm-slow"),
                        token_in: token_in.address.clone(),
                        token_out: token_out.address.clone(),
                        split_bps: 0,
                    }],
                }],
            }],
        };

        let err = match resimulate_route(
            &app_state,
            &normalized,
            Chain::Ethereum,
            &token_in.address,
            &token_out.address,
        )
        .await
        {
            Ok(_) => panic!("Expected VM pool simulation timeout"),
            Err(err) => err,
        };

        assert_eq!(
            err.kind(),
            crate::services::encode::EncodeErrorKind::Simulation
        );
        assert!(
            err.message().contains("pool-vm-slow") && err.message().contains("timed out"),
            "unexpected error: {}",
            err.message()
        );
    }
}
