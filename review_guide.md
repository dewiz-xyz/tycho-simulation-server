# Review Guide: master...HEAD

## Diff Snapshot
- Range: `master...HEAD`
- Summary: 42 files changed, 2474 insertions(+), 725 deletions(-)
- Theme: migrate the simulator from Ethereum-only assumptions to explicit runtime chain profiles (Ethereum/Base) and chain-aware tooling.
- Highest-risk surfaces: runtime profile selection, stream protocol registration, gas-price startup gating, and readiness/script contract changes.

## Suggested Human Review Order
1. **Runtime chain profile + startup path** (`src/config/mod.rs`, `src/main.rs`, `src/services/stream_builder.rs`, `src/services/gas_price.rs`).
2. **Encode semantics under runtime chain constraints** (`src/services/encode/**`, `tests/integration/encode_route.rs`).
3. **Operational behavior and readiness tooling** (`scripts/run_suite.sh`, `scripts/wait_ready.sh`, `scripts/presets.py`, supporting scripts).
4. **Integration safety nets** (`tests/integration/startup_gas_price_task.rs`, other integration fixture updates).
5. **Skill/docs/infra propagation** (`skills/**`, `README.md`, `STRESS_TEST_README.md`, `lib/**`).

## Risk Register (Prioritized)
1. **Chain-profile misconfiguration can select wrong protocol universe** (runtime startup + stream builder).
2. **RPC mismatch handling can silently disable gas-in-sell reporting if endpoint chain diverges** (startup task path).
3. **Encode route validation now hard-binds request `chainId` to server runtime chain**; clients relying on older behavior may break.
4. **Effective VM enablement differs from requested VM config on chains without VM protocols**; operator assumptions may drift from runtime reality.
5. **Script chain-id resolution failures (`--chain-id`/`CHAIN_ID`) can break CI or local runbooks unexpectedly.**
6. **Readiness checks now enforce JSON contract (`chain_id`, vm conditions) and can fail on format/field drift.**
7. **Skill mirror scripts/docs must stay in parity with repo scripts to avoid debugging with stale instructions.**
8. **Infra CHAIN_ID wiring is now mandatory; missing env propagation blocks startup in deployed tasks.**

## Hunk-by-Hunk Guide (Ordered)

### Phase 1: Runtime chain profile and startup safety

## src/config/mod.rs
Change summary: Introduces explicit per-chain runtime profiles (Ethereum/Base), protocol lists, and reset-allowance token maps resolved from required `CHAIN_ID`.
Risk level: High
Review focus: Verify supported chain IDs, profile correctness, and panic-on-unsupported behavior during startup.
Hunk context notes: This file now controls core runtime behavior selection; mistakes here can silently route the service to wrong protocol feeds or disable expected features.
````diff
diff --git a/src/config/mod.rs b/src/config/mod.rs
index d11ce5c..8a419aa 100644
--- a/src/config/mod.rs
+++ b/src/config/mod.rs
@@ -2,16 +2,88 @@ use std::collections::{HashMap, HashSet};
 use std::str::FromStr;
 use std::sync::Arc;
 
-use tycho_simulation::tycho_common::Bytes;
+use tycho_simulation::tycho_common::{models::Chain, Bytes};
 
 mod logging;
 mod memory;
 pub use logging::init_logging;
 pub use memory::MemoryConfig;
 
+/// Per-chain runtime profile resolved from `CHAIN_ID`.
+#[derive(Clone, Debug)]
+pub struct ChainProfile {
+    pub chain: Chain,
+    pub native_protocols: Vec<String>,
+    pub vm_protocols: Vec<String>,
+    /// Protocols allowed to swap with the native token (e.g. rocketpool on Ethereum).
+    pub native_token_protocol_allowlist: Vec<String>,
+    pub reset_allowance_tokens: HashMap<u64, HashSet<Bytes>>,
+}
+
+fn ethereum_profile() -> ChainProfile {
+    let mut reset_tokens = HashMap::new();
+    let mut mainnet = HashSet::new();
+    mainnet.insert(parse_address(ETHEREUM_USDT));
+    reset_tokens.insert(1, mainnet);
+
+    ChainProfile {
+        chain: Chain::Ethereum,
+        native_protocols: vec![
+            "uniswap_v2".into(),
+            "sushiswap_v2".into(),
+            "pancakeswap_v2".into(),
+            "uniswap_v3".into(),
+            "pancakeswap_v3".into(),
+            "uniswap_v4".into(),
+            "ekubo_v2".into(),
+            "fluid_v1".into(),
+            "ekubo_v3".into(),
+        ],
+        vm_protocols: vec![
+            "vm:curve".into(),
+            "vm:balancer_v2".into(),
+            "vm:maverick_v2".into(),
+        ],
+        native_token_protocol_allowlist: vec!["rocketpool".into()],
+        reset_allowance_tokens: reset_tokens,
+    }
+}
+
+fn base_profile() -> ChainProfile {
+    ChainProfile {
+        chain: Chain::Base,
+        native_protocols: vec![
+            "uniswap_v2".into(),
+            "uniswap_v3".into(),
+            "uniswap_v4".into(),
+            "pancakeswap_v3".into(),
+        ],
+        vm_protocols: vec![],
+        native_token_protocol_allowlist: vec![],
+        reset_allowance_tokens: HashMap::new(),
+    }
+}
+
+fn resolve_chain_profile(chain_id: u64) -> ChainProfile {
+    match chain_id {
+        1 => ethereum_profile(),
+        8453 => base_profile(),
+        other => panic!(
+            "Unsupported CHAIN_ID={}: supported values are 1 (Ethereum), 8453 (Base)",
+            other
+        ),
+    }
+}
+
 pub fn load_config() -> AppConfig {
     dotenv::dotenv().ok();
 
+    let chain_id: u64 = std::env::var("CHAIN_ID")
+        .expect("CHAIN_ID must be set (supported: 1 for Ethereum, 8453 for Base)")
+        .parse()
+        .expect("CHAIN_ID must be a valid u64");
+    let chain_profile = resolve_chain_profile(chain_id);
+
     let tycho_url =
         std::env::var("TYCHO_URL").unwrap_or_else(|_| "tycho-beta.propellerheads.xyz".to_string());
     let api_key = std::env::var("TYCHO_API_KEY").expect("TYCHO_API_KEY must be set");
@@ -100,7 +172,7 @@ pub fn load_config() -> AppConfig {
         .parse()
         .expect("Invalid ENABLE_VM_POOLS");
 
-    let reset_allowance_tokens = Arc::new(default_reset_allowance_tokens());
+    let reset_allowance_tokens = Arc::new(chain_profile.reset_allowance_tokens.clone());
 
     let cpu_count = std::thread::available_parallelism()
         .map(|value| value.get())
@@ -201,6 +273,7 @@ pub fn load_config() -> AppConfig {
     let memory = MemoryConfig::from_env();
 
     AppConfig {
+        chain_profile,
         tycho_url,
         api_key,
         rpc_url,
@@ -235,6 +308,7 @@ pub fn load_config() -> AppConfig {
 
 #[derive(Clone)]
 pub struct AppConfig {
+    pub chain_profile: ChainProfile,
     pub tycho_url: String,
     pub api_key: String,
     pub rpc_url: Option<String>,
@@ -266,18 +340,8 @@ pub struct AppConfig {
     pub memory: MemoryConfig,
 }
 
-const ETHEREUM_CHAIN_ID: u64 = 1;
 const ETHEREUM_USDT: &str = "0xdAC17F958D2ee523a2206206994597C13D831ec7";
 
-fn default_reset_allowance_tokens() -> HashMap<u64, HashSet<Bytes>> {
-    // Tokens that require approve(0) before approve(amount).
-    let mut tokens = HashMap::new();
-    let mut mainnet = HashSet::new();
-    mainnet.insert(parse_address(ETHEREUM_USDT));
-    tokens.insert(ETHEREUM_CHAIN_ID, mainnet);
-    tokens
-}
-
 fn parse_address(value: &str) -> Bytes {
     let bytes = Bytes::from_str(value).expect("reset_allowance_tokens address is invalid");
     assert!(
@@ -286,3 +350,43 @@ fn parse_address(value: &str) -> Bytes {
     );
     bytes
 }
+
+#[cfg(test)]
+mod tests {
+    use super::*;
+
+    #[test]
+    fn resolve_ethereum_profile() {
+        let profile = resolve_chain_profile(1);
+        assert_eq!(profile.chain, Chain::Ethereum);
+        assert!(profile.native_protocols.contains(&"uniswap_v2".to_string()));
+        assert!(profile.native_protocols.contains(&"uniswap_v3".to_string()));
+        assert!(profile.vm_protocols.contains(&"vm:curve".to_string()));
+        assert!(profile
+            .native_token_protocol_allowlist
+            .contains(&"rocketpool".to_string()));
+        assert!(profile.reset_allowance_tokens.contains_key(&1));
+    }
+
+    #[test]
+    fn resolve_base_profile() {
+        let profile = resolve_chain_profile(8453);
+        assert_eq!(profile.chain, Chain::Base);
+        assert!(profile.native_protocols.contains(&"uniswap_v2".to_string()));
+        assert!(profile.native_protocols.contains(&"uniswap_v3".to_string()));
+        assert!(profile.native_protocols.contains(&"uniswap_v4".to_string()));
+        assert!(profile
+            .native_protocols
+            .contains(&"pancakeswap_v3".to_string()));
+        assert_eq!(profile.native_protocols.len(), 4);
+        assert!(profile.vm_protocols.is_empty());
+        assert!(profile.native_token_protocol_allowlist.is_empty());
+        assert!(profile.reset_allowance_tokens.is_empty());
+    }
+
+    #[test]
+    #[should_panic(expected = "Unsupported CHAIN_ID=999")]
+    fn resolve_unsupported_chain_panics() {
+        resolve_chain_profile(999);
+    }
+}
````

## src/main.rs
Change summary: Threads chain profile data through service bootstrap, stream creation, VM enablement, and gas-price startup task creation.
Risk level: High
Review focus: Check that effective VM enablement and chain propagation are consistent with `/status` semantics and startup logs.
Hunk context notes: The startup path now has chain-dependent branches; a mismatch can produce healthy-looking startup logs with incorrect runtime capabilities.
````diff
diff --git a/src/main.rs b/src/main.rs
index 87f43be..48c6e88 100644
--- a/src/main.rs
+++ b/src/main.rs
@@ -4,7 +4,7 @@ use std::time::Duration;
 use tokio::sync::Semaphore;
 use tracing::{debug, error, info};
 
-use tycho_simulation::{tycho_common::models::Chain, utils::load_all_tokens};
+use tycho_simulation::utils::load_all_tokens;
 
 use tycho_simulation_server::api::create_router;
 use tycho_simulation_server::config::{init_logging, load_config};
@@ -15,7 +15,7 @@ use tycho_simulation_server::memory::maybe_log_memory_snapshot;
 use tycho_simulation_server::models::state::{AppState, StateStore, VmStreamStatus};
 use tycho_simulation_server::models::stream_health::StreamHealth;
 use tycho_simulation_server::models::tokens::TokenStore;
-use tycho_simulation_server::services::gas_price::run_native_gas_price_refresh_loop;
+use tycho_simulation_server::services::gas_price::spawn_gas_price_startup_task;
 use tycho_simulation_server::services::stream_builder::{build_native_stream, build_vm_stream};
 
 #[global_allocator]
@@ -28,7 +28,12 @@ async fn main() -> anyhow::Result<()> {
 
     // Load configuration
     let config = load_config();
-    info!("Initializing price service...");
+    let chain = config.chain_profile.chain;
+    info!(
+        chain_id = chain.id(),
+        chain = %chain,
+        "Initializing price service..."
+    );
 
     info!(
         event = "memory_config",
@@ -62,7 +67,7 @@ async fn main() -> anyhow::Result<()> {
         false,
         Some(&config.api_key),
         true,
-        Chain::Ethereum,
+        chain,
         Some(10),
         None,
     )
@@ -75,7 +80,7 @@ async fn main() -> anyhow::Result<()> {
         all_tokens,
         config.tycho_url.clone(),
         config.api_key.clone(),
-        Chain::Ethereum,
+        chain,
         Duration::from_millis(config.token_refresh_timeout_ms),
     ));
     let native_state_store = Arc::new(StateStore::new(Arc::clone(&tokens)));
@@ -94,7 +99,15 @@ async fn main() -> anyhow::Result<()> {
     let native_sim_concurrency = config.global_native_sim_concurrency;
     let vm_sim_concurrency = config.global_vm_sim_concurrency;
     let readiness_stale = Duration::from_secs(config.readiness_stale_secs);
+    // VM is only effective when enabled AND the chain profile defines VM protocols.
+    let effective_vm_enabled =
+        config.enable_vm_pools && !config.chain_profile.vm_protocols.is_empty();
+
     let app_state = AppState {
+        chain,
+        native_token_protocol_allowlist: Arc::new(
+            config.chain_profile.native_token_protocol_allowlist.clone(),
+        ),
         tokens: Arc::clone(&tokens),
         native_state_store: Arc::clone(&native_state_store),
         vm_state_store: Arc::clone(&vm_state_store),
@@ -103,7 +116,7 @@ async fn main() -> anyhow::Result<()> {
         vm_stream: Arc::clone(&vm_stream),
         latest_native_gas_price_wei: Arc::new(tokio::sync::RwLock::new(None)),
         native_gas_price_reporting_enabled: Arc::new(tokio::sync::RwLock::new(false)),
-        enable_vm_pools: config.enable_vm_pools,
+        enable_vm_pools: effective_vm_enabled,
         readiness_stale,
         quote_timeout,
         pool_timeout_native,
@@ -119,7 +132,8 @@ async fn main() -> anyhow::Result<()> {
     info!(
         native_sim_concurrency,
         vm_sim_concurrency,
-        enable_vm_pools = config.enable_vm_pools,
+        enable_vm_pools = effective_vm_enabled,
+        requested_vm_pools = config.enable_vm_pools,
         "Initialized simulation concurrency limits"
     );
 
@@ -137,24 +151,16 @@ async fn main() -> anyhow::Result<()> {
     };
 
     if let Some(rpc_url) = config.rpc_url.clone() {
-        let app_state_bg = app_state.clone();
         let refresh_interval = Duration::from_millis(config.gas_price_refresh_interval_ms);
         let failure_tolerance = config.gas_price_failure_tolerance;
-
-        tokio::spawn(async move {
-            info!(
-                refresh_interval_ms = refresh_interval.as_millis() as u64,
-                failure_tolerance, "Starting native gas price refresh loop"
-            );
-            run_native_gas_price_refresh_loop(
-                app_state_bg,
-                rpc_url,
-                refresh_interval,
-                failure_tolerance,
-                reqwest::Client::new(),
-            )
-            .await;
-        });
+        let _startup_task = spawn_gas_price_startup_task(
+            app_state.clone(),
+            chain,
+            rpc_url,
+            refresh_interval,
+            failure_tolerance,
+            reqwest::Client::new(),
+        );
     } else {
         info!("RPC_URL is not configured; gas-in-sell reporting remains disabled");
     }
@@ -170,6 +176,7 @@ async fn main() -> anyhow::Result<()> {
         let api_key = cfg.api_key.clone();
         let tvl_threshold = cfg.tvl_threshold;
         let tvl_keep_threshold = cfg.tvl_keep_threshold;
+        let native_protocols = cfg.chain_profile.native_protocols.clone();
         tokio::spawn(async move {
             info!("Starting native protocol stream supervisor...");
             supervise_native_stream(
@@ -177,6 +184,7 @@ async fn main() -> anyhow::Result<()> {
                     let tokens = Arc::clone(&tokens_bg);
                     let tycho_url = tycho_url.clone();
                     let api_key = api_key.clone();
+                    let protocols = native_protocols.clone();
                     async move {
                         build_native_stream(
                             &tycho_url,
@@ -184,6 +192,8 @@ async fn main() -> anyhow::Result<()> {
                             tvl_threshold,
                             tvl_keep_threshold,
                             tokens,
+                            chain,
+                            &protocols,
                         )
                         .await
                     }
@@ -197,7 +207,7 @@ async fn main() -> anyhow::Result<()> {
         debug!("Native stream supervisor task spawned");
     }
 
-    if config.enable_vm_pools {
+    if effective_vm_enabled {
         let vm_supervisor_cfg = supervisor_cfg.clone();
         let cfg = config.clone();
         let tokens_bg = Arc::clone(&tokens);
@@ -212,6 +222,7 @@ async fn main() -> anyhow::Result<()> {
         let vm_sim_concurrency =
             u32::try_from(vm_sim_concurrency).expect("VM simulation concurrency exceeds u32 range");
 
+        let vm_protocols = cfg.chain_profile.vm_protocols.clone();
         tokio::spawn(async move {
             info!("Starting VM protocol stream supervisor...");
             supervise_vm_stream(
@@ -219,6 +230,7 @@ async fn main() -> anyhow::Result<()> {
                     let tokens = Arc::clone(&tokens_bg);
                     let tycho_url = tycho_url.clone();
                     let api_key = api_key.clone();
+                    let protocols = vm_protocols.clone();
                     async move {
                         build_vm_stream(
                             &tycho_url,
@@ -226,6 +238,8 @@ async fn main() -> anyhow::Result<()> {
                             tvl_threshold,
                             tvl_keep_threshold,
                             tokens,
+                            chain,
+                            &protocols,
                         )
                         .await
                     }
@@ -242,8 +256,13 @@ async fn main() -> anyhow::Result<()> {
             .await;
         });
         debug!("VM stream supervisor task spawned");
-    } else {
+    } else if !config.enable_vm_pools {
         info!("VM pool feeds disabled");
+    } else {
+        info!(
+            chain = %chain,
+            "VM pool feeds enabled but no VM protocols configured for this chain; skipping VM stream"
+        );
     }
 
     // Create router and start server
````

## src/models/state.rs
Change summary: Applies chain-profile compatibility updates and fixture wiring needed by the runtime/profile refactor.
Risk level: Medium
Review focus: Verify behavior parity and ensure new `AppState` fields are consistently initialized in tests and helpers.
Hunk context notes: Most changes here are collateral updates to preserve compile/runtime consistency after introducing runtime chain context.
````diff
diff --git a/src/models/state.rs b/src/models/state.rs
index f1433d8..bc73976 100644
--- a/src/models/state.rs
+++ b/src/models/state.rs
@@ -7,7 +7,9 @@ use tokio::sync::{watch, RwLock, Semaphore};
 use tokio::time::Instant;
 use tycho_simulation::{
     protocol::models::{ProtocolComponent, Update},
-    tycho_common::{models::token::Token, simulation::protocol_sim::ProtocolSim, Bytes},
+    tycho_common::{
+        models::token::Token, models::Chain, simulation::protocol_sim::ProtocolSim, Bytes,
+    },
 };
 
 use super::{protocol::ProtocolKind, stream_health::StreamHealth, tokens::TokenStore};
@@ -16,6 +18,8 @@ const UPDATE_ANOMALY_SAMPLE_CAP: usize = 6;
 
 #[derive(Clone)]
 pub struct AppState {
+    pub chain: Chain,
+    pub native_token_protocol_allowlist: Arc<Vec<String>>,
     pub tokens: Arc<TokenStore>,
     pub native_state_store: Arc<StateStore>,
     pub vm_state_store: Arc<StateStore>,
@@ -1071,6 +1075,8 @@ mod tests {
             .await;
 
         let app_state = AppState {
+            chain: Chain::Ethereum,
+            native_token_protocol_allowlist: Arc::new(vec!["rocketpool".to_string()]),
             tokens: Arc::clone(&token_store),
             native_state_store: Arc::clone(&native_store),
             vm_state_store: Arc::clone(&vm_store),
@@ -1099,6 +1105,8 @@ mod tests {
     #[tokio::test]
     async fn app_state_native_gas_price_defaults_to_none() {
         let app_state = AppState {
+            chain: Chain::Ethereum,
+            native_token_protocol_allowlist: Arc::new(vec!["rocketpool".to_string()]),
             tokens: Arc::new(TokenStore::new(
                 HashMap::new(),
                 "http://localhost".to_string(),
@@ -1144,6 +1152,8 @@ mod tests {
     #[tokio::test]
     async fn app_state_native_gas_price_updates() {
         let app_state = AppState {
+            chain: Chain::Ethereum,
+            native_token_protocol_allowlist: Arc::new(vec!["rocketpool".to_string()]),
             tokens: Arc::new(TokenStore::new(
                 HashMap::new(),
                 "http://localhost".to_string(),
@@ -1190,6 +1200,8 @@ mod tests {
     #[tokio::test]
     async fn app_state_effective_native_gas_price_for_quotes_serializes_with_disable_transition() {
         let app_state = AppState {
+            chain: Chain::Ethereum,
+            native_token_protocol_allowlist: Arc::new(vec!["rocketpool".to_string()]),
             tokens: Arc::new(TokenStore::new(
                 HashMap::new(),
                 "http://localhost".to_string(),
````

## src/services/stream_builder.rs
Change summary: Switches stream protocol registration from hardcoded exchanges to chain-profile-driven protocol lists with explicit unknown-protocol errors.
Risk level: High
Review focus: Validate mapping between protocol identifiers and exchange builders for both native and VM flows.
Hunk context notes: This is the runtime bridge from config profiles to actual protocol subscriptions; wrong mapping leads to missing or broken liquidity feeds.
````diff
diff --git a/src/services/stream_builder.rs b/src/services/stream_builder.rs
index 4e5d890..dcf903f 100644
--- a/src/services/stream_builder.rs
+++ b/src/services/stream_builder.rs
@@ -1,6 +1,6 @@
 use std::sync::Arc;
 
-use anyhow::Result;
+use anyhow::{bail, Result};
 use futures::StreamExt;
 use tracing::info;
 use tycho_simulation::{
@@ -38,6 +38,8 @@ pub async fn build_native_stream(
     tvl_add_threshold: f64,
     tvl_keep_threshold: f64,
     tokens: Arc<TokenStore>,
+    chain: Chain,
+    protocols: &[String],
 ) -> Result<
     impl futures::Stream<
             Item = Result<
@@ -53,22 +55,12 @@ pub async fn build_native_stream(
         tvl_add_threshold,
         tvl_keep_threshold,
         decode_skip_state_failures(StreamDecodePolicy::Native),
+        chain,
     );
 
-    builder = builder.exchange::<UniswapV2State>("uniswap_v2", tvl_filter.clone(), None);
-    builder = builder.exchange::<UniswapV2State>("sushiswap_v2", tvl_filter.clone(), None);
-    builder = builder.exchange::<PancakeswapV2State>("pancakeswap_v2", tvl_filter.clone(), None);
-    builder = builder.exchange::<UniswapV3State>("uniswap_v3", tvl_filter.clone(), None);
-    builder = builder.exchange::<UniswapV3State>("pancakeswap_v3", tvl_filter.clone(), None);
-    builder = builder.exchange::<UniswapV4State>("uniswap_v4", tvl_filter.clone(), None);
-    builder = builder.exchange::<EkuboState>("ekubo_v2", tvl_filter.clone(), None);
-    builder = builder.exchange::<FluidV1>(
-        "fluid_v1",
-        tvl_filter.clone(),
-        Some(fluid_v1_paused_pools_filter),
-    );
-    // builder = builder.exchange::<RocketpoolState>("rocketpool", tvl_filter.clone(), None);
-    builder = builder.exchange::<EkuboV3State>("ekubo_v3", tvl_filter.clone(), None);
+    for protocol in protocols {
+        builder = register_native_protocol(builder, protocol, &tvl_filter)?;
+    }
 
     let snapshot = tokens.snapshot().await;
     let stream = builder.set_tokens(snapshot).await.build().await?;
@@ -78,12 +70,41 @@ pub async fn build_native_stream(
     }))
 }
 
+fn register_native_protocol(
+    builder: ProtocolStreamBuilder,
+    protocol: &str,
+    tvl_filter: &ComponentFilter,
+) -> Result<ProtocolStreamBuilder> {
+    match protocol {
+        "uniswap_v2" | "sushiswap_v2" => {
+            Ok(builder.exchange::<UniswapV2State>(protocol, tvl_filter.clone(), None))
+        }
+        "pancakeswap_v2" => {
+            Ok(builder.exchange::<PancakeswapV2State>(protocol, tvl_filter.clone(), None))
+        }
+        "uniswap_v3" | "pancakeswap_v3" => {
+            Ok(builder.exchange::<UniswapV3State>(protocol, tvl_filter.clone(), None))
+        }
+        "uniswap_v4" => Ok(builder.exchange::<UniswapV4State>(protocol, tvl_filter.clone(), None)),
+        "ekubo_v2" => Ok(builder.exchange::<EkuboState>(protocol, tvl_filter.clone(), None)),
+        "fluid_v1" => Ok(builder.exchange::<FluidV1>(
+            protocol,
+            tvl_filter.clone(),
+            Some(fluid_v1_paused_pools_filter),
+        )),
+        "ekubo_v3" => Ok(builder.exchange::<EkuboV3State>(protocol, tvl_filter.clone(), None)),
+        other => bail!("Unknown native protocol in chain profile: {}", other),
+    }
+}
+
 pub async fn build_vm_stream(
     tycho_url: &str,
     api_key: &str,
     tvl_add_threshold: f64,
     tvl_keep_threshold: f64,
     tokens: Arc<TokenStore>,
+    chain: Chain,
+    protocols: &[String],
 ) -> Result<
     impl futures::Stream<
             Item = Result<
@@ -99,20 +120,12 @@ pub async fn build_vm_stream(
         tvl_add_threshold,
         tvl_keep_threshold,
         decode_skip_state_failures(StreamDecodePolicy::Vm),
+        chain,
     );
 
-    builder = builder.exchange::<EVMPoolState<PreCachedDB>>(
-        "vm:curve",
-        tvl_filter.clone(),
-        Some(curve_pool_filter),
-    );
-    builder = builder.exchange::<EVMPoolState<PreCachedDB>>(
-        "vm:balancer_v2",
-        tvl_filter.clone(),
-        Some(balancer_v2_pool_filter),
-    );
-    builder =
-        builder.exchange::<EVMPoolState<PreCachedDB>>("vm:maverick_v2", tvl_filter.clone(), None);
+    for protocol in protocols {
+        builder = register_vm_protocol(builder, protocol, &tvl_filter)?;
+    }
 
     let snapshot = tokens.snapshot().await;
     let stream = builder.set_tokens(snapshot).await.build().await?;
@@ -122,12 +135,36 @@ pub async fn build_vm_stream(
     }))
 }
 
+fn register_vm_protocol(
+    builder: ProtocolStreamBuilder,
+    protocol: &str,
+    tvl_filter: &ComponentFilter,
+) -> Result<ProtocolStreamBuilder> {
+    match protocol {
+        "vm:curve" => Ok(builder.exchange::<EVMPoolState<PreCachedDB>>(
+            protocol,
+            tvl_filter.clone(),
+            Some(curve_pool_filter),
+        )),
+        "vm:balancer_v2" => Ok(builder.exchange::<EVMPoolState<PreCachedDB>>(
+            protocol,
+            tvl_filter.clone(),
+            Some(balancer_v2_pool_filter),
+        )),
+        "vm:maverick_v2" => {
+            Ok(builder.exchange::<EVMPoolState<PreCachedDB>>(protocol, tvl_filter.clone(), None))
+        }
+        other => bail!("Unknown VM protocol in chain profile: {}", other),
+    }
+}
+
 fn base_builder(
     tycho_url: &str,
     api_key: &str,
     tvl_add_threshold: f64,
     tvl_keep_threshold: f64,
     skip_state_decode_failures: bool,
+    chain: Chain,
 ) -> (ProtocolStreamBuilder, ComponentFilter) {
     let add_tvl = tvl_add_threshold;
     let keep_tvl = tvl_keep_threshold.min(add_tvl);
@@ -137,7 +174,7 @@ fn base_builder(
     );
     let tvl_filter = ComponentFilter::with_tvl_range(keep_tvl, add_tvl);
 
-    let builder = ProtocolStreamBuilder::new(tycho_url, Chain::Ethereum)
+    let builder = ProtocolStreamBuilder::new(tycho_url, chain)
         .latency_buffer(15)
         .auth_key(Some(api_key.to_string()))
         .skip_state_decode_failures(skip_state_decode_failures);
@@ -154,7 +191,13 @@ fn decode_skip_state_failures(policy: StreamDecodePolicy) -> bool {
 
 #[cfg(test)]
 mod tests {
-    use super::{decode_skip_state_failures, StreamDecodePolicy};
+    use super::{
+        decode_skip_state_failures, register_native_protocol, register_vm_protocol,
+        StreamDecodePolicy,
+    };
+    use tycho_simulation::{
+        evm::stream::ProtocolStreamBuilder, tycho_client::feed::component_tracker::ComponentFilter,
+    };
 
     #[test]
     fn native_stream_keeps_decode_skip_enabled() {
@@ -165,4 +208,34 @@ mod tests {
     fn vm_stream_keeps_decode_skip_enabled() {
         assert!(decode_skip_state_failures(StreamDecodePolicy::Vm));
     }
+
+    #[test]
+    fn unknown_native_protocol_returns_error() {
+        let builder = ProtocolStreamBuilder::new(
+            "localhost",
+            tycho_simulation::tycho_common::models::Chain::Ethereum,
+        );
+        let filter = ComponentFilter::with_tvl_range(0.0, 1.0);
+        let result = register_native_protocol(builder, "unknown_protocol", &filter);
+        assert!(result.is_err());
+        let err = result
+            .err()
+            .expect("expected error for unknown native protocol");
+        assert!(err.to_string().contains("Unknown native protocol"));
+    }
+
+    #[test]
+    fn unknown_vm_protocol_returns_error() {
+        let builder = ProtocolStreamBuilder::new(
+            "localhost",
+            tycho_simulation::tycho_common::models::Chain::Ethereum,
+        );
+        let filter = ComponentFilter::with_tvl_range(0.0, 1.0);
+        let result = register_vm_protocol(builder, "vm:unknown", &filter);
+        assert!(result.is_err());
+        let err = result
+            .err()
+            .expect("expected error for unknown VM protocol");
+        assert!(err.to_string().contains("Unknown VM protocol"));
+    }
 }
````

## src/services/gas_price.rs
Change summary: Adds RPC `eth_chainId` validation and startup gating before enabling gas-in-sell refresh loop, with retry-on-transient-error behavior.
Risk level: High
Review focus: Confirm mismatch handling, retry behavior, and that reporting remains disabled when chain validation fails.
Hunk context notes: Gas reporting correctness now depends on chain alignment between runtime and RPC endpoint, which is a safety-critical invariant.
````diff
diff --git a/src/services/gas_price.rs b/src/services/gas_price.rs
index 368d4ff..2b0f536 100644
--- a/src/services/gas_price.rs
+++ b/src/services/gas_price.rs
@@ -4,6 +4,7 @@ use serde::{Deserialize, Serialize};
 use std::time::Duration;
 use tokio::time::{interval, MissedTickBehavior};
 use tracing::{error, info, warn};
+use tycho_simulation::tycho_common::models::Chain;
 
 use crate::models::state::AppState;
 
@@ -25,10 +26,58 @@ struct JsonRpcResponse {
 }
 
 pub async fn fetch_eth_gas_price_wei(rpc_url: &str, client: &Client) -> Result<u128> {
+    fetch_rpc_hex_value(rpc_url, "eth_gasPrice", client).await
+}
+
+pub async fn fetch_rpc_chain_id(rpc_url: &str, client: &Client) -> Result<u64> {
+    let chain_id = fetch_rpc_hex_value(rpc_url, "eth_chainId", client).await?;
+    u64::try_from(chain_id)
+        .with_context(|| format!("eth_chainId result {} exceeds u64 range", chain_id))
+}
+
+pub fn ensure_rpc_chain_matches(expected_chain: Chain, rpc_chain_id: u64) -> Result<()> {
+    let expected_chain_id = expected_chain.id();
+    if rpc_chain_id != expected_chain_id {
+        return Err(anyhow!(
+            "RPC chain mismatch: expected {}, got {}",
+            expected_chain_id,
+            rpc_chain_id
+        ));
+    }
+    Ok(())
+}
+
+pub async fn wait_for_rpc_chain_match(
+    rpc_url: &str,
+    expected_chain: Chain,
+    retry_interval: Duration,
+    client: &Client,
+) -> Result<u64> {
+    loop {
+        match fetch_rpc_chain_id(rpc_url, client).await {
+            Ok(rpc_chain_id) => {
+                ensure_rpc_chain_matches(expected_chain, rpc_chain_id)?;
+                return Ok(rpc_chain_id);
+            }
+            Err(error) => {
+                // Keep trying when the endpoint is flaky during startup.
+                warn!(
+                    %error,
+                    expected_chain_id = expected_chain.id(),
+                    retry_interval_ms = retry_interval.as_millis() as u64,
+                    "Failed to read eth_chainId from RPC_URL; retrying chain validation"
+                );
+                tokio::time::sleep(retry_interval).await;
+            }
+        }
+    }
+}
+
+async fn fetch_rpc_hex_value(rpc_url: &str, method: &str, client: &Client) -> Result<u128> {
     let request = JsonRpcRequest {
         jsonrpc: "2.0",
         id: 1,
-        method: "eth_gasPrice",
+        method,
         params: [],
     };
 
@@ -38,26 +87,26 @@ pub async fn fetch_eth_gas_price_wei(rpc_url: &str, client: &Client) -> Result<u
         .json(&request)
         .send()
         .await
-        .map_err(|err| anyhow!("failed to call eth_gasPrice: {}", err.without_url()))?;
+        .map_err(|err| anyhow!("failed to call {}: {}", method, err.without_url()))?;
 
     if !response.status().is_success() {
         let status = response.status();
         let body = response.text().await.unwrap_or_default();
-        return Err(anyhow!("eth_gasPrice returned HTTP {}: {}", status, body));
+        return Err(anyhow!("{} returned HTTP {}: {}", method, status, body));
     }
 
     let body: JsonRpcResponse = response
         .json()
         .await
-        .context("failed to decode eth_gasPrice response")?;
+        .with_context(|| format!("failed to decode {} response", method))?;
 
     if let Some(error) = body.error {
-        return Err(anyhow!("eth_gasPrice returned rpc error: {}", error));
+        return Err(anyhow!("{} returned rpc error: {}", method, error));
     }
 
     let value = body
         .result
-        .ok_or_else(|| anyhow!("eth_gasPrice response missing result field"))?;
+        .ok_or_else(|| anyhow!("{} response missing result field", method))?;
     parse_hex_u128(&value)
 }
 
@@ -98,6 +147,41 @@ pub async fn run_native_gas_price_refresh_loop(
     }
 }
 
+pub fn spawn_gas_price_startup_task(
+    app_state: AppState,
+    chain: Chain,
+    rpc_url: String,
+    refresh_interval: Duration,
+    failure_tolerance: u64,
+    client: Client,
+) -> tokio::task::JoinHandle<()> {
+    tokio::spawn(async move {
+        match wait_for_rpc_chain_match(&rpc_url, chain, refresh_interval, &client).await {
+            Ok(rpc_chain_id) => {
+                info!(
+                    refresh_interval_ms = refresh_interval.as_millis() as u64,
+                    failure_tolerance, rpc_chain_id, "Starting native gas price refresh loop"
+                );
+                run_native_gas_price_refresh_loop(
+                    app_state,
+                    rpc_url,
+                    refresh_interval,
+                    failure_tolerance,
+                    client,
+                )
+                .await;
+            }
+            Err(error) => {
+                error!(
+                    %error,
+                    expected_chain_id = chain.id(),
+                    "RPC_URL chain validation failed; gas-in-sell reporting remains disabled"
+                );
+            }
+        }
+    })
+}
+
 async fn process_refresh_result(
     app_state: &AppState,
     failure_tolerance: u64,
@@ -189,10 +273,15 @@ fn parse_hex_u128(value: &str) -> Result<u128> {
 #[cfg(test)]
 mod tests {
     use std::collections::HashMap;
+    use std::sync::atomic::{AtomicUsize, Ordering};
     use std::sync::Arc;
     use std::time::Duration;
 
     use anyhow::anyhow;
+    use axum::{response::IntoResponse, routing::post, Json, Router};
+    use reqwest::Client;
+    use serde_json::json;
+    use tokio::net::TcpListener;
     use tokio::sync::{RwLock, Semaphore};
     use tycho_simulation::tycho_common::models::Chain;
 
@@ -202,7 +291,10 @@ mod tests {
         tokens::TokenStore,
     };
 
-    use super::{parse_hex_u128, process_refresh_result};
+    use super::{
+        ensure_rpc_chain_matches, fetch_rpc_chain_id, parse_hex_u128, process_refresh_result,
+        wait_for_rpc_chain_match,
+    };
 
     fn build_test_app_state() -> AppState {
         let token_store = Arc::new(TokenStore::new(
@@ -214,6 +306,8 @@ mod tests {
         ));
 
         AppState {
+            chain: Chain::Ethereum,
+            native_token_protocol_allowlist: Arc::new(vec!["rocketpool".to_string()]),
             tokens: Arc::clone(&token_store),
             native_state_store: Arc::new(StateStore::new(Arc::clone(&token_store))),
             vm_state_store: Arc::new(StateStore::new(token_store)),
@@ -249,6 +343,111 @@ mod tests {
         assert!(parse_hex_u128("0xzz").is_err());
     }
 
+    #[test]
+    fn ensure_rpc_chain_matches_accepts_equal_chain_id() {
+        let result = ensure_rpc_chain_matches(Chain::Base, 8453);
+        assert!(result.is_ok());
+    }
+
+    #[test]
+    fn ensure_rpc_chain_matches_rejects_mismatched_chain_id() {
+        let error = ensure_rpc_chain_matches(Chain::Ethereum, 8453).unwrap_err();
+        assert!(error
+            .to_string()
+            .contains("RPC chain mismatch: expected 1, got 8453"));
+    }
+
+    #[tokio::test]
+    async fn fetch_rpc_chain_id_reads_eth_chain_id() {
+        let app = Router::new().route(
+            "/",
+            post(|| async { Json(json!({"jsonrpc":"2.0","id":1,"result":"0x2105"})) }),
+        );
+        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
+        let addr = listener.local_addr().unwrap();
+        tokio::spawn(async move {
+            axum::serve(listener, app).await.unwrap();
+        });
+        let url = format!("http://{}", addr);
+
+        let chain_id = fetch_rpc_chain_id(&url, &Client::new()).await.unwrap();
+        assert_eq!(chain_id, 8453);
+    }
+
+    #[tokio::test]
+    async fn wait_for_rpc_chain_match_retries_after_transient_chain_id_failure() {
+        let attempts = Arc::new(AtomicUsize::new(0));
+        let attempts_for_handler = attempts.clone();
+        let app = Router::new().route(
+            "/",
+            post(move || {
+                let attempts_for_handler = attempts_for_handler.clone();
+                async move {
+                    let attempt = attempts_for_handler.fetch_add(1, Ordering::SeqCst);
+                    if attempt == 0 {
+                        (axum::http::StatusCode::SERVICE_UNAVAILABLE, "try again").into_response()
+                    } else {
+                        Json(json!({"jsonrpc":"2.0","id":1,"result":"0x1"})).into_response()
+                    }
+                }
+            }),
+        );
+        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
+        let addr = listener.local_addr().unwrap();
+        tokio::spawn(async move {
+            axum::serve(listener, app).await.unwrap();
+        });
+        let url = format!("http://{}", addr);
+
+        let rpc_chain_id = wait_for_rpc_chain_match(
+            &url,
+            Chain::Ethereum,
+            Duration::from_millis(5),
+            &Client::new(),
+        )
+        .await
+        .unwrap();
+
+        assert_eq!(rpc_chain_id, 1);
+        assert!(attempts.load(Ordering::SeqCst) >= 2);
+    }
+
+    #[tokio::test]
+    async fn wait_for_rpc_chain_match_fails_fast_on_chain_mismatch() {
+        let attempts = Arc::new(AtomicUsize::new(0));
+        let attempts_for_handler = attempts.clone();
+        let app = Router::new().route(
+            "/",
+            post(move || {
+                let attempts_for_handler = attempts_for_handler.clone();
+                async move {
+                    attempts_for_handler.fetch_add(1, Ordering::SeqCst);
+                    Json(json!({"jsonrpc":"2.0","id":1,"result":"0x2105"})).into_response()
+                }
+            }),
+        );
+        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
+        let addr = listener.local_addr().unwrap();
+        tokio::spawn(async move {
+            axum::serve(listener, app).await.unwrap();
+        });
+        let url = format!("http://{}", addr);
+
+        let error = wait_for_rpc_chain_match(
+            &url,
+            Chain::Ethereum,
+            Duration::from_millis(5),
+            &Client::new(),
+        )
+        .await
+        .unwrap_err();
+
+        assert!(error
+            .to_string()
+            .contains("RPC chain mismatch: expected 1, got 8453"));
+        assert_eq!(attempts.load(Ordering::SeqCst), 1);
+    }
+
     #[tokio::test]
     async fn refresh_success_sets_cache_and_enables_reporting() {
         let app_state = build_test_app_state();
````

## src/handlers/readiness.rs
Change summary: Applies chain-profile compatibility updates and fixture wiring needed by the runtime/profile refactor.
Risk level: Medium
Review focus: Verify behavior parity and ensure new `AppState` fields are consistently initialized in tests and helpers.
Hunk context notes: Most changes here are collateral updates to preserve compile/runtime consistency after introducing runtime chain context.
````diff
diff --git a/src/handlers/readiness.rs b/src/handlers/readiness.rs
index 0a20b3d..76ec423 100644
--- a/src/handlers/readiness.rs
+++ b/src/handlers/readiness.rs
@@ -6,6 +6,7 @@ use crate::models::state::AppState;
 #[derive(Serialize)]
 pub struct StatusPayload {
     status: &'static str,
+    chain_id: u64,
     block: u64,
     pools: usize,
     vm_enabled: bool,
@@ -74,6 +75,7 @@ pub async fn status(State(state): State<AppState>) -> (StatusCode, Json<StatusPa
         status_code,
         Json(StatusPayload {
             status,
+            chain_id: state.chain.id(),
             block,
             pools,
             vm_enabled,
````

## src/services/quotes.rs
Change summary: Applies chain-profile compatibility updates and fixture wiring needed by the runtime/profile refactor.
Risk level: Medium
Review focus: Verify behavior parity and ensure new `AppState` fields are consistently initialized in tests and helpers.
Hunk context notes: Most changes here are collateral updates to preserve compile/runtime consistency after introducing runtime chain context.
````diff
diff --git a/src/services/quotes.rs b/src/services/quotes.rs
index 58ba4a3..b04fb53 100644
--- a/src/services/quotes.rs
+++ b/src/services/quotes.rs
@@ -2482,6 +2482,8 @@ mod tests {
             .await;
 
         let app_state = AppState {
+            chain: Chain::Ethereum,
+            native_token_protocol_allowlist: Arc::new(vec!["rocketpool".to_string()]),
             tokens: Arc::clone(&token_store),
             native_state_store: Arc::clone(&native_state_store),
             vm_state_store: Arc::clone(&vm_state_store),
@@ -2612,6 +2614,8 @@ mod tests {
             .await;
 
         let app_state = AppState {
+            chain: Chain::Ethereum,
+            native_token_protocol_allowlist: Arc::new(vec!["rocketpool".to_string()]),
             tokens: Arc::clone(&token_store),
             native_state_store: Arc::clone(&native_state_store),
             vm_state_store: Arc::clone(&vm_state_store),
@@ -2707,6 +2711,8 @@ mod tests {
             .await;
 
         let app_state = AppState {
+            chain: Chain::Ethereum,
+            native_token_protocol_allowlist: Arc::new(vec!["rocketpool".to_string()]),
             tokens: Arc::clone(&token_store),
             native_state_store: Arc::clone(&native_state_store),
             vm_state_store: Arc::clone(&vm_state_store),
@@ -2803,6 +2809,8 @@ mod tests {
             .await;
 
         let app_state = AppState {
+            chain: Chain::Ethereum,
+            native_token_protocol_allowlist: Arc::new(vec!["rocketpool".to_string()]),
             tokens: Arc::clone(&token_store),
             native_state_store,
             vm_state_store,
@@ -2891,6 +2899,8 @@ mod tests {
             .await;
 
         let app_state = AppState {
+            chain: Chain::Ethereum,
+            native_token_protocol_allowlist: Arc::new(vec!["rocketpool".to_string()]),
             tokens: Arc::clone(&token_store),
             native_state_store: Arc::clone(&native_state_store),
             vm_state_store: Arc::clone(&vm_state_store),
@@ -3164,6 +3174,8 @@ mod tests {
             .await;
 
         let app_state = AppState {
+            chain: Chain::Ethereum,
+            native_token_protocol_allowlist: Arc::new(vec!["rocketpool".to_string()]),
             tokens: Arc::clone(&token_store),
             native_state_store: Arc::clone(&native_state_store),
             vm_state_store: Arc::clone(&vm_state_store),
@@ -3329,6 +3341,8 @@ mod tests {
             .await;
 
         let app_state = AppState {
+            chain: Chain::Ethereum,
+            native_token_protocol_allowlist: Arc::new(vec!["rocketpool".to_string()]),
             tokens: Arc::clone(&token_store),
             native_state_store: Arc::clone(&native_state_store),
             vm_state_store: Arc::clone(&vm_state_store),
@@ -3561,6 +3575,8 @@ mod tests {
             .await;
 
         let app_state = AppState {
+            chain: Chain::Ethereum,
+            native_token_protocol_allowlist: Arc::new(vec!["rocketpool".to_string()]),
             tokens: Arc::clone(&token_store),
             native_state_store: Arc::clone(&native_state_store),
             vm_state_store: Arc::clone(&vm_state_store),
@@ -3685,6 +3701,8 @@ mod tests {
             .await;
 
         let app_state = AppState {
+            chain: Chain::Ethereum,
+            native_token_protocol_allowlist: Arc::new(vec!["rocketpool".to_string()]),
             tokens: Arc::clone(&token_store),
             native_state_store: Arc::clone(&native_state_store),
             vm_state_store: Arc::clone(&vm_state_store),
@@ -3781,6 +3799,8 @@ mod tests {
             .await;
 
         let app_state = AppState {
+            chain: Chain::Ethereum,
+            native_token_protocol_allowlist: Arc::new(vec!["rocketpool".to_string()]),
             tokens: Arc::clone(&token_store),
             native_state_store: Arc::clone(&native_state_store),
             vm_state_store: Arc::clone(&vm_state_store),
@@ -3886,6 +3906,8 @@ mod tests {
             .await;
 
         let app_state = AppState {
+            chain: Chain::Ethereum,
+            native_token_protocol_allowlist: Arc::new(vec!["rocketpool".to_string()]),
             tokens: Arc::clone(&token_store),
             native_state_store: Arc::clone(&native_state_store),
             vm_state_store: Arc::clone(&vm_state_store),
````

### Phase 2: Encode path chain/allowlist enforcement

## src/services/encode.rs
Change summary: Applies chain-profile compatibility updates and fixture wiring needed by the runtime/profile refactor.
Risk level: Medium
Review focus: Verify behavior parity and ensure new `AppState` fields are consistently initialized in tests and helpers.
Hunk context notes: Most changes here are collateral updates to preserve compile/runtime consistency after introducing runtime chain context.
````diff
diff --git a/src/services/encode.rs b/src/services/encode.rs
index 5e35de6..098eed8 100644
--- a/src/services/encode.rs
+++ b/src/services/encode.rs
@@ -21,7 +21,7 @@ pub async fn encode_route(
     state: AppState,
     request: RouteEncodeRequest,
 ) -> Result<RouteEncodeResponse, EncodeError> {
-    let chain = request::validate_chain(request.chain_id)?;
+    let chain = request::validate_chain(request.chain_id, state.chain)?;
     request::validate_swap_kinds(&request)?;
 
     let token_in = wire::parse_address(&request.token_in)?;
@@ -34,11 +34,19 @@ pub async fn encode_route(
     wire::biguint_to_u256_checked(&min_amount_out, "minAmountOut")?;
 
     let native_address = chain.native_token().address;
+    let allowlist = &state.native_token_protocol_allowlist;
     let is_native_input = token_in == native_address;
-    let normalized =
-        normalize::normalize_route(&request, &token_in, &token_out, &amount_in, &native_address)?;
+    let normalized = normalize::normalize_route(
+        &request,
+        &token_in,
+        &token_out,
+        &amount_in,
+        &native_address,
+        allowlist,
+    )?;
     let resimulated =
-        resimulate::resimulate_route(&state, &normalized, chain, &token_in, &token_out).await?;
+        resimulate::resimulate_route(&state, &normalized, chain, &token_in, &token_out, allowlist)
+            .await?;
     response::log_resimulation_amounts(request.request_id.as_deref(), &resimulated);
     let encoder = calldata::build_encoder(chain, router_address.clone())?;
     let expected_total = response::compute_expected_total(&resimulated);
````

## src/services/encode/request.rs
Change summary: Replaces fixed-chain validation and static native allowlist with runtime-chain validation and injected allowlist helpers.
Risk level: High
Review focus: Ensure request chain validation error semantics and allowlist normalization logic remain deterministic.
Hunk context notes: Encoding now enforces server runtime chain explicitly, so this path defines API compatibility guarantees.
````diff
diff --git a/src/services/encode/request.rs b/src/services/encode/request.rs
index 76f9ecd..e02a82a 100644
--- a/src/services/encode/request.rs
+++ b/src/services/encode/request.rs
@@ -6,17 +6,15 @@ use crate::models::messages::{RouteEncodeRequest, SegmentDraft, SwapKind};
 
 use super::EncodeError;
 
-const NATIVE_PROTOCOL_ALLOWLIST: &[&str] = &["rocketpool"];
-
-pub(super) fn validate_chain(chain_id: u64) -> Result<Chain, EncodeError> {
-    let chain = Chain::Ethereum;
-    if chain.id() != chain_id {
+pub(super) fn validate_chain(chain_id: u64, expected: Chain) -> Result<Chain, EncodeError> {
+    if expected.id() != chain_id {
         return Err(EncodeError::invalid(format!(
-            "Unsupported chainId: {}",
-            chain_id
+            "Unsupported chainId: {} (this instance serves chain {})",
+            chain_id,
+            expected.id()
         )));
     }
-    Ok(chain)
+    Ok(expected)
 }
 
 pub(super) fn normalize_protocol_id(protocol_id: &str) -> String {
@@ -26,15 +24,15 @@ pub(super) fn normalize_protocol_id(protocol_id: &str) -> String {
         .replace(['-', ' '], "_")
 }
 
-pub(super) fn is_native_protocol_allowlisted(protocol_id: &str) -> bool {
+pub(super) fn is_native_protocol_allowlisted(protocol_id: &str, allowlist: &[String]) -> bool {
     let normalized = normalize_protocol_id(protocol_id);
-    NATIVE_PROTOCOL_ALLOWLIST
+    allowlist
         .iter()
         .any(|candidate| normalize_protocol_id(candidate) == normalized)
 }
 
-pub(super) fn native_protocol_allowlist() -> &'static [&'static str] {
-    NATIVE_PROTOCOL_ALLOWLIST
+pub(super) fn format_native_protocol_allowlist(allowlist: &[String]) -> String {
+    allowlist.join(", ")
 }
 
 pub(super) fn should_reset_allowance(
@@ -162,9 +160,36 @@ mod tests {
 
     #[test]
     fn native_protocol_allowlist_matches_rocketpool_only() {
-        assert!(is_native_protocol_allowlisted("rocketpool"));
-        assert!(is_native_protocol_allowlisted("ROCKETPOOL"));
-        assert!(!is_native_protocol_allowlisted("uniswap_v2"));
-        assert_eq!(native_protocol_allowlist(), &["rocketpool"]);
+        let allowlist = vec!["rocketpool".to_string()];
+        assert!(is_native_protocol_allowlisted("rocketpool", &allowlist));
+        assert!(is_native_protocol_allowlisted("ROCKETPOOL", &allowlist));
+        assert!(!is_native_protocol_allowlisted("uniswap_v2", &allowlist));
+    }
+
+    #[test]
+    fn empty_allowlist_rejects_everything() {
+        let allowlist: Vec<String> = vec![];
+        assert!(!is_native_protocol_allowlisted("rocketpool", &allowlist));
+    }
+
+    #[test]
+    fn validate_chain_accepts_matching_chain() {
+        let chain = validate_chain(1, Chain::Ethereum).unwrap();
+        assert_eq!(chain, Chain::Ethereum);
+    }
+
+    #[test]
+    fn validate_chain_rejects_mismatched_chain() {
+        let err = validate_chain(8453, Chain::Ethereum).unwrap_err();
+        assert_eq!(
+            err.kind(),
+            crate::services::encode::EncodeErrorKind::InvalidRequest
+        );
+    }
+
+    #[test]
+    fn validate_chain_accepts_base() {
+        let chain = validate_chain(8453, Chain::Base).unwrap();
+        assert_eq!(chain, Chain::Base);
     }
 }
````

## src/services/encode/normalize.rs
Change summary: Propagates native-token protocol allowlist through route normalization and error reporting.
Risk level: Medium
Review focus: Check native token pair validation logic and user-facing error clarity.
Hunk context notes: Normalization is early validation; regressions here can reject valid routes or allow unsupported native-token swaps.
````diff
diff --git a/src/services/encode/normalize.rs b/src/services/encode/normalize.rs
index 9fdb31b..9cf5200 100644
--- a/src/services/encode/normalize.rs
+++ b/src/services/encode/normalize.rs
@@ -9,7 +9,7 @@ use super::model::{
     NormalizedHopInternal, NormalizedRouteInternal, NormalizedSegmentInternal,
     NormalizedSwapDraftInternal,
 };
-use super::request::{is_native_protocol_allowlisted, native_protocol_allowlist};
+use super::request::{format_native_protocol_allowlist, is_native_protocol_allowlisted};
 use super::wire::parse_address;
 use super::EncodeError;
 
@@ -19,6 +19,7 @@ pub(super) fn normalize_route(
     request_token_out: &Bytes,
     total_amount_in: &BigUint,
     native_address: &Bytes,
+    native_token_protocol_allowlist: &[String],
 ) -> Result<NormalizedRouteInternal, EncodeError> {
     if request.segments.is_empty() {
         return Err(EncodeError::invalid("segments must not be empty"));
@@ -44,7 +45,7 @@ pub(super) fn normalize_route(
         let hops = segment
             .hops
             .iter()
-            .map(|hop| normalize_hop(hop, native_address))
+            .map(|hop| normalize_hop(hop, native_address, native_token_protocol_allowlist))
             .collect::<Result<Vec<_>, _>>()?;
 
         let segment_amount_in = segment_amounts
@@ -123,6 +124,7 @@ fn validate_hop_continuity(
 fn normalize_hop(
     hop: &HopDraft,
     native_address: &Bytes,
+    native_token_protocol_allowlist: &[String],
 ) -> Result<NormalizedHopInternal, EncodeError> {
     if hop.swaps.is_empty() {
         return Err(EncodeError::invalid("hop.swaps must not be empty"));
@@ -132,8 +134,9 @@ fn normalize_hop(
     let token_out = parse_address(&hop.token_out)?;
     if token_in == *native_address || token_out == *native_address {
         for swap in &hop.swaps {
-            if !is_native_protocol_allowlisted(&swap.pool.protocol) {
-                let supported = native_protocol_allowlist().join(", ");
+            if !is_native_protocol_allowlisted(&swap.pool.protocol, native_token_protocol_allowlist)
+            {
+                let supported = format_native_protocol_allowlist(native_token_protocol_allowlist);
                 return Err(EncodeError::invalid(format!(
                     "native tokenIn/tokenOut is only supported for protocols [{}]; got {}",
                     supported, swap.pool.protocol
@@ -244,8 +247,15 @@ mod tests {
         let amount_in = parse_amount(&request.amount_in).unwrap();
 
         let native_address = Chain::Ethereum.native_token().address;
-        let normalized =
-            normalize_route(&request, &token_in, &token_out, &amount_in, &native_address).unwrap();
+        let normalized = normalize_route(
+            &request,
+            &token_in,
+            &token_out,
+            &amount_in,
+            &native_address,
+            &["rocketpool".to_string()],
+        )
+        .unwrap();
         assert_eq!(normalized.segments.len(), 2);
         assert_eq!(normalized.segments[0].share_bps, 2000);
         assert_eq!(normalized.segments[1].share_bps, 0);
@@ -302,7 +312,14 @@ mod tests {
         let amount_in = BigUint::from(1u32);
         let native = Chain::Ethereum.native_token().address;
 
-        let err = match normalize_route(&request, &token_in, &token_out, &amount_in, &native) {
+        let err = match normalize_route(
+            &request,
+            &token_in,
+            &token_out,
+            &amount_in,
+            &native,
+            &["rocketpool".to_string()],
+        ) {
             Ok(_) => panic!("Expected segment share rounding to zero to be rejected"),
             Err(err) => err,
         };
@@ -366,7 +383,14 @@ mod tests {
         let amount_in = parse_amount(&request.amount_in).unwrap();
         let native_address = Chain::Ethereum.native_token().address;
 
-        match normalize_route(&request, &token_in, &token_out, &amount_in, &native_address) {
+        match normalize_route(
+            &request,
+            &token_in,
+            &token_out,
+            &amount_in,
+            &native_address,
+            &["rocketpool".to_string()],
+        ) {
             Err(err) => assert_eq!(
                 err.kind(),
                 crate::services::encode::EncodeErrorKind::InvalidRequest
@@ -420,7 +444,14 @@ mod tests {
         let token_out = parse_address(&request.token_out).unwrap();
         let amount_in = parse_amount(&request.amount_in).unwrap();
 
-        match normalize_route(&request, &token_in, &token_out, &amount_in, &native) {
+        match normalize_route(
+            &request,
+            &token_in,
+            &token_out,
+            &amount_in,
+            &native,
+            &["rocketpool".to_string()],
+        ) {
             Err(err) => assert_eq!(
                 err.kind(),
                 crate::services::encode::EncodeErrorKind::InvalidRequest
@@ -467,9 +498,15 @@ mod tests {
         let request_token_out = parse_address(&request.token_out).unwrap();
         let amount_in = parse_amount(&request.amount_in).unwrap();
 
-        let normalized =
-            normalize_route(&request, &token_in, &request_token_out, &amount_in, &native)
-                .expect("rocketpool native route should normalize");
+        let normalized = normalize_route(
+            &request,
+            &token_in,
+            &request_token_out,
+            &amount_in,
+            &native,
+            &["rocketpool".to_string()],
+        )
+        .expect("rocketpool native route should normalize");
         assert_eq!(normalized.segments.len(), 1);
         assert_eq!(normalized.segments[0].hops.len(), 1);
         assert_eq!(normalized.segments[0].hops[0].token_in, native);
````

## src/services/encode/resimulate.rs
Change summary: Uses injected native allowlist during hop simulation checks and updates tests to include chain/allowlist in state fixtures.
Risk level: Medium
Review focus: Verify native swap support checks still align with component protocol IDs across native and VM components.
Hunk context notes: Resimulation is correctness-critical for output amounts and calldata generation; allowlist mismatches can cause subtle encode failures.
````diff
diff --git a/src/services/encode/resimulate.rs b/src/services/encode/resimulate.rs
index e1b2be2..c509c0d 100644
--- a/src/services/encode/resimulate.rs
+++ b/src/services/encode/resimulate.rs
@@ -21,7 +21,7 @@ use super::model::{
     NormalizedRouteInternal, ResimulatedHopInternal, ResimulatedRouteInternal,
     ResimulatedSegmentInternal, ResimulatedSwapInternal,
 };
-use super::request::{is_native_protocol_allowlisted, native_protocol_allowlist};
+use super::request::{format_native_protocol_allowlist, is_native_protocol_allowlisted};
 use super::EncodeError;
 
 pub(super) async fn resimulate_route(
@@ -30,6 +30,7 @@ pub(super) async fn resimulate_route(
     chain: Chain,
     request_token_in: &Bytes,
     request_token_out: &Bytes,
+    native_token_protocol_allowlist: &[String],
 ) -> Result<ResimulatedRouteInternal, EncodeError> {
     let mut token_cache = TokenCache::new(state);
     let mut pool_cache: HashMap<String, (Arc<dyn ProtocolSim>, Arc<ProtocolComponent>)> =
@@ -100,6 +101,7 @@ pub(super) async fn resimulate_route(
                     &allocated.token_out,
                     &pool_entry.1,
                     &allocated.pool.component_id,
+                    native_token_protocol_allowlist,
                 )?;
                 let sim_token_in =
                     map_swap_token(&allocated.token_in, chain, keep_native_unwrapped);
@@ -274,13 +276,15 @@ fn ensure_native_swap_supported(
     token_out: &Bytes,
     component: &ProtocolComponent,
     component_id: &str,
+    native_token_protocol_allowlist: &[String],
 ) -> Result<bool, EncodeError> {
     let native_address = chain.native_token().address;
     let swap_uses_native = *token_in == native_address || *token_out == native_address;
-    let protocol_supports_native = is_native_protocol_allowlisted(&component.protocol_system);
+    let protocol_supports_native =
+        is_native_protocol_allowlisted(&component.protocol_system, native_token_protocol_allowlist);
 
     if swap_uses_native && !protocol_supports_native {
-        let supported = native_protocol_allowlist().join(", ");
+        let supported = format_native_protocol_allowlist(native_token_protocol_allowlist);
         return Err(EncodeError::invalid(format!(
             "native tokenIn/tokenOut is only supported for protocols [{}]; pool {} uses {}",
             supported, component_id, component.protocol_system
@@ -487,6 +491,8 @@ mod tests {
         native_state_store.apply_update(update).await;
 
         let app_state = AppState {
+            chain: Chain::Ethereum,
+            native_token_protocol_allowlist: Arc::new(vec!["rocketpool".to_string()]),
             tokens: Arc::clone(&tokens_store),
             native_state_store: Arc::clone(&native_state_store),
             vm_state_store: Arc::clone(&vm_state_store),
@@ -539,6 +545,7 @@ mod tests {
             Chain::Ethereum,
             &token_in.address,
             &token_out.address,
+            &["rocketpool".to_string()],
         )
         .await
         .unwrap();
@@ -595,6 +602,8 @@ mod tests {
         native_state_store.apply_update(update).await;
 
         let app_state = AppState {
+            chain: Chain::Ethereum,
+            native_token_protocol_allowlist: Arc::new(vec!["rocketpool".to_string()]),
             tokens: Arc::clone(&tokens_store),
             native_state_store: Arc::clone(&native_state_store),
             vm_state_store: Arc::clone(&vm_state_store),
@@ -667,6 +676,7 @@ mod tests {
             Chain::Ethereum,
             &token_a.address,
             &token_c.address,
+            &["rocketpool".to_string()],
         )
         .await
         .unwrap();
@@ -723,6 +733,8 @@ mod tests {
             .await;
 
         let app_state = AppState {
+            chain: Chain::Ethereum,
+            native_token_protocol_allowlist: Arc::new(vec!["rocketpool".to_string()]),
             tokens: Arc::clone(&tokens_store),
             native_state_store: Arc::clone(&native_state_store),
             vm_state_store: Arc::clone(&vm_state_store),
@@ -767,6 +779,7 @@ mod tests {
             Chain::Ethereum,
             &token_in.address,
             &token_out.address,
+            &["rocketpool".to_string()],
         )
         .await
         {
@@ -829,6 +842,8 @@ mod tests {
             .await;
 
         let app_state = AppState {
+            chain: Chain::Ethereum,
+            native_token_protocol_allowlist: Arc::new(vec!["rocketpool".to_string()]),
             tokens: Arc::clone(&tokens_store),
             native_state_store: Arc::clone(&native_state_store),
             vm_state_store: Arc::clone(&vm_state_store),
@@ -873,6 +888,7 @@ mod tests {
             Chain::Ethereum,
             &token_in.address,
             &token_out.address,
+            &["rocketpool".to_string()],
         )
         .await
         {
````

## src/services/encode/response.rs
Change summary: Applies chain-profile compatibility updates and fixture wiring needed by the runtime/profile refactor.
Risk level: Medium
Review focus: Verify behavior parity and ensure new `AppState` fields are consistently initialized in tests and helpers.
Hunk context notes: Most changes here are collateral updates to preserve compile/runtime consistency after introducing runtime chain context.
````diff
diff --git a/src/services/encode/response.rs b/src/services/encode/response.rs
index 32125cb..ee4e1af 100644
--- a/src/services/encode/response.rs
+++ b/src/services/encode/response.rs
@@ -109,6 +109,8 @@ mod tests {
             .await;
 
         let state = AppState {
+            chain: Chain::Ethereum,
+            native_token_protocol_allowlist: Arc::new(vec!["rocketpool".to_string()]),
             tokens: std::sync::Arc::clone(&tokens_store),
             native_state_store: std::sync::Arc::clone(&native_state_store),
             vm_state_store: std::sync::Arc::clone(&vm_state_store),
@@ -165,6 +167,8 @@ mod tests {
             .await;
 
         let state = AppState {
+            chain: Chain::Ethereum,
+            native_token_protocol_allowlist: Arc::new(vec!["rocketpool".to_string()]),
             tokens: std::sync::Arc::clone(&tokens_store),
             native_state_store: std::sync::Arc::clone(&native_state_store),
             vm_state_store: std::sync::Arc::clone(&vm_state_store),
````

## tests/integration/encode_route.rs
Change summary: Updates integration coverage and fixtures to align with chain-aware runtime state and startup behavior.
Risk level: Medium
Review focus: Ensure assertions cover new runtime invariants without introducing flaky timing assumptions.
Hunk context notes: These tests define acceptance criteria for the chain-aware migration and should remain deterministic.
````diff
diff --git a/tests/integration/encode_route.rs b/tests/integration/encode_route.rs
index 79bdaeb..e548149 100644
--- a/tests/integration/encode_route.rs
+++ b/tests/integration/encode_route.rs
@@ -213,6 +213,8 @@ async fn setup_app_state_and_request(
     }
 
     let state = AppState {
+        chain: Chain::Ethereum,
+        native_token_protocol_allowlist: Arc::new(vec!["rocketpool".to_string()]),
         tokens: Arc::clone(&tokens_store),
         native_state_store: Arc::clone(&native_state_store),
         vm_state_store: Arc::clone(&vm_state_store),
@@ -413,6 +415,43 @@ async fn encode_route_rejects_when_min_amount_out_exceeds_expected() {
     assert_eq!(response.request_id.as_deref(), Some("req-1"));
 }
 
+#[tokio::test]
+async fn encode_route_rejects_when_request_chain_does_not_match_runtime_chain() {
+    let (app, mut request) = setup_app_state_and_request(EncodeFixtureConfig::default()).await;
+    request.chain_id = 8453;
+
+    let response = app
+        .oneshot(
+            Request::builder()
+                .method("POST")
+                .uri("/encode")
+                .header("content-type", "application/json")
+                .body(Body::from(serde_json::to_vec(&request).unwrap()))
+                .unwrap(),
+        )
+        .await
+        .unwrap();
+
+    let status = response.status();
+    let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
+    assert_eq!(
+        status,
+        StatusCode::BAD_REQUEST,
+        "unexpected status {}: {}",
+        status,
+        String::from_utf8_lossy(&body)
+    );
+    let response: EncodeErrorResponse = serde_json::from_slice(&body).unwrap();
+    assert!(
+        response
+            .error
+            .contains("Unsupported chainId: 8453 (this instance serves chain 1)"),
+        "unexpected error: {}",
+        response.error
+    );
+    assert_eq!(response.request_id.as_deref(), Some("req-1"));
+}
+
 #[tokio::test]
 async fn encode_route_succeeds_for_vm_maverick_v2_pool() {
     let config = EncodeFixtureConfig {
````

### Phase 3: Integration safety nets

## tests/integration/main.rs
Change summary: Updates integration coverage and fixtures to align with chain-aware runtime state and startup behavior.
Risk level: Medium
Review focus: Ensure assertions cover new runtime invariants without introducing flaky timing assumptions.
Hunk context notes: These tests define acceptance criteria for the chain-aware migration and should remain deterministic.
````diff
diff --git a/tests/integration/main.rs b/tests/integration/main.rs
index 2a769de..cf81125 100644
--- a/tests/integration/main.rs
+++ b/tests/integration/main.rs
@@ -1,3 +1,4 @@
 mod encode_route;
 mod protocol_reset_memory;
 mod simulate_gas_in_sell;
+mod startup_gas_price_task;
````

## tests/integration/startup_gas_price_task.rs
Change summary: Adds end-to-end integration coverage for startup gas-price task behavior (retry then success, and fast-fail on chain mismatch).
Risk level: Medium
Review focus: Confirm timing assumptions, mock RPC responses, and assertions for reporting enablement vs mismatch shutdown.
Hunk context notes: This test encodes the new startup contract; stability here is important to avoid flaky CI around async startup loops.
````diff
diff --git a/tests/integration/startup_gas_price_task.rs b/tests/integration/startup_gas_price_task.rs
new file mode 100644
index 0000000..7c7f461
--- /dev/null
+++ b/tests/integration/startup_gas_price_task.rs
@@ -0,0 +1,199 @@
+use std::collections::{HashMap, HashSet};
+use std::sync::atomic::{AtomicUsize, Ordering};
+use std::sync::Arc;
+use std::time::Duration;
+
+use axum::{
+    extract::{Json, State},
+    response::IntoResponse,
+    routing::post,
+    Router,
+};
+use reqwest::Client;
+use tokio::net::TcpListener;
+use tokio::sync::{RwLock, Semaphore};
+use tokio::time::{sleep, timeout, Instant};
+use tycho_simulation::tycho_common::{models::Chain, Bytes};
+use tycho_simulation_server::models::{
+    state::{AppState, StateStore, VmStreamStatus},
+    stream_health::StreamHealth,
+    tokens::TokenStore,
+};
+use tycho_simulation_server::services::gas_price::spawn_gas_price_startup_task;
+
+#[derive(Clone, Copy)]
+enum RpcScenario {
+    RetryThenSuccess,
+    ChainMismatch,
+}
+
+#[derive(Default)]
+struct RpcCounters {
+    chain_id_calls: AtomicUsize,
+    gas_price_calls: AtomicUsize,
+}
+
+struct RpcMockState {
+    scenario: RpcScenario,
+    counters: RpcCounters,
+}
+
+fn build_test_app_state() -> AppState {
+    let token_store = Arc::new(TokenStore::new(
+        HashMap::new(),
+        "http://localhost".to_string(),
+        "test".to_string(),
+        Chain::Ethereum,
+        Duration::from_millis(10),
+    ));
+
+    AppState {
+        chain: Chain::Ethereum,
+        native_token_protocol_allowlist: Arc::new(vec!["rocketpool".to_string()]),
+        tokens: Arc::clone(&token_store),
+        native_state_store: Arc::new(StateStore::new(Arc::clone(&token_store))),
+        vm_state_store: Arc::new(StateStore::new(token_store)),
+        native_stream_health: Arc::new(StreamHealth::new()),
+        vm_stream_health: Arc::new(StreamHealth::new()),
+        vm_stream: Arc::new(RwLock::new(VmStreamStatus::default())),
+        latest_native_gas_price_wei: Arc::new(RwLock::new(None)),
+        native_gas_price_reporting_enabled: Arc::new(RwLock::new(false)),
+        enable_vm_pools: false,
+        readiness_stale: Duration::from_secs(120),
+        quote_timeout: Duration::from_millis(100),
+        pool_timeout_native: Duration::from_millis(50),
+        pool_timeout_vm: Duration::from_millis(50),
+        request_timeout: Duration::from_millis(1000),
+        native_sim_semaphore: Arc::new(Semaphore::new(1)),
+        vm_sim_semaphore: Arc::new(Semaphore::new(1)),
+        reset_allowance_tokens: Arc::new(HashMap::<u64, HashSet<Bytes>>::new()),
+        native_sim_concurrency: 1,
+        vm_sim_concurrency: 1,
+    }
+}
+
+async fn mock_rpc(
+    State(state): State<Arc<RpcMockState>>,
+    Json(payload): Json<serde_json::Value>,
+) -> axum::response::Response {
+    let Some(method) = payload.get("method").and_then(serde_json::Value::as_str) else {
+        return (axum::http::StatusCode::BAD_REQUEST, "missing method").into_response();
+    };
+
+    match method {
+        "eth_chainId" => {
+            let call = state.counters.chain_id_calls.fetch_add(1, Ordering::SeqCst);
+            match state.scenario {
+                RpcScenario::RetryThenSuccess if call == 0 => {
+                    (axum::http::StatusCode::SERVICE_UNAVAILABLE, "try again").into_response()
+                }
+                RpcScenario::RetryThenSuccess => {
+                    Json(serde_json::json!({"jsonrpc":"2.0","id":1,"result":"0x1"})).into_response()
+                }
+                RpcScenario::ChainMismatch => {
+                    Json(serde_json::json!({"jsonrpc":"2.0","id":1,"result":"0x2105"}))
+                        .into_response()
+                }
+            }
+        }
+        "eth_gasPrice" => {
+            state
+                .counters
+                .gas_price_calls
+                .fetch_add(1, Ordering::SeqCst);
+            Json(serde_json::json!({"jsonrpc":"2.0","id":1,"result":"0x3b9aca00"})).into_response()
+        }
+        _ => (axum::http::StatusCode::BAD_REQUEST, "unexpected method").into_response(),
+    }
+}
+
+async fn spawn_mock_rpc_server(
+    scenario: RpcScenario,
+) -> (String, Arc<RpcMockState>, tokio::task::JoinHandle<()>) {
+    let state = Arc::new(RpcMockState {
+        scenario,
+        counters: RpcCounters::default(),
+    });
+    let app = Router::new()
+        .route("/", post(mock_rpc))
+        .with_state(Arc::clone(&state));
+
+    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
+    let addr = listener.local_addr().unwrap();
+    let server = tokio::spawn(async move {
+        axum::serve(listener, app).await.unwrap();
+    });
+
+    (format!("http://{}", addr), state, server)
+}
+
+#[tokio::test]
+async fn startup_task_retries_chain_validation_then_enables_reporting() {
+    let (rpc_url, rpc_state, server_handle) =
+        spawn_mock_rpc_server(RpcScenario::RetryThenSuccess).await;
+    let app_state = build_test_app_state();
+
+    let startup_handle = spawn_gas_price_startup_task(
+        app_state.clone(),
+        Chain::Ethereum,
+        rpc_url,
+        Duration::from_millis(5),
+        5,
+        Client::new(),
+    );
+
+    let start = Instant::now();
+    timeout(Duration::from_secs(1), async {
+        loop {
+            if app_state.native_gas_price_reporting_enabled().await {
+                break;
+            }
+            if start.elapsed() > Duration::from_secs(1) {
+                panic!("reporting did not become enabled in time");
+            }
+            sleep(Duration::from_millis(5)).await;
+        }
+    })
+    .await
+    .expect("timed out waiting for gas reporting to become enabled");
+
+    assert_eq!(
+        app_state.latest_native_gas_price_wei().await,
+        Some(1_000_000_000)
+    );
+    assert!(rpc_state.counters.chain_id_calls.load(Ordering::SeqCst) >= 2);
+    assert!(rpc_state.counters.gas_price_calls.load(Ordering::SeqCst) >= 1);
+
+    startup_handle.abort();
+    let _ = startup_handle.await;
+    server_handle.abort();
+    let _ = server_handle.await;
+}
+
+#[tokio::test]
+async fn startup_task_fails_fast_on_chain_mismatch() {
+    let (rpc_url, rpc_state, server_handle) =
+        spawn_mock_rpc_server(RpcScenario::ChainMismatch).await;
+    let app_state = build_test_app_state();
+
+    let startup_handle = spawn_gas_price_startup_task(
+        app_state.clone(),
+        Chain::Ethereum,
+        rpc_url,
+        Duration::from_millis(5),
+        5,
+        Client::new(),
+    );
+
+    timeout(Duration::from_secs(1), startup_handle)
+        .await
+        .expect("startup task should exit on chain mismatch")
+        .expect("startup task join should not panic");
+
+    assert!(!app_state.native_gas_price_reporting_enabled().await);
+    assert_eq!(app_state.latest_native_gas_price_wei().await, None);
+    assert_eq!(rpc_state.counters.gas_price_calls.load(Ordering::SeqCst), 0);
+
+    server_handle.abort();
+    let _ = server_handle.await;
+}
````

## tests/integration/simulate_gas_in_sell.rs
Change summary: Updates integration coverage and fixtures to align with chain-aware runtime state and startup behavior.
Risk level: Medium
Review focus: Ensure assertions cover new runtime invariants without introducing flaky timing assumptions.
Hunk context notes: These tests define acceptance criteria for the chain-aware migration and should remain deterministic.
````diff
diff --git a/tests/integration/simulate_gas_in_sell.rs b/tests/integration/simulate_gas_in_sell.rs
index cc1bf0e..dbbdbbd 100644
--- a/tests/integration/simulate_gas_in_sell.rs
+++ b/tests/integration/simulate_gas_in_sell.rs
@@ -366,6 +366,8 @@ async fn simulate_gas_in_sell_matches_gas_price_cost_in_usd_for_multiple_pairs()
         .await;
 
     let app_state = AppState {
+        chain: Chain::Ethereum,
+        native_token_protocol_allowlist: Arc::new(vec!["rocketpool".to_string()]),
         tokens: Arc::clone(&token_store),
         native_state_store: Arc::clone(&native_state_store),
         vm_state_store: Arc::clone(&vm_state_store),
@@ -591,6 +593,8 @@ async fn simulate_gas_in_sell_is_zero_when_reporting_disabled() {
         .await;
 
     let app_state = AppState {
+        chain: Chain::Ethereum,
+        native_token_protocol_allowlist: Arc::new(vec!["rocketpool".to_string()]),
         tokens: Arc::clone(&token_store),
         native_state_store: Arc::clone(&native_state_store),
         vm_state_store: Arc::clone(&vm_state_store),
````

## tests/integration/protocol_reset_memory.rs
Change summary: Updates integration coverage and fixtures to align with chain-aware runtime state and startup behavior.
Risk level: Medium
Review focus: Ensure assertions cover new runtime invariants without introducing flaky timing assumptions.
Hunk context notes: These tests define acceptance criteria for the chain-aware migration and should remain deterministic.
````diff
diff --git a/tests/integration/protocol_reset_memory.rs b/tests/integration/protocol_reset_memory.rs
index 36501b9..245f793 100644
--- a/tests/integration/protocol_reset_memory.rs
+++ b/tests/integration/protocol_reset_memory.rs
@@ -485,6 +485,8 @@ async fn vm_rebuild_resets_store_and_blocks_quotes() {
     vm_state_store.apply_update(vm_update).await;
 
     let app_state = AppState {
+        chain: Chain::Ethereum,
+        native_token_protocol_allowlist: Arc::new(vec!["rocketpool".to_string()]),
         tokens: Arc::clone(&token_store),
         native_state_store: Arc::clone(&native_state_store),
         vm_state_store: Arc::clone(&vm_state_store),
````

### Phase 4: Runtime scripts and operator workflow

## .env.example
Change summary: Documentation/configuration update related to chain-aware runtime rollout.
Risk level: Low
Review focus: Verify wording and examples reflect the new required `CHAIN_ID` behavior.
Hunk context notes: These changes reduce operator confusion during rollout and incident response.
````diff
diff --git a/.env.example b/.env.example
index 5d2b00e..5f3d389 100644
--- a/.env.example
+++ b/.env.example
@@ -1,7 +1,9 @@
 # Tycho API settings
 TYCHO_URL=tycho-beta.propellerheads.xyz
 TYCHO_API_KEY=your_api_key_here
-# Optional Ethereum JSON-RPC endpoint used for background eth_gasPrice refreshes
+# Runtime chain selection (required): 1 = Ethereum, 8453 = Base
+CHAIN_ID=1
+# Optional JSON-RPC endpoint matching CHAIN_ID, used for background eth_gasPrice refreshes
 RPC_URL=
 # Refresh interval for gas price polling task (milliseconds)
 GAS_PRICE_REFRESH_INTERVAL_MS=5000
````

## scripts/presets.py
Change summary: Refactors presets into chain-aware token/suite registries with explicit chain-id resolution from args/environment.
Risk level: High
Review focus: Review chain-specific token maps, pair suites, and defaults for coverage parity expectations.
Hunk context notes: Most operational scripts now depend on this module; any malformed suite/token mapping can cascade across smoke, coverage, and latency tooling.
````diff
diff --git a/scripts/presets.py b/scripts/presets.py
index 403a514..53ae8f3 100755
--- a/scripts/presets.py
+++ b/scripts/presets.py
@@ -2,12 +2,40 @@
 
 from __future__ import annotations
 
+import os
 from dataclasses import dataclass
 from typing import Tuple
 
+SUPPORTED_CHAIN_IDS = (1, 8453)
+
+
+def parse_chain_id(raw_value: str) -> int:
+    try:
+        chain_id = int(raw_value)
+    except ValueError as exc:
+        raise ValueError(f"Invalid chain id: {raw_value!r}. Expected one of {SUPPORTED_CHAIN_IDS}.") from exc
+    if chain_id not in SUPPORTED_CHAIN_IDS:
+        raise ValueError(f"Unsupported chain id: {chain_id}. Supported values: {SUPPORTED_CHAIN_IDS}.")
+    return chain_id
+
+
+def resolve_chain_id(chain_id_arg: str | None) -> int:
+    raw = chain_id_arg
+    if raw is None:
+        raw = os.environ.get("CHAIN_ID")
+    if raw is None or not raw.strip():
+        raise ValueError(
+            "Missing chain id. Pass --chain-id or set CHAIN_ID in the environment/.env."
+        )
+    return parse_chain_id(raw.strip())
+
+
+def chain_label(chain_id: int) -> str:
+    return {1: "ethereum", 8453: "base"}.get(chain_id, f"chain-{chain_id}")
+
 
 # Ethereum mainnet token addresses (lowercased).
-TOKENS: dict[str, str] = {
+ETHEREUM_TOKENS: dict[str, str] = {
     "ETH": "0x0000000000000000000000000000000000000000",
     "WETH": "0xc02aaa39b223fe8d0a0e5c4f27ead9083c756cc2",
     "WBTC": "0x2260fac5e5542a773aa44fbcfedf7c193bc2c599",
@@ -41,58 +69,99 @@ TOKENS: dict[str, str] = {
     "FXS": "0x3432b6a60d23ca0dfca7761b7ab56459d9c964d0",
 }
 
-TOKEN_DECIMALS: dict[str, int] = {
-    "USDC": 6,
-    "USDT": 6,
-    "WBTC": 8,
+# Base token addresses (lowercased).
+BASE_TOKENS: dict[str, str] = {
+    "ETH": "0x0000000000000000000000000000000000000000",
+    "WETH": "0x4200000000000000000000000000000000000006",
+    "USDC": "0x833589fcd6edb6e08f4c7c32d4f71b54bda02913",
+    "DAI": "0x50c5725949a6f0c72e6c4a641f24049a917db0cb",
 }
 
-ADDRESS_DECIMALS: dict[str, int] = {
-    TOKENS[symbol].lower(): decimals for symbol, decimals in TOKEN_DECIMALS.items()
+TOKENS_BY_CHAIN: dict[int, dict[str, str]] = {
+    1: ETHEREUM_TOKENS,
+    8453: BASE_TOKENS,
 }
 
-TOKEN_BASE_UNITS: dict[str, list[int]] = {
-    # 0.001 .. 2 WBTC (8 decimals) to avoid pathological ladders at huge sizes.
-    "WBTC": [
-        100_000,
-        500_000,
-        1_000_000,
-        5_000_000,
-        10_000_000,
-        25_000_000,
-        50_000_000,
-        100_000_000,
-        200_000_000,
-    ],
-    # 0.1 .. 500 WETH (18 decimals) to avoid VM pool reverts at very large sizes.
-    "WETH": [
-        100_000_000_000_000_000,
-        500_000_000_000_000_000,
-        1_000_000_000_000_000_000,
-        2_000_000_000_000_000_000,
-        5_000_000_000_000_000_000,
-        10_000_000_000_000_000_000,
-        20_000_000_000_000_000_000,
-        50_000_000_000_000_000_000,
-        100_000_000_000_000_000_000,
-        500_000_000_000_000_000_000,
-    ],
-    "ETH": [
-        100_000_000_000_000_000,
-        500_000_000_000_000_000,
-        1_000_000_000_000_000_000,
-        2_000_000_000_000_000_000,
-        5_000_000_000_000_000_000,
-        10_000_000_000_000_000_000,
-        20_000_000_000_000_000_000,
-        50_000_000_000_000_000_000,
-        100_000_000_000_000_000_000,
-        500_000_000_000_000_000_000,
-    ],
+# Backward-compatible alias used by older callers.
+TOKENS: dict[str, str] = TOKENS_BY_CHAIN[1]
+
+TOKEN_DECIMALS_BY_CHAIN: dict[int, dict[str, int]] = {
+    1: {
+        "USDC": 6,
+        "USDT": 6,
+        "WBTC": 8,
+    },
+    8453: {
+        "USDC": 6,
+    },
 }
 
-ADDRESS_BASE_UNITS: dict[str, list[int]] = {
-    TOKENS[symbol].lower(): units for symbol, units in TOKEN_BASE_UNITS.items()
+TOKEN_BASE_UNITS_BY_CHAIN: dict[int, dict[str, list[int]]] = {
+    1: {
+        # 0.001 .. 2 WBTC (8 decimals) to avoid pathological ladders at huge sizes.
+        "WBTC": [
+            100_000,
+            500_000,
+            1_000_000,
+            5_000_000,
+            10_000_000,
+            25_000_000,
+            50_000_000,
+            100_000_000,
+            200_000_000,
+        ],
+        # 0.1 .. 500 WETH (18 decimals) to avoid VM pool reverts at very large sizes.
+        "WETH": [
+            100_000_000_000_000_000,
+            500_000_000_000_000_000,
+            1_000_000_000_000_000_000,
+            2_000_000_000_000_000_000,
+            5_000_000_000_000_000_000,
+            10_000_000_000_000_000_000,
+            20_000_000_000_000_000_000,
+            50_000_000_000_000_000_000,
+            100_000_000_000_000_000_000,
+            500_000_000_000_000_000_000,
+        ],
+        "ETH": [
+            100_000_000_000_000_000,
+            500_000_000_000_000_000,
+            1_000_000_000_000_000_000,
+            2_000_000_000_000_000_000,
+            5_000_000_000_000_000_000,
+            10_000_000_000_000_000_000,
+            20_000_000_000_000_000_000,
+            50_000_000_000_000_000_000,
+            100_000_000_000_000_000_000,
+            500_000_000_000_000_000_000,
+        ],
+    },
+    8453: {
+        "WETH": [
+            10_000_000_000_000_000,
+            50_000_000_000_000_000,
+            100_000_000_000_000_000,
+            500_000_000_000_000_000,
+            1_000_000_000_000_000_000,
+            2_000_000_000_000_000_000,
+            5_000_000_000_000_000_000,
+            10_000_000_000_000_000_000,
+            20_000_000_000_000_000_000,
+            50_000_000_000_000_000_000,
+        ],
+        "ETH": [
+            10_000_000_000_000_000,
+            50_000_000_000_000_000,
+            100_000_000_000_000_000,
+            500_000_000_000_000_000,
+            1_000_000_000_000_000_000,
+            2_000_000_000_000_000_000,
+            5_000_000_000_000_000_000,
+            10_000_000_000_000_000_000,
+            20_000_000_000_000_000_000,
+            50_000_000_000_000_000_000,
+        ],
+    },
 }
 
 BASE_AMOUNTS: list[int] = [
@@ -110,80 +179,144 @@ BASE_AMOUNTS: list[int] = [
 
 Pair = Tuple[str, str]
 
-CORE_PAIRS: list[Pair] = [
-    ("DAI", "USDC"),
-    ("USDC", "USDT"),
-    ("GHO", "USDC"),
-    ("WETH", "USDC"),
-    ("WETH", "USDT"),
-    ("WETH", "DAI"),
-    ("WBTC", "USDC"),
-    ("WBTC", "WETH"),
-    ("FRAX", "USDC"),
-    ("STETH", "WETH"),
-    ("WSTETH", "WETH"),
-    ("RETH", "WETH"),
-    ("ETH", "RETH"),
-    ("RETH", "ETH"),
-    ("CBETH", "WETH"),
-    ("UNI", "WETH"),
-    ("LINK", "WETH"),
-    ("AAVE", "WETH"),
-    ("COMP", "WETH"),
-    ("MKR", "WETH"),
-    ("SUSHI", "WETH"),
-    ("LDO", "WETH"),
-]
-
-LOW_LIQUIDITY_PAIRS: list[Pair] = [
-    ("LUSD", "USDC"),
-    ("FRXETH", "WETH"),
-    ("CRV", "WETH"),
-    ("CVX", "WETH"),
-    ("BAL", "WETH"),
-    ("RPL", "WETH"),
-    ("SNX", "WETH"),
-    ("YFI", "WETH"),
-    ("FXS", "WETH"),
-]
+PAIR_SUITES_BY_CHAIN: dict[int, dict[str, list[Pair]]] = {
+    1: {
+        "smoke": [
+            ("DAI", "USDC"),
+            ("WETH", "USDC"),
+            ("USDC", "USDT"),
+            ("WBTC", "WETH"),
+            ("STETH", "WETH"),
+        ],
+        "core": [
+            ("DAI", "USDC"),
+            ("USDC", "USDT"),
+            ("GHO", "USDC"),
+            ("WETH", "USDC"),
+            ("WETH", "USDT"),
+            ("WETH", "DAI"),
+            ("WBTC", "USDC"),
+            ("WBTC", "WETH"),
+            ("FRAX", "USDC"),
+            ("STETH", "WETH"),
+            ("WSTETH", "WETH"),
+            ("RETH", "WETH"),
+            ("ETH", "RETH"),
+            ("RETH", "ETH"),
+            ("CBETH", "WETH"),
+            ("UNI", "WETH"),
+            ("LINK", "WETH"),
+            ("AAVE", "WETH"),
+            ("COMP", "WETH"),
+            ("MKR", "WETH"),
+            ("SUSHI", "WETH"),
+            ("LDO", "WETH"),
+        ],
+        "extended": [
+            ("DAI", "USDC"),
+            ("USDC", "USDT"),
+            ("GHO", "USDC"),
+            ("WETH", "USDC"),
+            ("WETH", "USDT"),
+            ("WETH", "DAI"),
+            ("WBTC", "USDC"),
+            ("WBTC", "WETH"),
+            ("FRAX", "USDC"),
+            ("STETH", "WETH"),
+            ("WSTETH", "WETH"),
+            ("RETH", "WETH"),
+            ("ETH", "RETH"),
+            ("RETH", "ETH"),
+            ("CBETH", "WETH"),
+            ("UNI", "WETH"),
+            ("LINK", "WETH"),
+            ("AAVE", "WETH"),
+            ("COMP", "WETH"),
+            ("MKR", "WETH"),
+            ("SUSHI", "WETH"),
+            ("LDO", "WETH"),
+            ("LUSD", "USDC"),
+            ("FRXETH", "WETH"),
+            ("CRV", "WETH"),
+            ("CVX", "WETH"),
+            ("BAL", "WETH"),
+            ("RPL", "WETH"),
+            ("SNX", "WETH"),
+            ("YFI", "WETH"),
+            ("FXS", "WETH"),
+        ],
+        "stables": [
+            ("DAI", "USDC"),
+            ("DAI", "USDT"),
+            ("USDC", "USDT"),
+            ("GHO", "USDC"),
+            ("FRAX", "USDC"),
+        ],
+        "lst": [
+            ("STETH", "WETH"),
+            ("WSTETH", "WETH"),
+            ("RETH", "WETH"),
+            ("CBETH", "WETH"),
+        ],
+        "governance": [
+            ("UNI", "WETH"),
+            ("LINK", "WETH"),
+            ("AAVE", "WETH"),
+            ("COMP", "WETH"),
+            ("MKR", "WETH"),
+            ("SUSHI", "WETH"),
+            ("LDO", "WETH"),
+        ],
+        "v4_candidates": [
+            ("WETH", "USDC"),
+            ("WBTC", "USDC"),
+            ("ETH", "USDC"),
+        ],
+    },
+    8453: {
+        "smoke": [
+            ("USDC", "WETH"),
+            ("WETH", "USDC"),
+            ("ETH", "USDC"),
+            ("USDC", "ETH"),
+        ],
+        "core": [
+            ("USDC", "WETH"),
+            ("WETH", "USDC"),
+            ("ETH", "USDC"),
+            ("USDC", "ETH"),
+            ("DAI", "USDC"),
+            ("USDC", "DAI"),
+        ],
+        "extended": [
+            ("USDC", "WETH"),
+            ("WETH", "USDC"),
+            ("ETH", "USDC"),
+            ("USDC", "ETH"),
+            ("DAI", "USDC"),
+            ("USDC", "DAI"),
+        ],
+        "stables": [
+            ("DAI", "USDC"),
+            ("USDC", "DAI"),
+        ],
+        "lst": [
+            ("ETH", "WETH"),
+            ("WETH", "ETH"),
+        ],
+        "governance": [
+            ("WETH", "USDC"),
+        ],
+        "v4_candidates": [
+            ("WETH", "USDC"),
+            ("ETH", "USDC"),
+        ],
+    },
+}
 
-PAIR_SUITES: dict[str, list[Pair]] = {
-    "smoke": [
-        ("DAI", "USDC"),
-        ("WETH", "USDC"),
-        ("USDC", "USDT"),
-        ("WBTC", "WETH"),
-        ("STETH", "WETH"),
-    ],
-    "core": CORE_PAIRS,
-    "extended": CORE_PAIRS + LOW_LIQUIDITY_PAIRS,
-    "stables": [
-        ("DAI", "USDC"),
-        ("DAI", "USDT"),
-        ("USDC", "USDT"),
-        ("GHO", "USDC"),
-        ("FRAX", "USDC"),
-    ],
-    "lst": [
-        ("STETH", "WETH"),
-        ("WSTETH", "WETH"),
-        ("RETH", "WETH"),
-        ("CBETH", "WETH"),
-    ],
-    "governance": [
-        ("UNI", "WETH"),
-        ("LINK", "WETH"),
-        ("AAVE", "WETH"),
-        ("COMP", "WETH"),
-        ("MKR", "WETH"),
-        ("SUSHI", "WETH"),
-        ("LDO", "WETH"),
-    ],
-    "v4_candidates": [
-        ("WETH", "USDC"),
-        ("WBTC", "USDC"),
-        ("ETH", "USDC"),
-    ],
+DEFAULT_ENCODE_ROUTE_BY_CHAIN: dict[int, tuple[str, str, str]] = {
+    1: ("DAI", "USDC", "USDT"),
+    8453: ("USDC", "WETH", "USDC"),
 }
 
 
@@ -193,40 +326,78 @@ class ResolvedPair:
     token_out: str
 
 
-def resolve_token(token_or_symbol: str) -> str:
+def tokens_for_chain(chain_id: int) -> dict[str, str]:
+    return TOKENS_BY_CHAIN[chain_id]
+
+
+def default_encode_route(chain_id: int) -> tuple[str, str, str]:
+    route = DEFAULT_ENCODE_ROUTE_BY_CHAIN.get(chain_id)
+    if route is None:
+        raise ValueError(f"No default encode route configured for chain {chain_id}")
+    return route
+
+
+def resolve_token(token_or_symbol: str, chain_id: int) -> str:
     token_or_symbol = token_or_symbol.strip()
     if token_or_symbol.lower().startswith("0x"):
         return token_or_symbol.lower()
 
     symbol = token_or_symbol.upper()
-    address = TOKENS.get(symbol)
+    address = TOKENS_BY_CHAIN[chain_id].get(symbol)
     if not address:
-        raise ValueError(f"Unknown token symbol: {token_or_symbol}")
+        raise ValueError(
+            f"Unknown token symbol for chain {chain_id} ({chain_label(chain_id)}): {token_or_symbol}"
+        )
     return address
 
 
 def default_amounts(decimals: int) -> list[str]:
-    scale = 10 ** decimals
+    scale = 10**decimals
     return [str(amount * scale) for amount in BASE_AMOUNTS]
 
 
-def token_decimals(token_or_symbol: str) -> int:
+def token_decimals(token_or_symbol: str, chain_id: int) -> int:
     token_or_symbol = token_or_symbol.strip()
     if token_or_symbol.lower().startswith("0x"):
-        return ADDRESS_DECIMALS.get(token_or_symbol.lower(), 18)
-    symbol = token_or_symbol.upper()
-    return TOKEN_DECIMALS.get(symbol, 18)
+        symbol = None
+        token_address = token_or_symbol.lower()
+    else:
+        symbol = token_or_symbol.upper()
+        token_address = TOKENS_BY_CHAIN[chain_id].get(symbol, "").lower()
 
+    decimals_by_symbol = TOKEN_DECIMALS_BY_CHAIN.get(chain_id, {})
+    if symbol and symbol in decimals_by_symbol:
+        return decimals_by_symbol[symbol]
 
-def default_amounts_for_token(token_or_symbol: str) -> list[str]:
+    for known_symbol, known_address in TOKENS_BY_CHAIN[chain_id].items():
+        if known_address.lower() == token_address:
+            return decimals_by_symbol.get(known_symbol, 18)
+
+    return 18
+
+
+def default_amounts_for_token(token_or_symbol: str, chain_id: int) -> list[str]:
     token_or_symbol = token_or_symbol.strip()
     if token_or_symbol.lower().startswith("0x"):
-        base_units = ADDRESS_BASE_UNITS.get(token_or_symbol.lower())
+        address = token_or_symbol.lower()
+        symbol = None
     else:
-        base_units = TOKEN_BASE_UNITS.get(token_or_symbol.upper())
+        symbol = token_or_symbol.upper()
+        address = TOKENS_BY_CHAIN[chain_id].get(symbol, "").lower()
+
+    base_units = None
+    if symbol:
+        base_units = TOKEN_BASE_UNITS_BY_CHAIN.get(chain_id, {}).get(symbol)
+    if base_units is None and address:
+        for known_symbol, known_address in TOKENS_BY_CHAIN[chain_id].items():
+            if known_address.lower() == address:
+                base_units = TOKEN_BASE_UNITS_BY_CHAIN.get(chain_id, {}).get(known_symbol)
+                break
+
     if base_units is not None:
         return [str(value) for value in base_units]
-    return default_amounts(token_decimals(token_or_symbol))
+
+    return default_amounts(token_decimals(token_or_symbol, chain_id))
 
 
 def parse_amounts(amounts_csv: str | None) -> list[str]:
@@ -238,7 +409,7 @@ def parse_amounts(amounts_csv: str | None) -> list[str]:
     return amounts
 
 
-def parse_pairs(pair_args: list[str] | None, pairs_csv: str | None) -> list[ResolvedPair]:
+def parse_pairs(pair_args: list[str] | None, pairs_csv: str | None, chain_id: int) -> list[ResolvedPair]:
     pairs: list[str] = []
     if pairs_csv:
         pairs.extend([entry.strip() for entry in pairs_csv.split(",") if entry.strip()])
@@ -250,21 +421,24 @@ def parse_pairs(pair_args: list[str] | None, pairs_csv: str | None) -> list[Reso
         if ":" not in entry:
             raise ValueError(f"Invalid pair format: {entry} (expected token_in:token_out)")
         left, right = entry.split(":", 1)
-        resolved.append(ResolvedPair(resolve_token(left), resolve_token(right)))
+        resolved.append(ResolvedPair(resolve_token(left, chain_id), resolve_token(right, chain_id)))
     return resolved
 
 
-def suite_pairs(name: str) -> list[ResolvedPair]:
-    suite = PAIR_SUITES.get(name)
+def suite_pairs(name: str, chain_id: int) -> list[ResolvedPair]:
+    suites = PAIR_SUITES_BY_CHAIN.get(chain_id, {})
+    suite = suites.get(name)
     if suite is None:
-        known = ", ".join(sorted(PAIR_SUITES.keys()))
-        raise ValueError(f"Unknown suite: {name}. Known suites: {known}")
-    return [ResolvedPair(resolve_token(a), resolve_token(b)) for (a, b) in suite]
+        known = ", ".join(sorted(suites.keys()))
+        raise ValueError(
+            f"Unknown suite for chain {chain_id} ({chain_label(chain_id)}): {name}. Known suites: {known}"
+        )
+    return [ResolvedPair(resolve_token(a, chain_id), resolve_token(b, chain_id)) for (a, b) in suite]
 
 
-def list_suites() -> list[str]:
-    return sorted(PAIR_SUITES.keys())
+def list_suites(chain_id: int) -> list[str]:
+    return sorted(PAIR_SUITES_BY_CHAIN.get(chain_id, {}).keys())
 
 
-def list_tokens() -> list[tuple[str, str]]:
-    return sorted(TOKENS.items())
+def list_tokens(chain_id: int) -> list[tuple[str, str]]:
+    return sorted(TOKENS_BY_CHAIN.get(chain_id, {}).items())
````

## scripts/run_suite.sh
Change summary: Extends suite orchestrator with required chain resolution, VM readiness semantics, and allow-no-liquidity/allow-partial routing flags.
Risk level: High
Review focus: Validate chain-id resolution precedence, readiness gates, and option forwarding to script sub-steps.
Hunk context notes: This is the main operator entrypoint and now gates correctness by chain and effective VM availability; regressions directly affect release checks.
````diff
diff --git a/scripts/run_suite.sh b/scripts/run_suite.sh
index 16e3e51..921567d 100755
--- a/scripts/run_suite.sh
+++ b/scripts/run_suite.sh
@@ -3,7 +3,7 @@ set -euo pipefail
 
 usage() {
   cat <<'USAGE'
-Usage: run_suite.sh --repo <path> [--base-url <url>] [--suite <name>] [--disable-vm-pools] [--enable-vm-pools] [--wait-vm-ready] [--allow-no-liquidity] [--allow-partial] [--stop]
+Usage: run_suite.sh --repo <path> [--base-url <url>] [--chain-id <id>] [--suite <name>] [--disable-vm-pools] [--enable-vm-pools] [--wait-vm-ready] [--allow-no-liquidity] [--allow-partial] [--stop]
 
 Run a small end-to-end test suite:
 1) start server (if not already running)
@@ -14,23 +14,70 @@ Run a small end-to-end test suite:
 5) latency percentiles (p50/p90/p99)
 
 Options:
-  --repo             Repo root containing Cargo.toml
-  --base-url         Base URL (default: http://localhost:3000)
-  --suite            Pair suite for coverage/latency (default: core)
-  --disable-vm-pools Start server with ENABLE_VM_POOLS=false
-  --enable-vm-pools  Start server with ENABLE_VM_POOLS=true (default)
-  --wait-vm-ready    Wait for vm_status=ready after /status is ready (only when VM pools are enabled)
-  --allow-no-liquidity  Allow no_liquidity responses with only no_pools failures
-  --allow-partial    Allow partial_success responses (and their failures)
-  --stop             Stop server when done (only if started by this script)
-  -h, --help         Show this help
+  --repo               Repo root containing Cargo.toml
+  --base-url           Base URL (default: http://localhost:3000)
+  --chain-id           Runtime chain id (1 or 8453). Overrides CHAIN_ID env/.env.
+  --suite              Pair suite for coverage/latency (default: core)
+  --disable-vm-pools   Start server with ENABLE_VM_POOLS=false
+  --enable-vm-pools    Start server with ENABLE_VM_POOLS=true (default)
+  --wait-vm-ready      Wait for vm_status=ready after /status is ready (when VM is effectively enabled)
+  --allow-no-liquidity Allow no_liquidity responses with only no_pools failures
+  --allow-partial      Allow partial_success responses (and their failures)
+  --stop               Stop server when done (only if started by this script)
+  -h, --help           Show this help
 
 Tip: For mainnet variability, use --allow-partial --allow-no-liquidity for local runs.
 USAGE
 }
 
+validate_chain_id() {
+  case "$1" in
+    1|8453) ;;
+    *)
+      echo "Error: unsupported chain id '$1'. Supported values: 1 (Ethereum), 8453 (Base)." >&2
+      return 1
+      ;;
+  esac
+}
+
+load_chain_id_from_env_file() {
+  local env_file="$1"
+  if [[ ! -f "$env_file" ]]; then
+    return 1
+  fi
+  local raw
+  raw="$(grep -E '^[[:space:]]*(export[[:space:]]+)?CHAIN_ID[[:space:]]*=' "$env_file" | tail -n 1 || true)"
+  if [[ -z "$raw" ]]; then
+    return 1
+  fi
+  local value="${raw#*=}"
+  value="${value%%#*}"
+  value="${value#"${value%%[![:space:]]*}"}"
+  value="${value%"${value##*[![:space:]]}"}"
+  if [[ "$value" == \"*\" ]]; then
+    value="${value#\"}"
+    value="${value%\"}"
+  elif [[ "$value" == \'*\' ]]; then
+    value="${value#\'}"
+    value="${value%\'}"
+  fi
+  if [[ -z "$value" ]]; then
+    return 1
+  fi
+  echo "$value"
+}
+
+chain_label() {
+  case "$1" in
+    1) echo "ethereum" ;;
+    8453) echo "base" ;;
+    *) echo "chain-$1" ;;
+  esac
+}
+
 repo=""
 base_url="http://localhost:3000"
+chain_id_arg=""
 suite="core"
 enable_vm_pools="true"
 wait_vm_ready="false"
@@ -48,6 +95,10 @@ while [[ $# -gt 0 ]]; do
       base_url="$2"
       shift 2
       ;;
+    --chain-id)
+      chain_id_arg="$2"
+      shift 2
+      ;;
     --suite)
       suite="$2"
       shift 2
@@ -99,12 +150,25 @@ status_url="${base_url%/}/status"
 simulate_url="${base_url%/}/simulate"
 encode_url="${base_url%/}/encode"
 
+resolved_chain_id="${chain_id_arg:-${CHAIN_ID:-}}"
+if [[ -z "$resolved_chain_id" ]]; then
+  resolved_chain_id="$(load_chain_id_from_env_file "$repo/.env" || true)"
+fi
+if [[ -z "$resolved_chain_id" ]]; then
+  echo "Error: missing chain id. Pass --chain-id or set CHAIN_ID in env/.env." >&2
+  exit 2
+fi
+validate_chain_id "$resolved_chain_id"
+chain_id="$resolved_chain_id"
+chain_name="$(chain_label "$chain_id")"
+
 script_dir="$(cd "$(dirname "$0")" && pwd)"
 started_by_me="false"
 
 echo "Base URL: $base_url"
+echo "Chain: $chain_name ($chain_id)"
 echo "Suite: $suite"
-echo "Enable VM pools: $enable_vm_pools"
+echo "Enable VM pools (requested): $enable_vm_pools"
 echo "Wait VM ready: $wait_vm_ready"
 echo "Allow no_liquidity: $allow_no_liquidity"
 echo "Allow partial_success: $allow_partial"
@@ -112,107 +176,165 @@ echo "Allow partial_success: $allow_partial"
 simulate_allow_status="ready"
 coverage_allow_status="ready"
 latency_allow_status="ready"
-allow_failures_flag=""
-allow_no_pools_flag=""
+simulate_flags=()
+coverage_flags=()
+latency_flags=()
 
 if [[ "$allow_partial" == "true" ]]; then
   simulate_allow_status="${simulate_allow_status},partial_success"
   coverage_allow_status="${coverage_allow_status},partial_success"
   latency_allow_status="${latency_allow_status},partial_success"
-  allow_failures_flag="--allow-failures"
+  simulate_flags+=(--allow-failures)
+  coverage_flags+=(--allow-failures)
+  latency_flags+=(--allow-failures)
 fi
 
 if [[ "$allow_no_liquidity" == "true" ]]; then
   coverage_allow_status="${coverage_allow_status},no_liquidity"
   latency_allow_status="${latency_allow_status},no_liquidity"
-  allow_no_pools_flag="--allow-no-pools"
+  coverage_flags+=(--allow-no-pools)
+  latency_flags+=(--allow-no-pools)
 fi
 
 if curl -s "$status_url" >/dev/null 2>&1; then
   echo "Server already responding at $status_url"
 else
   echo "Starting server..."
+  start_server_args=(--repo "$repo" --chain-id "$chain_id")
   if [[ "$enable_vm_pools" == "true" ]]; then
-    "$script_dir/start_server.sh" --repo "$repo" --enable-vm-pools
+    start_server_args+=(--enable-vm-pools)
   elif [[ "$enable_vm_pools" == "false" ]]; then
-    "$script_dir/start_server.sh" --repo "$repo" --env ENABLE_VM_POOLS=false
-  else
-    "$script_dir/start_server.sh" --repo "$repo"
+    start_server_args+=(--env ENABLE_VM_POOLS=false)
   fi
+  "$script_dir/start_server.sh" "${start_server_args[@]}"
   started_by_me="true"
 fi
 
 echo "Waiting for readiness..."
 wait_timeout=300
-wait_args=(--url "$status_url" --timeout "$wait_timeout" --interval 2)
 if [[ "$enable_vm_pools" == "true" ]]; then
   # VM protocols can take much longer to ingest than native pools.
   wait_timeout=600
-  wait_args=(--url "$status_url" --timeout "$wait_timeout" --interval 2 --require-vm-ready --require-vm-pools-min 1)
 fi
-"$script_dir/wait_ready.sh" "${wait_args[@]}"
+"$script_dir/wait_ready.sh" \
+  --url "$status_url" \
+  --timeout "$wait_timeout" \
+  --interval 2 \
+  --expect-chain-id "$chain_id"
 
-if [[ "$enable_vm_pools" == "true" ]] && [[ "$wait_vm_ready" == "true" ]]; then
-  echo "Waiting for VM pool readiness..."
-  STATUS_URL="$status_url" python3 -u - <<'PY'
+# Resolve effective VM availability from runtime status so Base won't run VM-only assertions.
+runtime_vm_enabled="$(STATUS_URL="$status_url" python3 - <<'PY'
 import json
 import os
-import time
 import urllib.request
 
-status_url = os.environ["STATUS_URL"]
-deadline = time.time() + 300
-
-while True:
-    with urllib.request.urlopen(status_url, timeout=5) as r:
-        s = json.loads(r.read().decode())
-    vm_enabled = s.get("vm_enabled")
-    vm_status = s.get("vm_status")
-    vm_pools = s.get("vm_pools")
-    if not vm_enabled:
-        break
-    if vm_status == "ready":
-        break
-    if time.time() > deadline:
-        raise SystemExit(f"timeout waiting for vm ready (vm_status={vm_status} vm_pools={vm_pools})")
-    time.sleep(5)
+with urllib.request.urlopen(os.environ["STATUS_URL"], timeout=5) as response:
+    status = json.loads(response.read().decode())
+
+print("true" if bool(status.get("vm_enabled")) else "false")
 PY
+)"
+
+echo "Runtime VM enabled (effective): $runtime_vm_enabled"
+
+if [[ "$wait_vm_ready" == "true" ]]; then
+  if [[ "$runtime_vm_enabled" == "true" ]]; then
+    echo "Waiting for VM pool readiness..."
+    "$script_dir/wait_ready.sh" \
+      --url "$status_url" \
+      --timeout 600 \
+      --interval 2 \
+      --expect-chain-id "$chain_id" \
+      --require-vm-ready \
+      --require-vm-pools-min 1
+  else
+    echo "Skipping --wait-vm-ready because runtime VM is effectively disabled on this chain/config."
+  fi
 fi
 
 echo "Smoke testing /simulate..."
-python3 "$script_dir/simulate_smoke.py" --url "$simulate_url" --suite smoke --allow-status "$simulate_allow_status" --require-data --validate-data $allow_failures_flag
+python3 "$script_dir/simulate_smoke.py" \
+  --url "$simulate_url" \
+  --chain-id "$chain_id" \
+  --suite smoke \
+  --allow-status "$simulate_allow_status" \
+  --require-data \
+  --validate-data \
+  "${simulate_flags[@]}"
 
 echo "Encode smoke testing..."
-python3 "$script_dir/encode_smoke.py" --encode-url "$encode_url" --simulate-url "$simulate_url" --repo "$repo" --allow-status "$simulate_allow_status" $allow_failures_flag
+python3 "$script_dir/encode_smoke.py" \
+  --encode-url "$encode_url" \
+  --simulate-url "$simulate_url" \
+  --chain-id "$chain_id" \
+  --repo "$repo" \
+  --allow-status "$simulate_allow_status" \
+  "${simulate_flags[@]}"
 
 echo "Coverage sweep..."
 mkdir -p "$repo/logs"
-python3 "$script_dir/coverage_sweep.py" --url "$simulate_url" --suite "$suite" --allow-status "$coverage_allow_status" $allow_failures_flag $allow_no_pools_flag --out "$repo/logs/coverage_sweep.json"
+python3 "$script_dir/coverage_sweep.py" \
+  --url "$simulate_url" \
+  --chain-id "$chain_id" \
+  --suite "$suite" \
+  --allow-status "$coverage_allow_status" \
+  "${coverage_flags[@]}" \
+  --out "$repo/logs/coverage_sweep.json"
 
-if [[ "$enable_vm_pools" == "true" ]]; then
-  # Keep this check tied to protocols that are consistently surfaced by the probe pairs.
-  # Rocketpool/Ekubo-v3 can legitimately be absent for these pairs, making the suite flaky.
-  echo "Protocol presence checks (Maverick)..."
-  python3 "$script_dir/coverage_sweep.py" \
-    --url "$simulate_url" \
-    --pair USDC:USDT \
-    --pair USDT:USDC \
-    --pair ETH:RETH \
-    --allow-status "$coverage_allow_status" \
-    $allow_failures_flag \
-    $allow_no_pools_flag \
-    --expect-protocols maverick_v2 \
-    --out "$repo/logs/coverage_protocol_presence.json"
+if [[ "$runtime_vm_enabled" == "true" ]]; then
+  expected_vm_protocols=""
+  case "$chain_id" in
+    1)
+      expected_vm_protocols="maverick_v2"
+      ;;
+    8453)
+      expected_vm_protocols=""
+      ;;
+  esac
+
+  if [[ -n "$expected_vm_protocols" ]]; then
+    echo "Ensuring VM readiness for strict VM protocol checks..."
+    "$script_dir/wait_ready.sh" \
+      --url "$status_url" \
+      --timeout 600 \
+      --interval 2 \
+      --expect-chain-id "$chain_id" \
+      --require-vm-ready \
+      --require-vm-pools-min 1
+
+    echo "Protocol presence checks (chain-aware VM expectations)..."
+    python3 "$script_dir/coverage_sweep.py" \
+      --url "$simulate_url" \
+      --chain-id "$chain_id" \
+      --pair USDC:USDT \
+      --pair USDT:USDC \
+      --pair ETH:RETH \
+      --allow-status "$coverage_allow_status" \
+      "${coverage_flags[@]}" \
+      --expect-protocols "$expected_vm_protocols" \
+      --out "$repo/logs/coverage_protocol_presence.json"
+  else
+    echo "No chain-specific VM protocol expectation configured for chain $chain_id; skipping strict VM protocol gate."
+  fi
+else
+  echo "Skipping VM protocol presence checks (runtime VM is effectively disabled)."
 fi
 
 echo "Latency percentiles..."
 latency_requests="${LATENCY_REQUESTS:-200}"
-if [[ "$enable_vm_pools" == "true" ]]; then
+if [[ "$runtime_vm_enabled" == "true" ]]; then
   latency_concurrency="${LATENCY_CONCURRENCY_VM:-4}"
 else
   latency_concurrency="${LATENCY_CONCURRENCY:-8}"
 fi
-python3 "$script_dir/latency_percentiles.py" --url "$simulate_url" --suite "$suite" --requests "$latency_requests" --concurrency "$latency_concurrency" --allow-status "$latency_allow_status" $allow_failures_flag $allow_no_pools_flag
+python3 "$script_dir/latency_percentiles.py" \
+  --url "$simulate_url" \
+  --chain-id "$chain_id" \
+  --suite "$suite" \
+  --requests "$latency_requests" \
+  --concurrency "$latency_concurrency" \
+  --allow-status "$latency_allow_status" \
+  "${latency_flags[@]}"
 
 if [[ "$stop_after" == "true" ]] && [[ "$started_by_me" == "true" ]]; then
   echo "Stopping server..."
````

## scripts/start_server.sh
Change summary: Extends operational script behavior for explicit chain context and safer readiness/smoke handling.
Risk level: Medium
Review focus: Review CLI flags, env precedence, and backward-compatible defaults expected by operators.
Hunk context notes: Script regressions can fail CI/prod runbooks even when service code is healthy.
````diff
diff --git a/scripts/start_server.sh b/scripts/start_server.sh
index 97698e8..93a00b1 100755
--- a/scripts/start_server.sh
+++ b/scripts/start_server.sh
@@ -3,21 +3,33 @@ set -euo pipefail
 
 usage() {
   cat <<'USAGE'
-Usage: start_server.sh [--repo <path>] [--log-file <path>] [--env KEY=VALUE] [--enable-vm-pools]
+Usage: start_server.sh [--repo <path>] [--log-file <path>] [--chain-id <id>] [--env KEY=VALUE] [--enable-vm-pools]
 
 Start the tycho-simulation-server from a repo checkout.
 
 Options:
   --repo             Path to repo root (default: current directory)
   --log-file         Log file path (default: <repo>/logs/tycho-sim-server.log)
+  --chain-id         Runtime chain id (1 or 8453). Overrides CHAIN_ID from env/.env.
   --env              Export KEY=VALUE before starting (repeatable)
   --enable-vm-pools  Shortcut for --env ENABLE_VM_POOLS=true
   -h, --help         Show this help
 USAGE
 }
 
+validate_chain_id() {
+  case "$1" in
+    1|8453) ;;
+    *)
+      echo "Error: unsupported chain id '$1'. Supported values: 1 (Ethereum), 8453 (Base)." >&2
+      return 1
+      ;;
+  esac
+}
+
 repo="."
 log_file=""
+chain_id_arg=""
 env_overrides=()
 
 while [[ $# -gt 0 ]]; do
@@ -30,6 +42,10 @@ while [[ $# -gt 0 ]]; do
       log_file="$2"
       shift 2
       ;;
+    --chain-id)
+      chain_id_arg="$2"
+      shift 2
+      ;;
     --env)
       env_overrides+=("$2")
       shift 2
@@ -90,6 +106,16 @@ if ((${#env_overrides[@]})); then
   done
 fi
 
+if [[ -n "$chain_id_arg" ]]; then
+  export CHAIN_ID="$chain_id_arg"
+fi
+
+if [[ -z "${CHAIN_ID:-}" ]]; then
+  echo "Error: missing chain id. Pass --chain-id or set CHAIN_ID in env/.env." >&2
+  exit 2
+fi
+validate_chain_id "$CHAIN_ID"
+
 if [[ -z "$log_file" ]]; then
   mkdir -p "$repo/logs"
   log_file="$repo/logs/tycho-sim-server.log"
@@ -102,6 +128,7 @@ fi
 )
 
 echo "Started tycho-simulation-server."
+echo "Chain ID: $CHAIN_ID"
 echo "PID: $(cat "$pid_file")"
 echo "Log: $log_file"
 echo "Tip: tail -f $log_file"
````

## scripts/wait_ready.sh
Change summary: Adds structured JSON readiness validation with optional chain-id and VM readiness constraints.
Risk level: High
Review focus: Check the Python exit-code contract and shell error handling paths, especially chain mismatch fast-fail.
Hunk context notes: Incorrect readiness parsing can falsely mark deployments healthy or mask wrong-chain targets.
````diff
diff --git a/scripts/wait_ready.sh b/scripts/wait_ready.sh
index 33de6d4..b3f560a 100755
--- a/scripts/wait_ready.sh
+++ b/scripts/wait_ready.sh
@@ -3,23 +3,25 @@ set -euo pipefail
 
 usage() {
   cat <<'USAGE'
-Usage: wait_ready.sh [--url <status_url>] [--timeout <seconds>] [--interval <seconds>] [--require-vm-ready] [--require-vm-pools-min <count>]
+Usage: wait_ready.sh [--url <status_url>] [--timeout <seconds>] [--interval <seconds>] [--expect-chain-id <id>] [--require-vm-ready] [--require-vm-pools-min <count>]
 
 Poll the /status endpoint until it returns ready or times out.
 
 Options:
-  --url        Status URL (default: http://localhost:3000/status)
-  --timeout    Timeout in seconds (default: 180)
-  --interval   Poll interval in seconds (default: 2)
-  --require-vm-ready    Require vm_status=ready before succeeding
-  --require-vm-pools-min  Minimum vm_pools required when --require-vm-ready is set (default: 1)
-  -h, --help   Show this help
+  --url                  Status URL (default: http://localhost:3000/status)
+  --timeout              Timeout in seconds (default: 180)
+  --interval             Poll interval in seconds (default: 2)
+  --expect-chain-id      Require /status.chain_id to match this chain id
+  --require-vm-ready     Require vm_status=ready before succeeding
+  --require-vm-pools-min Minimum vm_pools required when --require-vm-ready is set (default: 1)
+  -h, --help             Show this help
 USAGE
 }
 
 url="http://localhost:3000/status"
 timeout=180
 interval=2
+expect_chain_id=""
 require_vm_ready="false"
 require_vm_pools_min=1
 
@@ -37,6 +39,10 @@ while [[ $# -gt 0 ]]; do
       interval="$2"
       shift 2
       ;;
+    --expect-chain-id)
+      expect_chain_id="$2"
+      shift 2
+      ;;
     --require-vm-ready)
       require_vm_ready="true"
       shift 1
@@ -65,30 +71,46 @@ while true; do
   status_code="${response##*$'\n'}"
 
   if [[ "$status_code" == "200" ]]; then
-    if python3 -c '
+    if check_output="$(python3 -c '
 import json
 import sys
 
 require_vm = sys.argv[1] == "true"
 vm_pools_min = int(sys.argv[2])
+expected_chain_raw = sys.argv[3].strip()
+expected_chain = int(expected_chain_raw) if expected_chain_raw else None
 
 try:
     payload = json.load(sys.stdin)
 except Exception:
-    raise SystemExit(1)
+    raise SystemExit(2)
 
 if payload.get("status") != "ready":
-    raise SystemExit(1)
+    raise SystemExit(2)
+
+if expected_chain is not None:
+    actual_chain = payload.get("chain_id")
+    if actual_chain != expected_chain:
+        print(f"expected chain_id={expected_chain}, got {actual_chain}")
+        raise SystemExit(42)
 
 if require_vm:
+    if not payload.get("vm_enabled"):
+        raise SystemExit(2)
     if payload.get("vm_status") != "ready":
-        raise SystemExit(1)
+        raise SystemExit(2)
     if int(payload.get("vm_pools") or 0) < vm_pools_min:
-        raise SystemExit(1)
-' "$require_vm_ready" "$require_vm_pools_min" <<<"$body"
+        raise SystemExit(2)
+' "$require_vm_ready" "$require_vm_pools_min" "$expect_chain_id" <<<"$body" 2>&1)"
     then
       echo "ready"
       exit 0
+    else
+      check_code=$?
+      if [[ "$check_code" -eq 42 ]]; then
+        echo "Chain mismatch while waiting for readiness: $check_output" >&2
+        exit 1
+      fi
     fi
   fi
 
````

## scripts/simulate_smoke.py
Change summary: Extends operational script behavior for explicit chain context and safer readiness/smoke handling.
Risk level: Medium
Review focus: Review CLI flags, env precedence, and backward-compatible defaults expected by operators.
Hunk context notes: Script regressions can fail CI/prod runbooks even when service code is healthy.
````diff
diff --git a/scripts/simulate_smoke.py b/scripts/simulate_smoke.py
index 0d89d27..e2d273e 100755
--- a/scripts/simulate_smoke.py
+++ b/scripts/simulate_smoke.py
@@ -12,17 +12,17 @@ from urllib import request
 from urllib.error import HTTPError, URLError
 
 from presets import (
-    TOKENS,
+    chain_label,
     default_amounts_for_token,
     list_suites,
     list_tokens,
     parse_amounts,
     parse_pairs,
+    resolve_chain_id,
     suite_pairs,
+    tokens_for_chain,
 )
 
-ADDRESS_TO_SYMBOL = {address: symbol for (symbol, address) in TOKENS.items()}
-
 
 def request_simulate(url, payload, timeout):
     data = json.dumps(payload).encode("utf-8")
@@ -34,9 +34,9 @@ def request_simulate(url, payload, timeout):
         return response.status, body, elapsed
 
 
-def fmt_token(address: str) -> str:
+def fmt_token(address: str, address_to_symbol: dict[str, str]) -> str:
     address = address.lower()
-    symbol = ADDRESS_TO_SYMBOL.get(address)
+    symbol = address_to_symbol.get(address)
     if symbol:
         return symbol
     return address
@@ -85,6 +85,7 @@ def validate_pool_entry(entry, expected_len: int) -> tuple[bool, str]:
 def main() -> int:
     parser = argparse.ArgumentParser(description="Smoke test POST /simulate")
     parser.add_argument("--url", default="http://localhost:3000/simulate")
+    parser.add_argument("--chain-id", help="Runtime chain id (1 or 8453); overrides CHAIN_ID env")
     parser.add_argument("--suite", default="smoke", help="Named pair suite from presets")
     parser.add_argument("--pair", action="append", help="token_in:token_out (symbol or address)")
     parser.add_argument("--pairs", help="Comma-separated token_in:token_out pairs")
@@ -109,25 +110,35 @@ def main() -> int:
 
     args = parser.parse_args()
 
+    try:
+        chain_id = resolve_chain_id(args.chain_id)
+    except ValueError as exc:
+        print(f"Error: {exc}", file=sys.stderr)
+        return 2
+
+    address_to_symbol = {
+        address.lower(): symbol for (symbol, address) in tokens_for_chain(chain_id).items()
+    }
+
     if args.list_suites:
-        for name in list_suites():
+        for name in list_suites(chain_id):
             print(name)
         return 0
 
     if args.list_tokens:
-        for symbol, address in list_tokens():
+        for symbol, address in list_tokens(chain_id):
             print(f"{symbol} {address}")
         return 0
 
     try:
-        pairs = parse_pairs(args.pair, args.pairs)
+        pairs = parse_pairs(args.pair, args.pairs, chain_id)
     except ValueError as exc:
         print(f"Error: {exc}", file=sys.stderr)
         return 2
 
     if not pairs:
         try:
-            pairs = suite_pairs(args.suite)
+            pairs = suite_pairs(args.suite, chain_id)
         except ValueError as exc:
             print(f"Error: {exc}", file=sys.stderr)
             return 2
@@ -148,7 +159,7 @@ def main() -> int:
     for idx, pair in enumerate(pairs, start=1):
         token_in = pair.token_in
         token_out = pair.token_out
-        amounts = amounts_override or default_amounts_for_token(token_in)
+        amounts = amounts_override or default_amounts_for_token(token_in, chain_id)
         payload = {
             "request_id": f"{args.request_id_prefix}-{idx}-{uuid.uuid4().hex[:8]}",
             "token_in": token_in,
@@ -175,7 +186,7 @@ def main() -> int:
             failures += 1
             continue
 
-        pair_label = f"{fmt_token(token_in)}->{fmt_token(token_out)}"
+        pair_label = f"{fmt_token(token_in, address_to_symbol)}->{fmt_token(token_out, address_to_symbol)}"
 
         if status_code != 200:
             print(f"[FAIL] {pair_label} HTTP {status_code}")
@@ -234,7 +245,7 @@ def main() -> int:
 
     total = len(pairs)
     success = total - failures
-    print(f"\nSummary: {success}/{total} successful")
+    print(f"\nSummary ({chain_label(chain_id)}:{chain_id}): {success}/{total} successful")
     return 0 if failures == 0 else 1
 
 
````

## scripts/coverage_sweep.py
Change summary: Extends operational script behavior for explicit chain context and safer readiness/smoke handling.
Risk level: Medium
Review focus: Review CLI flags, env precedence, and backward-compatible defaults expected by operators.
Hunk context notes: Script regressions can fail CI/prod runbooks even when service code is healthy.
````diff
diff --git a/scripts/coverage_sweep.py b/scripts/coverage_sweep.py
index 5996e61..e209f84 100755
--- a/scripts/coverage_sweep.py
+++ b/scripts/coverage_sweep.py
@@ -14,7 +14,16 @@ from pathlib import Path
 from urllib import request
 from urllib.error import HTTPError, URLError
 
-from presets import default_amounts_for_token, list_suites, list_tokens, parse_amounts, parse_pairs, suite_pairs
+from presets import (
+    chain_label,
+    default_amounts_for_token,
+    list_suites,
+    list_tokens,
+    parse_amounts,
+    parse_pairs,
+    resolve_chain_id,
+    suite_pairs,
+)
 
 
 def request_simulate(url: str, payload: dict, timeout: float) -> tuple[int, bytes, float]:
@@ -43,6 +52,7 @@ def protocol_from_pool_name(pool_name: str | None) -> str:
 def main() -> int:
     parser = argparse.ArgumentParser(description="Coverage sweep for POST /simulate")
     parser.add_argument("--url", default="http://localhost:3000/simulate")
+    parser.add_argument("--chain-id", help="Runtime chain id (1 or 8453); overrides CHAIN_ID env")
     parser.add_argument("--suite", default="core", help="Named pair suite from presets")
     parser.add_argument("--pair", action="append", help="token_in:token_out (symbol or address)")
     parser.add_argument("--pairs", help="Comma-separated token_in:token_out pairs")
@@ -64,20 +74,26 @@ def main() -> int:
 
     args = parser.parse_args()
 
+    try:
+        chain_id = resolve_chain_id(args.chain_id)
+    except ValueError as exc:
+        print(f"Error: {exc}", file=sys.stderr)
+        return 2
+
     if args.list_suites:
-        for name in list_suites():
+        for name in list_suites(chain_id):
             print(name)
         return 0
 
     if args.list_tokens:
-        for symbol, address in list_tokens():
+        for symbol, address in list_tokens(chain_id):
             print(f"{symbol} {address}")
         return 0
 
     try:
-        pairs = parse_pairs(args.pair, args.pairs)
+        pairs = parse_pairs(args.pair, args.pairs, chain_id)
         if not pairs:
-            pairs = suite_pairs(args.suite)
+            pairs = suite_pairs(args.suite, chain_id)
         amounts_override = parse_amounts(args.amounts) if args.amounts else None
     except ValueError as exc:
         print(f"Error: {exc}", file=sys.stderr)
@@ -93,6 +109,8 @@ def main() -> int:
         expected_protocols = {p.strip().lower() for p in args.expect_protocols.split(",") if p.strip()}
 
     report = {
+        "chain_id": chain_id,
+        "chain": chain_label(chain_id),
         "url": args.url,
         "suite": args.suite,
         "pairs": [asdict(p) for p in pairs],
@@ -112,7 +130,7 @@ def main() -> int:
     allowed_no_pools = 0
 
     for idx, pair in enumerate(pairs, start=1):
-        amounts = amounts_override or default_amounts_for_token(pair.token_in)
+        amounts = amounts_override or default_amounts_for_token(pair.token_in, chain_id)
         payload = {
             "request_id": f"{args.request_id_prefix}-{idx}-{uuid.uuid4().hex[:8]}",
             "token_in": pair.token_in,
@@ -234,7 +252,13 @@ def main() -> int:
                     )
 
         report["requests"].append(
-            {"pair": asdict(pair), "elapsed_s": elapsed, "meta": meta, "pool_count": pool_count, "amounts": amounts}
+            {
+                "pair": asdict(pair),
+                "elapsed_s": elapsed,
+                "meta": meta,
+                "pool_count": pool_count,
+                "amounts": amounts,
+            }
         )
 
         if args.verbose:
@@ -261,7 +285,7 @@ def main() -> int:
         pools_seen.values(), key=lambda item: (item.get("protocol", ""), item.get("pool_name", ""))
     )
 
-    print("Coverage sweep summary")
+    print(f"Coverage sweep summary ({chain_label(chain_id)}:{chain_id})")
     print("=====================")
     print(f"Pairs: {len(pairs)}")
     print(f"Failures: {failures}")
````

## scripts/latency_percentiles.py
Change summary: Extends operational script behavior for explicit chain context and safer readiness/smoke handling.
Risk level: Medium
Review focus: Review CLI flags, env precedence, and backward-compatible defaults expected by operators.
Hunk context notes: Script regressions can fail CI/prod runbooks even when service code is healthy.
````diff
diff --git a/scripts/latency_percentiles.py b/scripts/latency_percentiles.py
index 920e73a..8a37950 100755
--- a/scripts/latency_percentiles.py
+++ b/scripts/latency_percentiles.py
@@ -18,14 +18,16 @@ from urllib import request
 from urllib.error import HTTPError, URLError
 
 from presets import (
-    TOKENS,
+    chain_label,
     default_amounts_for_token,
     list_suites,
     list_tokens,
     parse_amounts,
     parse_pairs,
+    resolve_chain_id,
     resolve_token,
     suite_pairs,
+    tokens_for_chain,
 )
 
 
@@ -53,18 +55,19 @@ def request_simulate(url, payload, timeout):
         return response.status, body, elapsed
 
 
-def parse_tokens(tokens_csv: str | None) -> list[str]:
+def parse_tokens(tokens_csv: str | None, chain_id: int) -> list[str]:
     if not tokens_csv:
-        return list(TOKENS.values())
+        return list(tokens_for_chain(chain_id).values())
     tokens = [token.strip() for token in tokens_csv.split(",") if token.strip()]
     if len(tokens) < 2:
         raise ValueError("Provide at least two tokens")
-    return [resolve_token(token) for token in tokens]
+    return [resolve_token(token, chain_id) for token in tokens]
 
 
 def main() -> int:
     parser = argparse.ArgumentParser(description="Latency percentiles for POST /simulate")
     parser.add_argument("--url", default="http://localhost:3000/simulate")
+    parser.add_argument("--chain-id", help="Runtime chain id (1 or 8453); overrides CHAIN_ID env")
     parser.add_argument("--requests", type=int, default=200)
     parser.add_argument("--concurrency", type=int, default=50)
     parser.add_argument("--suite", default="core", help="Named pair suite from presets (used unless --random)")
@@ -89,13 +92,19 @@ def main() -> int:
 
     args = parser.parse_args()
 
+    try:
+        chain_id = resolve_chain_id(args.chain_id)
+    except ValueError as exc:
+        print(f"Error: {exc}", file=sys.stderr)
+        return 2
+
     if args.list_suites:
-        for name in list_suites():
+        for name in list_suites(chain_id):
             print(name)
         return 0
 
     if args.list_tokens:
-        for symbol, address in list_tokens():
+        for symbol, address in list_tokens(chain_id):
             print(f"{symbol} {address}")
         return 0
 
@@ -110,9 +119,9 @@ def main() -> int:
         random.seed(args.seed)
 
     try:
-        explicit_pairs = parse_pairs(args.pair, args.pairs)
+        explicit_pairs = parse_pairs(args.pair, args.pairs, chain_id)
         amounts_override = parse_amounts(args.amounts) if args.amounts else None
-        tokens = parse_tokens(args.tokens)
+        tokens = parse_tokens(args.tokens, chain_id)
     except ValueError as exc:
         print(f"Error: {exc}", file=sys.stderr)
         return 2
@@ -131,7 +140,7 @@ def main() -> int:
         pairs = explicit_pairs
     elif not args.random:
         try:
-            pairs = suite_pairs(args.suite)
+            pairs = suite_pairs(args.suite, chain_id)
         except ValueError as exc:
             print(f"Error: {exc}", file=sys.stderr)
             return 2
@@ -143,7 +152,7 @@ def main() -> int:
         return tuple(random.sample(tokens, 2))
 
     def amounts_for_token(token_in: str) -> list[str]:
-        return amounts_override or default_amounts_for_token(token_in)
+        return amounts_override or default_amounts_for_token(token_in, chain_id)
 
     if args.dry_run:
         sample_pair = pick_pair()
@@ -157,6 +166,8 @@ def main() -> int:
         print(
             json.dumps(
                 {
+                    "chain_id": chain_id,
+                    "chain": chain_label(chain_id),
                     "url": args.url,
                     "requests": args.requests,
                     "concurrency": args.concurrency,
@@ -237,7 +248,7 @@ def main() -> int:
     total_time = time.perf_counter() - start_time
 
     if not response_times:
-        print("No successful requests.")
+        print(f"No successful requests ({chain_label(chain_id)}:{chain_id}).")
         print(f"Failures: {failures}")
         return 1
 
@@ -247,7 +258,7 @@ def main() -> int:
     p90 = percentile(response_times, 0.90)
     p99 = percentile(response_times, 0.99)
 
-    print("Latency results (seconds)")
+    print(f"Latency results ({chain_label(chain_id)}:{chain_id})")
     print("========================")
     print(f"Requests: {args.requests}")
     print(f"Successes: {len(response_times)}")
````

## scripts/encode_smoke.py
Change summary: Makes encode smoke chain-aware, uses route defaults per chain, and propagates symbolic labels in failure output.
Risk level: Medium
Review focus: Verify route construction remains valid on both supported chains and all token resolutions are deterministic.
Hunk context notes: Encode smoke is now part of chain bring-up; bad defaults can produce misleading "service broken" signals.
````diff
diff --git a/scripts/encode_smoke.py b/scripts/encode_smoke.py
index bed27ce..657e3b3 100755
--- a/scripts/encode_smoke.py
+++ b/scripts/encode_smoke.py
@@ -13,7 +13,13 @@ from pathlib import Path
 from urllib import request
 from urllib.error import HTTPError, URLError
 
-from presets import TOKENS, default_amounts_for_token
+from presets import (
+    chain_label,
+    default_amounts_for_token,
+    default_encode_route,
+    resolve_chain_id,
+    resolve_token,
+)
 
 DEFAULT_SETTLEMENT = "0x9008D19f58AAbD9eD0D60971565AA8510560ab41"
 DEFAULT_TYCHO_ROUTER = "0xfD0b31d2E955fA55e3fa641Fe90e08b677188d35"
@@ -134,6 +140,7 @@ def main() -> int:
     parser = argparse.ArgumentParser(description="Smoke test POST /encode (spec schema)")
     parser.add_argument("--encode-url", default="http://localhost:3000/encode")
     parser.add_argument("--simulate-url", default="http://localhost:3000/simulate")
+    parser.add_argument("--chain-id", help="Runtime chain id (1 or 8453); overrides CHAIN_ID env")
     parser.add_argument("--repo", default=".")
     parser.add_argument("--timeout", type=float, default=15.0)
     parser.add_argument("--allow-status", default="ready", help="Comma-separated allowed meta.status values")
@@ -142,6 +149,12 @@ def main() -> int:
 
     args = parser.parse_args()
 
+    try:
+        chain_id = resolve_chain_id(args.chain_id)
+    except ValueError as exc:
+        print(f"Error: {exc}", file=sys.stderr)
+        return 2
+
     repo = Path(args.repo)
     settlement = load_env_value(repo, "COW_SETTLEMENT_CONTRACT") or DEFAULT_SETTLEMENT
     tycho_router = load_env_value(repo, "TYCHO_ROUTER_ADDRESS") or DEFAULT_TYCHO_ROUTER
@@ -150,19 +163,17 @@ def main() -> int:
         print("Error: --allow-status produced no values", file=sys.stderr)
         return 2
 
-    dai = TOKENS.get("DAI")
-    usdc = TOKENS.get("USDC")
-    usdt = TOKENS.get("USDT")
-    if not dai or not usdc or not usdt:
-        print("Error: DAI/USDC/USDT must be present in presets", file=sys.stderr)
-        return 2
+    token_in_symbol, mid_symbol, token_out_symbol = default_encode_route(chain_id)
+    token_in = resolve_token(token_in_symbol, chain_id)
+    mid_token = resolve_token(mid_symbol, chain_id)
+    token_out = resolve_token(token_out_symbol, chain_id)
 
-    amounts = default_amounts_for_token(dai)
+    amounts = default_amounts_for_token(token_in, chain_id)
 
     simulate_payload = {
         "request_id": f"encode-smoke-{uuid.uuid4().hex[:8]}",
-        "token_in": dai,
-        "token_out": usdc,
+        "token_in": token_in,
+        "token_out": mid_token,
         "amounts": amounts,
     }
 
@@ -180,20 +191,22 @@ def main() -> int:
     status_value = meta.get("status") if isinstance(meta, dict) else None
     failures_list = meta.get("failures", []) if isinstance(meta, dict) else []
     if status_value not in allowed_statuses:
-        print(f"[FAIL] simulate dai->usdc expected {sorted(allowed_statuses)}, got {status_value}")
+        print(
+            f"[FAIL] simulate {token_in_symbol}->{mid_symbol} expected {sorted(allowed_statuses)}, got {status_value}"
+        )
         return 1
     if failures_list and not args.allow_failures:
-        print(f"[FAIL] simulate dai->usdc had {len(failures_list)} failures")
+        print(f"[FAIL] simulate {token_in_symbol}->{mid_symbol} had {len(failures_list)} failures")
         return 1
 
-    pool_first = select_pool(response, "simulate dai->usdc")
+    pool_first = select_pool(response, f"simulate {token_in_symbol}->{mid_symbol}")
 
     hop_amounts_out = pool_first["amounts_out"]
     hop_amounts_in = [apply_slippage(value, DEFAULT_SLIPPAGE_BPS) for value in hop_amounts_out]
     simulate_payload_second = {
         "request_id": f"encode-smoke-hop-{uuid.uuid4().hex[:8]}",
-        "token_in": usdc,
-        "token_out": usdt,
+        "token_in": mid_token,
+        "token_out": token_out,
         "amounts": hop_amounts_in,
     }
 
@@ -213,13 +226,15 @@ def main() -> int:
     status_value = meta.get("status") if isinstance(meta, dict) else None
     failures_list = meta.get("failures", []) if isinstance(meta, dict) else []
     if status_value not in allowed_statuses:
-        print(f"[FAIL] simulate usdc->usdt expected {sorted(allowed_statuses)}, got {status_value}")
+        print(
+            f"[FAIL] simulate {mid_symbol}->{token_out_symbol} expected {sorted(allowed_statuses)}, got {status_value}"
+        )
         return 1
     if failures_list and not args.allow_failures:
-        print(f"[FAIL] simulate usdc->usdt had {len(failures_list)} failures")
+        print(f"[FAIL] simulate {mid_symbol}->{token_out_symbol} had {len(failures_list)} failures")
         return 1
 
-    pool_second = select_pool(response_second, "simulate usdc->usdt")
+    pool_second = select_pool(response_second, f"simulate {mid_symbol}->{token_out_symbol}")
 
     protocol_first = protocol_from_pool_name(pool_first.get("pool_name", ""))
     protocol_second = protocol_from_pool_name(pool_second.get("pool_name", ""))
@@ -234,9 +249,9 @@ def main() -> int:
         return 1
 
     encode_request = {
-        "chainId": 1,
-        "tokenIn": dai,
-        "tokenOut": usdt,
+        "chainId": chain_id,
+        "tokenIn": token_in,
+        "tokenOut": token_out,
         "amountIn": amounts[0],
         "minAmountOut": min_out_second,
         "settlementAddress": settlement,
@@ -248,8 +263,8 @@ def main() -> int:
                 "shareBps": 0,
                 "hops": [
                     {
-                        "tokenIn": dai,
-                        "tokenOut": usdc,
+                        "tokenIn": token_in,
+                        "tokenOut": mid_token,
                         "swaps": [
                             {
                                 "pool": {
@@ -257,15 +272,15 @@ def main() -> int:
                                     "componentId": pool_first["pool"],
                                     "poolAddress": pool_first["pool_address"],
                                 },
-                                "tokenIn": dai,
-                                "tokenOut": usdc,
+                                "tokenIn": token_in,
+                                "tokenOut": mid_token,
                                 "splitBps": 0,
                             }
                         ],
                     },
                     {
-                        "tokenIn": usdc,
-                        "tokenOut": usdt,
+                        "tokenIn": mid_token,
+                        "tokenOut": token_out,
                         "swaps": [
                             {
                                 "pool": {
@@ -273,8 +288,8 @@ def main() -> int:
                                     "componentId": pool_second["pool"],
                                     "poolAddress": pool_second["pool_address"],
                                 },
-                                "tokenIn": usdc,
-                                "tokenOut": usdt,
+                                "tokenIn": mid_token,
+                                "tokenOut": token_out,
                                 "splitBps": 0,
                             }
                         ],
@@ -301,7 +316,7 @@ def main() -> int:
         return 1
 
     try:
-        interactions_len = validate_encode_response(encode_response, tycho_router, dai)
+        interactions_len = validate_encode_response(encode_response, tycho_router, token_in)
     except AssertionError as exc:
         print(f"[FAIL] encode response validation failed: {exc}")
         return 1
@@ -309,7 +324,10 @@ def main() -> int:
     if args.verbose:
         print(json.dumps(encode_response, indent=2))
 
-    print(f"[OK] encode route {elapsed:.3f}s interactions={interactions_len}")
+    print(
+        f"[OK] encode route {elapsed:.3f}s chain={chain_label(chain_id)}:{chain_id} "
+        f"path={token_in_symbol}->{mid_symbol}->{token_out_symbol} interactions={interactions_len}"
+    )
     return 0
 
 
````

### Phase 5: Skill mirrors, docs, and infra wiring

## skills/simulation-service-tests/scripts/presets.py
Change summary: Synchronizes skill-owned script/docs mirrors with repository runtime and chain-aware workflow updates.
Risk level: Low
Review focus: Check parity between repo scripts and skill mirrors to avoid workflow drift.
Hunk context notes: These files are guidance/tooling mirrors; correctness matters for agent/operator UX more than runtime behavior.
````diff
diff --git a/skills/simulation-service-tests/scripts/presets.py b/skills/simulation-service-tests/scripts/presets.py
index ea930c4..53ae8f3 100644
--- a/skills/simulation-service-tests/scripts/presets.py
+++ b/skills/simulation-service-tests/scripts/presets.py
@@ -1,20 +1,41 @@
-"""Curated tokens + pair suites for simulation-service-tests.
-
-This lives in the skill (not the repo) so we can evolve test coverage without
-touching production code. Keep the list biased toward high-liquidity mainnet
-assets that are likely to exist across multiple DEXes/protocols.
-"""
+"""Curated tokens + pair suites for simulation testing."""
 
 from __future__ import annotations
 
+import os
 from dataclasses import dataclass
 from typing import Tuple
 
+SUPPORTED_CHAIN_IDS = (1, 8453)
+
+
+def parse_chain_id(raw_value: str) -> int:
+    try:
+        chain_id = int(raw_value)
+    except ValueError as exc:
+        raise ValueError(f"Invalid chain id: {raw_value!r}. Expected one of {SUPPORTED_CHAIN_IDS}.") from exc
+    if chain_id not in SUPPORTED_CHAIN_IDS:
+        raise ValueError(f"Unsupported chain id: {chain_id}. Supported values: {SUPPORTED_CHAIN_IDS}.")
+    return chain_id
+
+
+def resolve_chain_id(chain_id_arg: str | None) -> int:
+    raw = chain_id_arg
+    if raw is None:
+        raw = os.environ.get("CHAIN_ID")
+    if raw is None or not raw.strip():
+        raise ValueError(
+            "Missing chain id. Pass --chain-id or set CHAIN_ID in the environment/.env."
+        )
+    return parse_chain_id(raw.strip())
+
+
+def chain_label(chain_id: int) -> str:
+    return {1: "ethereum", 8453: "base"}.get(chain_id, f"chain-{chain_id}")
+
 
 # Ethereum mainnet token addresses (lowercased).
-TOKENS: dict[str, str] = {
-    # Native ETH placeholder used by some protocols (e.g., Uniswap v4 SDK).
-    # Whether Tycho exposes this as a token depends on the upstream token list.
+ETHEREUM_TOKENS: dict[str, str] = {
     "ETH": "0x0000000000000000000000000000000000000000",
     "WETH": "0xc02aaa39b223fe8d0a0e5c4f27ead9083c756cc2",
     "WBTC": "0x2260fac5e5542a773aa44fbcfedf7c193bc2c599",
@@ -48,58 +69,99 @@ TOKENS: dict[str, str] = {
     "FXS": "0x3432b6a60d23ca0dfca7761b7ab56459d9c964d0",
 }
 
-TOKEN_DECIMALS: dict[str, int] = {
-    "USDC": 6,
-    "USDT": 6,
-    "WBTC": 8,
+# Base token addresses (lowercased).
+BASE_TOKENS: dict[str, str] = {
+    "ETH": "0x0000000000000000000000000000000000000000",
+    "WETH": "0x4200000000000000000000000000000000000006",
+    "USDC": "0x833589fcd6edb6e08f4c7c32d4f71b54bda02913",
+    "DAI": "0x50c5725949a6f0c72e6c4a641f24049a917db0cb",
 }
 
-ADDRESS_DECIMALS: dict[str, int] = {
-    TOKENS[symbol].lower(): decimals for symbol, decimals in TOKEN_DECIMALS.items()
+TOKENS_BY_CHAIN: dict[int, dict[str, str]] = {
+    1: ETHEREUM_TOKENS,
+    8453: BASE_TOKENS,
 }
 
-TOKEN_BASE_UNITS: dict[str, list[int]] = {
-    # 0.001 .. 5 WBTC (8 decimals) to avoid pathological ladders at huge sizes.
-    "WBTC": [
-        100_000,
-        500_000,
-        1_000_000,
-        5_000_000,
-        10_000_000,
-        25_000_000,
-        50_000_000,
-        100_000_000,
-        200_000_000,
-    ],
-    # 0.1 .. 500 WETH (18 decimals) to avoid VM pool reverts at very large sizes.
-    "WETH": [
-        100_000_000_000_000_000,
-        500_000_000_000_000_000,
-        1_000_000_000_000_000_000,
-        2_000_000_000_000_000_000,
-        5_000_000_000_000_000_000,
-        10_000_000_000_000_000_000,
-        20_000_000_000_000_000_000,
-        50_000_000_000_000_000_000,
-        100_000_000_000_000_000_000,
-        500_000_000_000_000_000_000,
-    ],
-    "ETH": [
-        100_000_000_000_000_000,
-        500_000_000_000_000_000,
-        1_000_000_000_000_000_000,
-        2_000_000_000_000_000_000,
-        5_000_000_000_000_000_000,
-        10_000_000_000_000_000_000,
-        20_000_000_000_000_000_000,
-        50_000_000_000_000_000_000,
-        100_000_000_000_000_000_000,
-        500_000_000_000_000_000_000,
-    ],
+# Backward-compatible alias used by older callers.
+TOKENS: dict[str, str] = TOKENS_BY_CHAIN[1]
+
+TOKEN_DECIMALS_BY_CHAIN: dict[int, dict[str, int]] = {
+    1: {
+        "USDC": 6,
+        "USDT": 6,
+        "WBTC": 8,
+    },
+    8453: {
+        "USDC": 6,
+    },
 }
 
-ADDRESS_BASE_UNITS: dict[str, list[int]] = {
-    TOKENS[symbol].lower(): units for symbol, units in TOKEN_BASE_UNITS.items()
+TOKEN_BASE_UNITS_BY_CHAIN: dict[int, dict[str, list[int]]] = {
+    1: {
+        # 0.001 .. 2 WBTC (8 decimals) to avoid pathological ladders at huge sizes.
+        "WBTC": [
+            100_000,
+            500_000,
+            1_000_000,
+            5_000_000,
+            10_000_000,
+            25_000_000,
+            50_000_000,
+            100_000_000,
+            200_000_000,
+        ],
+        # 0.1 .. 500 WETH (18 decimals) to avoid VM pool reverts at very large sizes.
+        "WETH": [
+            100_000_000_000_000_000,
+            500_000_000_000_000_000,
+            1_000_000_000_000_000_000,
+            2_000_000_000_000_000_000,
+            5_000_000_000_000_000_000,
+            10_000_000_000_000_000_000,
+            20_000_000_000_000_000_000,
+            50_000_000_000_000_000_000,
+            100_000_000_000_000_000_000,
+            500_000_000_000_000_000_000,
+        ],
+        "ETH": [
+            100_000_000_000_000_000,
+            500_000_000_000_000_000,
+            1_000_000_000_000_000_000,
+            2_000_000_000_000_000_000,
+            5_000_000_000_000_000_000,
+            10_000_000_000_000_000_000,
+            20_000_000_000_000_000_000,
+            50_000_000_000_000_000_000,
+            100_000_000_000_000_000_000,
+            500_000_000_000_000_000_000,
+        ],
+    },
+    8453: {
+        "WETH": [
+            10_000_000_000_000_000,
+            50_000_000_000_000_000,
+            100_000_000_000_000_000,
+            500_000_000_000_000_000,
+            1_000_000_000_000_000_000,
+            2_000_000_000_000_000_000,
+            5_000_000_000_000_000_000,
+            10_000_000_000_000_000_000,
+            20_000_000_000_000_000_000,
+            50_000_000_000_000_000_000,
+        ],
+        "ETH": [
+            10_000_000_000_000_000,
+            50_000_000_000_000_000,
+            100_000_000_000_000_000,
+            500_000_000_000_000_000,
+            1_000_000_000_000_000_000,
+            2_000_000_000_000_000_000,
+            5_000_000_000_000_000_000,
+            10_000_000_000_000_000_000,
+            20_000_000_000_000_000_000,
+            50_000_000_000_000_000_000,
+        ],
+    },
 }
 
 BASE_AMOUNTS: list[int] = [
@@ -115,73 +177,146 @@ BASE_AMOUNTS: list[int] = [
     50_000,
 ]
 
-
 Pair = Tuple[str, str]
 
+PAIR_SUITES_BY_CHAIN: dict[int, dict[str, list[Pair]]] = {
+    1: {
+        "smoke": [
+            ("DAI", "USDC"),
+            ("WETH", "USDC"),
+            ("USDC", "USDT"),
+            ("WBTC", "WETH"),
+            ("STETH", "WETH"),
+        ],
+        "core": [
+            ("DAI", "USDC"),
+            ("USDC", "USDT"),
+            ("GHO", "USDC"),
+            ("WETH", "USDC"),
+            ("WETH", "USDT"),
+            ("WETH", "DAI"),
+            ("WBTC", "USDC"),
+            ("WBTC", "WETH"),
+            ("FRAX", "USDC"),
+            ("STETH", "WETH"),
+            ("WSTETH", "WETH"),
+            ("RETH", "WETH"),
+            ("ETH", "RETH"),
+            ("RETH", "ETH"),
+            ("CBETH", "WETH"),
+            ("UNI", "WETH"),
+            ("LINK", "WETH"),
+            ("AAVE", "WETH"),
+            ("COMP", "WETH"),
+            ("MKR", "WETH"),
+            ("SUSHI", "WETH"),
+            ("LDO", "WETH"),
+        ],
+        "extended": [
+            ("DAI", "USDC"),
+            ("USDC", "USDT"),
+            ("GHO", "USDC"),
+            ("WETH", "USDC"),
+            ("WETH", "USDT"),
+            ("WETH", "DAI"),
+            ("WBTC", "USDC"),
+            ("WBTC", "WETH"),
+            ("FRAX", "USDC"),
+            ("STETH", "WETH"),
+            ("WSTETH", "WETH"),
+            ("RETH", "WETH"),
+            ("ETH", "RETH"),
+            ("RETH", "ETH"),
+            ("CBETH", "WETH"),
+            ("UNI", "WETH"),
+            ("LINK", "WETH"),
+            ("AAVE", "WETH"),
+            ("COMP", "WETH"),
+            ("MKR", "WETH"),
+            ("SUSHI", "WETH"),
+            ("LDO", "WETH"),
+            ("LUSD", "USDC"),
+            ("FRXETH", "WETH"),
+            ("CRV", "WETH"),
+            ("CVX", "WETH"),
+            ("BAL", "WETH"),
+            ("RPL", "WETH"),
+            ("SNX", "WETH"),
+            ("YFI", "WETH"),
+            ("FXS", "WETH"),
+        ],
+        "stables": [
+            ("DAI", "USDC"),
+            ("DAI", "USDT"),
+            ("USDC", "USDT"),
+            ("GHO", "USDC"),
+            ("FRAX", "USDC"),
+        ],
+        "lst": [
+            ("STETH", "WETH"),
+            ("WSTETH", "WETH"),
+            ("RETH", "WETH"),
+            ("CBETH", "WETH"),
+        ],
+        "governance": [
+            ("UNI", "WETH"),
+            ("LINK", "WETH"),
+            ("AAVE", "WETH"),
+            ("COMP", "WETH"),
+            ("MKR", "WETH"),
+            ("SUSHI", "WETH"),
+            ("LDO", "WETH"),
+        ],
+        "v4_candidates": [
+            ("WETH", "USDC"),
+            ("WBTC", "USDC"),
+            ("ETH", "USDC"),
+        ],
+    },
+    8453: {
+        "smoke": [
+            ("USDC", "WETH"),
+            ("WETH", "USDC"),
+            ("ETH", "USDC"),
+            ("USDC", "ETH"),
+        ],
+        "core": [
+            ("USDC", "WETH"),
+            ("WETH", "USDC"),
+            ("ETH", "USDC"),
+            ("USDC", "ETH"),
+            ("DAI", "USDC"),
+            ("USDC", "DAI"),
+        ],
+        "extended": [
+            ("USDC", "WETH"),
+            ("WETH", "USDC"),
+            ("ETH", "USDC"),
+            ("USDC", "ETH"),
+            ("DAI", "USDC"),
+            ("USDC", "DAI"),
+        ],
+        "stables": [
+            ("DAI", "USDC"),
+            ("USDC", "DAI"),
+        ],
+        "lst": [
+            ("ETH", "WETH"),
+            ("WETH", "ETH"),
+        ],
+        "governance": [
+            ("WETH", "USDC"),
+        ],
+        "v4_candidates": [
+            ("WETH", "USDC"),
+            ("ETH", "USDC"),
+        ],
+    },
+}
 
-PAIR_SUITES: dict[str, list[Pair]] = {
-    # Fast sanity checks; should work even with TVL thresholds at defaults.
-    "smoke": [
-        ("DAI", "USDC"),
-        ("WETH", "USDC"),
-        ("USDC", "USDT"),
-        ("WBTC", "WETH"),
-        ("STETH", "WETH"),
-    ],
-    # Broad coverage across common pool types; useful after upgrades.
-    "core": [
-        ("DAI", "USDC"),
-        ("USDC", "USDT"),
-        ("GHO", "USDC"),
-        ("WETH", "USDC"),
-        ("WETH", "USDT"),
-        ("WETH", "DAI"),
-        ("WBTC", "USDC"),
-        ("WBTC", "WETH"),
-        ("FRAX", "USDC"),
-        ("STETH", "WETH"),
-        ("WSTETH", "WETH"),
-        ("RETH", "WETH"),
-        ("ETH", "RETH"),
-        ("RETH", "ETH"),
-        ("CBETH", "WETH"),
-        ("UNI", "WETH"),
-        ("LINK", "WETH"),
-        ("AAVE", "WETH"),
-        ("COMP", "WETH"),
-        ("MKR", "WETH"),
-        ("SUSHI", "WETH"),
-        ("LDO", "WETH"),
-    ],
-    "stables": [
-        ("DAI", "USDC"),
-        ("DAI", "USDT"),
-        ("USDC", "USDT"),
-        ("GHO", "USDC"),
-        ("FRAX", "USDC"),
-    ],
-    "lst": [
-        ("STETH", "WETH"),
-        ("WSTETH", "WETH"),
-        ("RETH", "WETH"),
-        ("CBETH", "WETH"),
-    ],
-    "governance": [
-        ("UNI", "WETH"),
-        ("LINK", "WETH"),
-        ("AAVE", "WETH"),
-        ("COMP", "WETH"),
-        ("MKR", "WETH"),
-        ("SUSHI", "WETH"),
-        ("LDO", "WETH"),
-    ],
-    # Candidate pairs that are likely to hit Uniswap v4 pools on mainnet.
-    # Note: some v4 pools may use native ETH (0x000..0) instead of WETH.
-    "v4_candidates": [
-        ("WETH", "USDC"),
-        ("WBTC", "USDC"),
-        ("ETH", "USDC"),
-    ],
+DEFAULT_ENCODE_ROUTE_BY_CHAIN: dict[int, tuple[str, str, str]] = {
+    1: ("DAI", "USDC", "USDT"),
+    8453: ("USDC", "WETH", "USDC"),
 }
 
 
@@ -191,52 +326,90 @@ class ResolvedPair:
     token_out: str
 
 
-def resolve_token(token_or_symbol: str) -> str:
+def tokens_for_chain(chain_id: int) -> dict[str, str]:
+    return TOKENS_BY_CHAIN[chain_id]
+
+
+def default_encode_route(chain_id: int) -> tuple[str, str, str]:
+    route = DEFAULT_ENCODE_ROUTE_BY_CHAIN.get(chain_id)
+    if route is None:
+        raise ValueError(f"No default encode route configured for chain {chain_id}")
+    return route
+
+
+def resolve_token(token_or_symbol: str, chain_id: int) -> str:
     token_or_symbol = token_or_symbol.strip()
     if token_or_symbol.lower().startswith("0x"):
         return token_or_symbol.lower()
 
     symbol = token_or_symbol.upper()
-    address = TOKENS.get(symbol)
+    address = TOKENS_BY_CHAIN[chain_id].get(symbol)
     if not address:
-        raise ValueError(f"Unknown token symbol: {token_or_symbol}")
+        raise ValueError(
+            f"Unknown token symbol for chain {chain_id} ({chain_label(chain_id)}): {token_or_symbol}"
+        )
     return address
 
 
-def parse_amounts(amounts_csv: str | None) -> list[str]:
-    if not amounts_csv:
-        return default_amounts(18)
-    amounts = [amount.strip() for amount in amounts_csv.split(",") if amount.strip()]
-    if not amounts:
-        raise ValueError("No amounts provided")
-    return amounts
-
-
 def default_amounts(decimals: int) -> list[str]:
-    scale = 10 ** decimals
+    scale = 10**decimals
     return [str(amount * scale) for amount in BASE_AMOUNTS]
 
 
-def token_decimals(token_or_symbol: str) -> int:
+def token_decimals(token_or_symbol: str, chain_id: int) -> int:
     token_or_symbol = token_or_symbol.strip()
     if token_or_symbol.lower().startswith("0x"):
-        return ADDRESS_DECIMALS.get(token_or_symbol.lower(), 18)
-    symbol = token_or_symbol.upper()
-    return TOKEN_DECIMALS.get(symbol, 18)
+        symbol = None
+        token_address = token_or_symbol.lower()
+    else:
+        symbol = token_or_symbol.upper()
+        token_address = TOKENS_BY_CHAIN[chain_id].get(symbol, "").lower()
+
+    decimals_by_symbol = TOKEN_DECIMALS_BY_CHAIN.get(chain_id, {})
+    if symbol and symbol in decimals_by_symbol:
+        return decimals_by_symbol[symbol]
+
+    for known_symbol, known_address in TOKENS_BY_CHAIN[chain_id].items():
+        if known_address.lower() == token_address:
+            return decimals_by_symbol.get(known_symbol, 18)
 
+    return 18
 
-def default_amounts_for_token(token_or_symbol: str) -> list[str]:
+
+def default_amounts_for_token(token_or_symbol: str, chain_id: int) -> list[str]:
     token_or_symbol = token_or_symbol.strip()
     if token_or_symbol.lower().startswith("0x"):
-        base_units = ADDRESS_BASE_UNITS.get(token_or_symbol.lower())
+        address = token_or_symbol.lower()
+        symbol = None
     else:
-        base_units = TOKEN_BASE_UNITS.get(token_or_symbol.upper())
+        symbol = token_or_symbol.upper()
+        address = TOKENS_BY_CHAIN[chain_id].get(symbol, "").lower()
+
+    base_units = None
+    if symbol:
+        base_units = TOKEN_BASE_UNITS_BY_CHAIN.get(chain_id, {}).get(symbol)
+    if base_units is None and address:
+        for known_symbol, known_address in TOKENS_BY_CHAIN[chain_id].items():
+            if known_address.lower() == address:
+                base_units = TOKEN_BASE_UNITS_BY_CHAIN.get(chain_id, {}).get(known_symbol)
+                break
+
     if base_units is not None:
         return [str(value) for value in base_units]
-    return default_amounts(token_decimals(token_or_symbol))
+
+    return default_amounts(token_decimals(token_or_symbol, chain_id))
+
+
+def parse_amounts(amounts_csv: str | None) -> list[str]:
+    if not amounts_csv:
+        return default_amounts(18)
+    amounts = [amount.strip() for amount in amounts_csv.split(",") if amount.strip()]
+    if not amounts:
+        raise ValueError("No amounts provided")
+    return amounts
 
 
-def parse_pairs(pair_args: list[str] | None, pairs_csv: str | None) -> list[ResolvedPair]:
+def parse_pairs(pair_args: list[str] | None, pairs_csv: str | None, chain_id: int) -> list[ResolvedPair]:
     pairs: list[str] = []
     if pairs_csv:
         pairs.extend([entry.strip() for entry in pairs_csv.split(",") if entry.strip()])
@@ -248,21 +421,24 @@ def parse_pairs(pair_args: list[str] | None, pairs_csv: str | None) -> list[Reso
         if ":" not in entry:
             raise ValueError(f"Invalid pair format: {entry} (expected token_in:token_out)")
         left, right = entry.split(":", 1)
-        resolved.append(ResolvedPair(resolve_token(left), resolve_token(right)))
+        resolved.append(ResolvedPair(resolve_token(left, chain_id), resolve_token(right, chain_id)))
     return resolved
 
 
-def suite_pairs(name: str) -> list[ResolvedPair]:
-    suite = PAIR_SUITES.get(name)
+def suite_pairs(name: str, chain_id: int) -> list[ResolvedPair]:
+    suites = PAIR_SUITES_BY_CHAIN.get(chain_id, {})
+    suite = suites.get(name)
     if suite is None:
-        known = ", ".join(sorted(PAIR_SUITES.keys()))
-        raise ValueError(f"Unknown suite: {name}. Known suites: {known}")
-    return [ResolvedPair(resolve_token(a), resolve_token(b)) for (a, b) in suite]
+        known = ", ".join(sorted(suites.keys()))
+        raise ValueError(
+            f"Unknown suite for chain {chain_id} ({chain_label(chain_id)}): {name}. Known suites: {known}"
+        )
+    return [ResolvedPair(resolve_token(a, chain_id), resolve_token(b, chain_id)) for (a, b) in suite]
 
 
-def list_suites() -> list[str]:
-    return sorted(PAIR_SUITES.keys())
+def list_suites(chain_id: int) -> list[str]:
+    return sorted(PAIR_SUITES_BY_CHAIN.get(chain_id, {}).keys())
 
 
-def list_tokens() -> list[tuple[str, str]]:
-    return sorted(TOKENS.items())
+def list_tokens(chain_id: int) -> list[tuple[str, str]]:
+    return sorted(TOKENS_BY_CHAIN.get(chain_id, {}).items())
````

## skills/simulation-service-tests/scripts/run_suite.zsh
Change summary: Synchronizes skill-owned script/docs mirrors with repository runtime and chain-aware workflow updates.
Risk level: Low
Review focus: Check parity between repo scripts and skill mirrors to avoid workflow drift.
Hunk context notes: These files are guidance/tooling mirrors; correctness matters for agent/operator UX more than runtime behavior.
````diff
diff --git a/skills/simulation-service-tests/scripts/run_suite.zsh b/skills/simulation-service-tests/scripts/run_suite.zsh
index 113bae5..222c854 100755
--- a/skills/simulation-service-tests/scripts/run_suite.zsh
+++ b/skills/simulation-service-tests/scripts/run_suite.zsh
@@ -3,30 +3,76 @@ set -euo pipefail
 
 usage() {
   cat <<'USAGE'
-Usage: run_suite.zsh --repo <path> [--base-url <url>] [--suite <name>] [--disable-vm-pools] [--enable-vm-pools] [--stop]
+Usage: run_suite.zsh --repo <path> [--base-url <url>] [--chain-id <id>] [--suite <name>] [--disable-vm-pools] [--enable-vm-pools] [--wait-vm-ready] [--allow-no-liquidity] [--allow-partial] [--stop]
 
 Run a small end-to-end test suite:
 1) start server (if not already running)
 2) wait for /status ready
+2.5) (optional) wait for VM pool readiness
 3) smoke test /simulate
 4) coverage sweep (pool/protocol summary)
 5) latency percentiles (p50/p90/p99)
 
 Options:
-  --repo             Repo root containing Cargo.toml
-  --base-url         Base URL (default: http://localhost:3000)
-  --suite            Pair suite for coverage/latency (default: core)
-  --disable-vm-pools Start server with ENABLE_VM_POOLS=false
-  --enable-vm-pools  Start server with ENABLE_VM_POOLS=true (default)
-  --stop             Stop server when done (only if started by this script)
-  -h, --help         Show this help
+  --repo               Repo root containing Cargo.toml
+  --base-url           Base URL (default: http://localhost:3000)
+  --chain-id           Runtime chain id (1 or 8453). Overrides CHAIN_ID env/.env.
+  --suite              Pair suite for coverage/latency (default: core)
+  --disable-vm-pools   Start server with ENABLE_VM_POOLS=false
+  --enable-vm-pools    Start server with ENABLE_VM_POOLS=true (default)
+  --wait-vm-ready      Wait for vm_status=ready after /status is ready (when VM is effectively enabled)
+  --allow-no-liquidity Allow no_liquidity responses with only no_pools failures
+  --allow-partial      Allow partial_success responses (and their failures)
+  --stop               Stop server when done (only if started by this script)
+  -h, --help           Show this help
 USAGE
 }
 
+validate_chain_id() {
+  case "$1" in
+    1|8453) ;;
+    *)
+      echo "Error: unsupported chain id '$1'. Supported values: 1 (Ethereum), 8453 (Base)." >&2
+      return 1
+      ;;
+  esac
+}
+
+load_chain_id_from_env_file() {
+  local env_file="$1"
+  if [[ ! -f "$env_file" ]]; then
+    return 1
+  fi
+  local raw
+  raw="$(grep -E '^[[:space:]]*(export[[:space:]]+)?CHAIN_ID[[:space:]]*=' "$env_file" | tail -n 1 || true)"
+  if [[ -z "$raw" ]]; then
+    return 1
+  fi
+  local value="${raw#*=}"
+  value="${value%%#*}"
+  value="${value#"${value%%[![:space:]]*}"}"
+  value="${value%"${value##*[![:space:]]}"}"
+  if [[ "$value" == \"*\" ]]; then
+    value="${value#\"}"
+    value="${value%\"}"
+  elif [[ "$value" == \'*\' ]]; then
+    value="${value#\'}"
+    value="${value%\'}"
+  fi
+  if [[ -z "$value" ]]; then
+    return 1
+  fi
+  echo "$value"
+}
+
 repo=""
 base_url="http://localhost:3000"
+chain_id_arg=""
 suite="core"
 enable_vm_pools="true"
+wait_vm_ready="false"
+allow_no_liquidity="false"
+allow_partial="false"
 stop_after="false"
 
 while [[ $# -gt 0 ]]; do
@@ -39,6 +85,10 @@ while [[ $# -gt 0 ]]; do
       base_url="$2"
       shift 2
       ;;
+    --chain-id)
+      chain_id_arg="$2"
+      shift 2
+      ;;
     --suite)
       suite="$2"
       shift 2
@@ -51,6 +101,18 @@ while [[ $# -gt 0 ]]; do
       enable_vm_pools="true"
       shift 1
       ;;
+    --wait-vm-ready)
+      wait_vm_ready="true"
+      shift 1
+      ;;
+    --allow-no-liquidity)
+      allow_no_liquidity="true"
+      shift 1
+      ;;
+    --allow-partial)
+      allow_partial="true"
+      shift 1
+      ;;
     --stop)
       stop_after="true"
       shift 1
@@ -78,11 +140,23 @@ repo_script="$repo/scripts/run_suite.sh"
 
 if [[ -f "$repo_script" ]]; then
   args=(--repo "$repo" --base-url "$base_url" --suite "$suite")
+  if [[ -n "$chain_id_arg" ]]; then
+    args+=(--chain-id "$chain_id_arg")
+  fi
   if [[ "$enable_vm_pools" == "true" ]]; then
     args+=(--enable-vm-pools)
   elif [[ "$enable_vm_pools" == "false" ]]; then
     args+=(--disable-vm-pools)
   fi
+  if [[ "$wait_vm_ready" == "true" ]]; then
+    args+=(--wait-vm-ready)
+  fi
+  if [[ "$allow_no_liquidity" == "true" ]]; then
+    args+=(--allow-no-liquidity)
+  fi
+  if [[ "$allow_partial" == "true" ]]; then
+    args+=(--allow-partial)
+  fi
   if [[ "$stop_after" == "true" ]]; then
     args+=(--stop)
   fi
@@ -95,15 +169,50 @@ simulate_url="${base_url%/}/simulate"
 skill_dir="$(cd "$(dirname "$0")" && pwd)"
 started_by_me="false"
 
+resolved_chain_id="${chain_id_arg:-${CHAIN_ID:-}}"
+if [[ -z "$resolved_chain_id" ]]; then
+  resolved_chain_id="$(load_chain_id_from_env_file "$repo/.env" || true)"
+fi
+if [[ -z "$resolved_chain_id" ]]; then
+  echo "Error: missing chain id. Pass --chain-id or set CHAIN_ID in env/.env." >&2
+  exit 2
+fi
+validate_chain_id "$resolved_chain_id"
+chain_id="$resolved_chain_id"
+
 echo "Base URL: $base_url"
+echo "Chain ID: $chain_id"
 echo "Suite: $suite"
-echo "Enable VM pools: $enable_vm_pools"
+echo "Enable VM pools (requested): $enable_vm_pools"
+
+simulate_allow_status="ready"
+coverage_allow_status="ready"
+latency_allow_status="ready"
+typeset -a simulate_flags=()
+typeset -a coverage_flags=()
+typeset -a latency_flags=()
+
+if [[ "$allow_partial" == "true" ]]; then
+  simulate_allow_status="${simulate_allow_status},partial_success"
+  coverage_allow_status="${coverage_allow_status},partial_success"
+  latency_allow_status="${latency_allow_status},partial_success"
+  simulate_flags+=(--allow-failures)
+  coverage_flags+=(--allow-failures)
+  latency_flags+=(--allow-failures)
+fi
+
+if [[ "$allow_no_liquidity" == "true" ]]; then
+  coverage_allow_status="${coverage_allow_status},no_liquidity"
+  latency_allow_status="${latency_allow_status},no_liquidity"
+  coverage_flags+=(--allow-no-pools)
+  latency_flags+=(--allow-no-pools)
+fi
 
 if curl -s "$status_url" >/dev/null 2>&1; then
   echo "Server already responding at $status_url"
 else
   echo "Starting server..."
-  start_server_args=(--repo "$repo")
+  start_server_args=(--repo "$repo" --chain-id "$chain_id")
   if [[ "$enable_vm_pools" == "true" ]]; then
     start_server_args+=(--enable-vm-pools)
   elif [[ "$enable_vm_pools" == "false" ]]; then
@@ -114,23 +223,49 @@ else
 fi
 
 echo "Waiting for readiness..."
-"$skill_dir/wait_ready.zsh" --url "$status_url" --timeout 300 --interval 2
+wait_timeout=300
+if [[ "$enable_vm_pools" == "true" ]]; then
+  wait_timeout=600
+fi
+"$skill_dir/wait_ready.zsh" --url "$status_url" --timeout "$wait_timeout" --interval 2 --expect-chain-id "$chain_id"
+
+runtime_vm_enabled="$(STATUS_URL="$status_url" python3 - <<'PY'
+import json
+import os
+import urllib.request
+
+with urllib.request.urlopen(os.environ["STATUS_URL"], timeout=5) as response:
+    status = json.loads(response.read().decode())
+
+print("true" if bool(status.get("vm_enabled")) else "false")
+PY
+)"
+
+echo "Runtime VM enabled (effective): $runtime_vm_enabled"
+
+if [[ "$wait_vm_ready" == "true" ]]; then
+  if [[ "$runtime_vm_enabled" == "true" ]]; then
+    "$skill_dir/wait_ready.zsh" --url "$status_url" --timeout 600 --interval 2 --expect-chain-id "$chain_id" --require-vm-ready --require-vm-pools-min 1
+  else
+    echo "Skipping --wait-vm-ready because runtime VM is effectively disabled on this chain/config."
+  fi
+fi
 
 echo "Smoke testing /simulate..."
-python3 "$skill_dir/simulate_smoke.py" --url "$simulate_url" --suite smoke --allow-status ready
+python3 "$skill_dir/simulate_smoke.py" --url "$simulate_url" --chain-id "$chain_id" --suite smoke --allow-status "$simulate_allow_status" --require-data --validate-data "${simulate_flags[@]}"
 
 echo "Coverage sweep..."
 mkdir -p "$repo/logs"
-python3 "$skill_dir/coverage_sweep.py" --url "$simulate_url" --suite "$suite" --allow-status ready --out "$repo/logs/coverage_sweep.json"
+python3 "$skill_dir/coverage_sweep.py" --url "$simulate_url" --chain-id "$chain_id" --suite "$suite" --allow-status "$coverage_allow_status" "${coverage_flags[@]}" --out "$repo/logs/coverage_sweep.json"
 
 echo "Latency percentiles..."
 latency_requests="${LATENCY_REQUESTS:-200}"
-if [[ "$enable_vm_pools" == "true" ]]; then
+if [[ "$runtime_vm_enabled" == "true" ]]; then
   latency_concurrency="${LATENCY_CONCURRENCY_VM:-4}"
 else
   latency_concurrency="${LATENCY_CONCURRENCY:-8}"
 fi
-python3 "$skill_dir/latency_percentiles.py" --url "$simulate_url" --suite "$suite" --requests "$latency_requests" --concurrency "$latency_concurrency" --allow-status ready
+python3 "$skill_dir/latency_percentiles.py" --url "$simulate_url" --chain-id "$chain_id" --suite "$suite" --requests "$latency_requests" --concurrency "$latency_concurrency" --allow-status "$latency_allow_status" "${latency_flags[@]}"
 
 if [[ "$stop_after" == "true" ]] && [[ "$started_by_me" == "true" ]]; then
   echo "Stopping server..."
````

## skills/simulation-service-tests/scripts/start_server.zsh
Change summary: Synchronizes skill-owned script/docs mirrors with repository runtime and chain-aware workflow updates.
Risk level: Low
Review focus: Check parity between repo scripts and skill mirrors to avoid workflow drift.
Hunk context notes: These files are guidance/tooling mirrors; correctness matters for agent/operator UX more than runtime behavior.
````diff
diff --git a/skills/simulation-service-tests/scripts/start_server.zsh b/skills/simulation-service-tests/scripts/start_server.zsh
index 900af2b..d4409ec 100755
--- a/skills/simulation-service-tests/scripts/start_server.zsh
+++ b/skills/simulation-service-tests/scripts/start_server.zsh
@@ -3,21 +3,33 @@ set -euo pipefail
 
 usage() {
   cat <<'USAGE'
-Usage: start_server.zsh [--repo <path>] [--log-file <path>] [--env KEY=VALUE] [--enable-vm-pools]
+Usage: start_server.zsh [--repo <path>] [--log-file <path>] [--chain-id <id>] [--env KEY=VALUE] [--enable-vm-pools]
 
 Start the tycho-simulation-server from a repo checkout.
 
 Options:
-  --repo       Path to repo root (default: current directory)
-  --log-file   Log file path (default: <repo>/logs/tycho-sim-server.log)
-  --env        Export KEY=VALUE before starting (repeatable)
+  --repo             Path to repo root (default: current directory)
+  --log-file         Log file path (default: <repo>/logs/tycho-sim-server.log)
+  --chain-id         Runtime chain id (1 or 8453). Overrides CHAIN_ID from env/.env.
+  --env              Export KEY=VALUE before starting (repeatable)
   --enable-vm-pools  Shortcut for --env ENABLE_VM_POOLS=true
-  -h, --help   Show this help
+  -h, --help         Show this help
 USAGE
 }
 
+validate_chain_id() {
+  case "$1" in
+    1|8453) ;;
+    *)
+      echo "Error: unsupported chain id '$1'. Supported values: 1 (Ethereum), 8453 (Base)." >&2
+      return 1
+      ;;
+  esac
+}
+
 repo="."
 log_file=""
+chain_id_arg=""
 typeset -a env_overrides=()
 
 while [[ $# -gt 0 ]]; do
@@ -30,6 +42,10 @@ while [[ $# -gt 0 ]]; do
       log_file="$2"
       shift 2
       ;;
+    --chain-id)
+      chain_id_arg="$2"
+      shift 2
+      ;;
     --env)
       env_overrides+=("$2")
       shift 2
@@ -58,6 +74,9 @@ if [[ -f "$repo_script" ]]; then
   if [[ -n "$log_file" ]]; then
     args+=(--log-file "$log_file")
   fi
+  if [[ -n "$chain_id_arg" ]]; then
+    args+=(--chain-id "$chain_id_arg")
+  fi
   for pair in "${env_overrides[@]}"; do
     args+=(--env "$pair")
   done
@@ -80,7 +99,6 @@ if [[ -f "$pid_file" ]]; then
 fi
 
 if [[ -f "$repo/.env" ]]; then
-  # Load .env into the environment without printing secrets.
   set -a
   source "$repo/.env"
   set +a
@@ -97,9 +115,19 @@ if [[ -z "${RUST_LOG:-}" ]]; then
 fi
 
 for pair in "${env_overrides[@]}"; do
-  export "${pair}"
+  export "$pair"
 done
 
+if [[ -n "$chain_id_arg" ]]; then
+  export CHAIN_ID="$chain_id_arg"
+fi
+
+if [[ -z "${CHAIN_ID:-}" ]]; then
+  echo "Error: missing chain id. Pass --chain-id or set CHAIN_ID in env/.env." >&2
+  exit 2
+fi
+validate_chain_id "$CHAIN_ID"
+
 if [[ -z "$log_file" ]]; then
   mkdir -p "$repo/logs"
   log_file="$repo/logs/tycho-sim-server.log"
@@ -112,6 +140,7 @@ fi
 )
 
 echo "Started tycho-simulation-server."
+echo "Chain ID: $CHAIN_ID"
 echo "PID: $(cat "$pid_file")"
 echo "Log: $log_file"
 echo "Tip: tail -f $log_file"
````

## skills/simulation-service-tests/scripts/wait_ready.zsh
Change summary: Synchronizes skill-owned script/docs mirrors with repository runtime and chain-aware workflow updates.
Risk level: Low
Review focus: Check parity between repo scripts and skill mirrors to avoid workflow drift.
Hunk context notes: These files are guidance/tooling mirrors; correctness matters for agent/operator UX more than runtime behavior.
````diff
diff --git a/skills/simulation-service-tests/scripts/wait_ready.zsh b/skills/simulation-service-tests/scripts/wait_ready.zsh
index cc2d820..a5c5d1f 100755
--- a/skills/simulation-service-tests/scripts/wait_ready.zsh
+++ b/skills/simulation-service-tests/scripts/wait_ready.zsh
@@ -3,21 +3,27 @@ set -euo pipefail
 
 usage() {
   cat <<'USAGE'
-Usage: wait_ready.zsh [--url <status_url>] [--timeout <seconds>] [--interval <seconds>]
+Usage: wait_ready.zsh [--url <status_url>] [--timeout <seconds>] [--interval <seconds>] [--expect-chain-id <id>] [--require-vm-ready] [--require-vm-pools-min <count>]
 
 Poll the /status endpoint until it returns ready or times out.
 
 Options:
-  --url        Status URL (default: http://localhost:3000/status)
-  --timeout    Timeout in seconds (default: 180)
-  --interval   Poll interval in seconds (default: 2)
-  -h, --help   Show this help
+  --url                  Status URL (default: http://localhost:3000/status)
+  --timeout              Timeout in seconds (default: 180)
+  --interval             Poll interval in seconds (default: 2)
+  --expect-chain-id      Require /status.chain_id to match this chain id
+  --require-vm-ready     Require vm_status=ready before succeeding
+  --require-vm-pools-min Minimum vm_pools required when --require-vm-ready is set (default: 1)
+  -h, --help             Show this help
 USAGE
 }
 
 url="http://localhost:3000/status"
 timeout=180
 interval=2
+expect_chain_id=""
+require_vm_ready="false"
+require_vm_pools_min=1
 
 while [[ $# -gt 0 ]]; do
   case "$1" in
@@ -33,6 +39,18 @@ while [[ $# -gt 0 ]]; do
       interval="$2"
       shift 2
       ;;
+    --expect-chain-id)
+      expect_chain_id="$2"
+      shift 2
+      ;;
+    --require-vm-ready)
+      require_vm_ready="true"
+      shift 1
+      ;;
+    --require-vm-pools-min)
+      require_vm_pools_min="$2"
+      shift 2
+      ;;
     -h|--help)
       usage
       exit 0
@@ -52,9 +70,48 @@ while true; do
   body="${response%$'\n'*}"
   status_code="${response##*$'\n'}"
 
-  if [[ "$status_code" == "200" ]] && [[ "$body" == *"\"ready\""* ]]; then
-    echo "ready"
-    exit 0
+  if [[ "$status_code" == "200" ]]; then
+    if check_output="$(python3 -c '
+import json
+import sys
+
+require_vm = sys.argv[1] == "true"
+vm_pools_min = int(sys.argv[2])
+expected_chain_raw = sys.argv[3].strip()
+expected_chain = int(expected_chain_raw) if expected_chain_raw else None
+
+try:
+    payload = json.load(sys.stdin)
+except Exception:
+    raise SystemExit(2)
+
+if payload.get("status") != "ready":
+    raise SystemExit(2)
+
+if expected_chain is not None:
+    actual_chain = payload.get("chain_id")
+    if actual_chain != expected_chain:
+        print(f"expected chain_id={expected_chain}, got {actual_chain}")
+        raise SystemExit(42)
+
+if require_vm:
+    if not payload.get("vm_enabled"):
+        raise SystemExit(2)
+    if payload.get("vm_status") != "ready":
+        raise SystemExit(2)
+    if int(payload.get("vm_pools") or 0) < vm_pools_min:
+        raise SystemExit(2)
+' "$require_vm_ready" "$require_vm_pools_min" "$expect_chain_id" <<<"$body" 2>&1)"
+    then
+      echo "ready"
+      exit 0
+    else
+      check_code=$?
+      if [[ "$check_code" -eq 42 ]]; then
+        echo "Chain mismatch while waiting for readiness: $check_output" >&2
+        exit 1
+      fi
+    fi
   fi
 
   now="$(date +%s)"
````

## skills/simulation-service-tests/scripts/simulate_smoke.py
Change summary: Synchronizes skill-owned script/docs mirrors with repository runtime and chain-aware workflow updates.
Risk level: Low
Review focus: Check parity between repo scripts and skill mirrors to avoid workflow drift.
Hunk context notes: These files are guidance/tooling mirrors; correctness matters for agent/operator UX more than runtime behavior.
````diff
diff --git a/skills/simulation-service-tests/scripts/simulate_smoke.py b/skills/simulation-service-tests/scripts/simulate_smoke.py
index 30ef74f..e2d273e 100755
--- a/skills/simulation-service-tests/scripts/simulate_smoke.py
+++ b/skills/simulation-service-tests/scripts/simulate_smoke.py
@@ -1,8 +1,5 @@
 #!/usr/bin/env python3
-"""Smoke test helper for POST /simulate.
-
-Runs a small suite of token pairs and validates quote meta status + basic shape.
-"""
+"""Smoke test helper for POST /simulate."""
 
 from __future__ import annotations
 
@@ -15,17 +12,17 @@ from urllib import request
 from urllib.error import HTTPError, URLError
 
 from presets import (
-    TOKENS,
+    chain_label,
     default_amounts_for_token,
     list_suites,
     list_tokens,
     parse_amounts,
     parse_pairs,
+    resolve_chain_id,
     suite_pairs,
+    tokens_for_chain,
 )
 
-ADDRESS_TO_SYMBOL = {address: symbol for (symbol, address) in TOKENS.items()}
-
 
 def request_simulate(url, payload, timeout):
     data = json.dumps(payload).encode("utf-8")
@@ -37,9 +34,9 @@ def request_simulate(url, payload, timeout):
         return response.status, body, elapsed
 
 
-def fmt_token(address: str) -> str:
+def fmt_token(address: str, address_to_symbol: dict[str, str]) -> str:
     address = address.lower()
-    symbol = ADDRESS_TO_SYMBOL.get(address)
+    symbol = address_to_symbol.get(address)
     if symbol:
         return symbol
     return address
@@ -67,11 +64,14 @@ def validate_pool_entry(entry, expected_len: int) -> tuple[bool, str]:
     if not all(isinstance(v, int) and v >= 0 for v in gas_used):
         return False, "gas_used contains non-integers"
 
+    gas_in_sell = entry.get("gas_in_sell")
+    if not is_int_string(gas_in_sell):
+        return False, 'gas_in_sell must be an integer string ("0" is valid)'
+
     block_number = entry.get("block_number")
     if not isinstance(block_number, int) or block_number < 0:
         return False, "block_number is invalid"
 
-    # Basic monotonicity: larger input amounts should not produce smaller outputs.
     prev = -1
     for raw in amounts_out:
         current = int(raw)
@@ -85,6 +85,7 @@ def validate_pool_entry(entry, expected_len: int) -> tuple[bool, str]:
 def main() -> int:
     parser = argparse.ArgumentParser(description="Smoke test POST /simulate")
     parser.add_argument("--url", default="http://localhost:3000/simulate")
+    parser.add_argument("--chain-id", help="Runtime chain id (1 or 8453); overrides CHAIN_ID env")
     parser.add_argument("--suite", default="smoke", help="Named pair suite from presets")
     parser.add_argument("--pair", action="append", help="token_in:token_out (symbol or address)")
     parser.add_argument("--pairs", help="Comma-separated token_in:token_out pairs")
@@ -95,7 +96,10 @@ def main() -> int:
     parser.add_argument(
         "--validate-data",
         action="store_true",
-        help="Validate response pool entries (amounts_out/gas_used lengths, ints, monotonicity)",
+        help=(
+            "Validate response pool entries (amounts_out/gas_used lengths, gas_in_sell integer-string, "
+            "block_number, monotonicity)"
+        ),
     )
     parser.add_argument("--list-suites", action="store_true", help="List available suites and exit")
     parser.add_argument("--list-tokens", action="store_true", help="List available token symbols and exit")
@@ -106,25 +110,35 @@ def main() -> int:
 
     args = parser.parse_args()
 
+    try:
+        chain_id = resolve_chain_id(args.chain_id)
+    except ValueError as exc:
+        print(f"Error: {exc}", file=sys.stderr)
+        return 2
+
+    address_to_symbol = {
+        address.lower(): symbol for (symbol, address) in tokens_for_chain(chain_id).items()
+    }
+
     if args.list_suites:
-        for name in list_suites():
+        for name in list_suites(chain_id):
             print(name)
         return 0
 
     if args.list_tokens:
-        for symbol, address in list_tokens():
+        for symbol, address in list_tokens(chain_id):
             print(f"{symbol} {address}")
         return 0
 
     try:
-        pairs = parse_pairs(args.pair, args.pairs)
+        pairs = parse_pairs(args.pair, args.pairs, chain_id)
     except ValueError as exc:
         print(f"Error: {exc}", file=sys.stderr)
         return 2
 
     if not pairs:
         try:
-            pairs = suite_pairs(args.suite)
+            pairs = suite_pairs(args.suite, chain_id)
         except ValueError as exc:
             print(f"Error: {exc}", file=sys.stderr)
             return 2
@@ -145,7 +159,7 @@ def main() -> int:
     for idx, pair in enumerate(pairs, start=1):
         token_in = pair.token_in
         token_out = pair.token_out
-        amounts = amounts_override or default_amounts_for_token(token_in)
+        amounts = amounts_override or default_amounts_for_token(token_in, chain_id)
         payload = {
             "request_id": f"{args.request_id_prefix}-{idx}-{uuid.uuid4().hex[:8]}",
             "token_in": token_in,
@@ -172,7 +186,7 @@ def main() -> int:
             failures += 1
             continue
 
-        pair_label = f"{fmt_token(token_in)}->{fmt_token(token_out)}"
+        pair_label = f"{fmt_token(token_in, address_to_symbol)}->{fmt_token(token_out, address_to_symbol)}"
 
         if status_code != 200:
             print(f"[FAIL] {pair_label} HTTP {status_code}")
@@ -231,7 +245,7 @@ def main() -> int:
 
     total = len(pairs)
     success = total - failures
-    print(f"\nSummary: {success}/{total} successful")
+    print(f"\nSummary ({chain_label(chain_id)}:{chain_id}): {success}/{total} successful")
     return 0 if failures == 0 else 1
 
 
````

## skills/simulation-service-tests/scripts/coverage_sweep.py
Change summary: Synchronizes skill-owned script/docs mirrors with repository runtime and chain-aware workflow updates.
Risk level: Low
Review focus: Check parity between repo scripts and skill mirrors to avoid workflow drift.
Hunk context notes: These files are guidance/tooling mirrors; correctness matters for agent/operator UX more than runtime behavior.
````diff
diff --git a/skills/simulation-service-tests/scripts/coverage_sweep.py b/skills/simulation-service-tests/scripts/coverage_sweep.py
index 25221d3..e209f84 100755
--- a/skills/simulation-service-tests/scripts/coverage_sweep.py
+++ b/skills/simulation-service-tests/scripts/coverage_sweep.py
@@ -1,9 +1,5 @@
 #!/usr/bin/env python3
-"""Coverage sweep for POST /simulate.
-
-Goal: exercise many pools/protocols by running a curated token-pair suite and
-summarizing which pools/protocols were observed in responses.
-"""
+"""Coverage sweep for POST /simulate."""
 
 from __future__ import annotations
 
@@ -18,7 +14,16 @@ from pathlib import Path
 from urllib import request
 from urllib.error import HTTPError, URLError
 
-from presets import default_amounts_for_token, list_suites, list_tokens, parse_amounts, parse_pairs, suite_pairs
+from presets import (
+    chain_label,
+    default_amounts_for_token,
+    list_suites,
+    list_tokens,
+    parse_amounts,
+    parse_pairs,
+    resolve_chain_id,
+    suite_pairs,
+)
 
 
 def request_simulate(url: str, payload: dict, timeout: float) -> tuple[int, bytes, float]:
@@ -35,19 +40,30 @@ def protocol_from_pool_name(pool_name: str | None) -> str:
     if not pool_name:
         return "unknown"
     if "::" in pool_name:
-        return pool_name.split("::", 1)[0]
-    return pool_name
+        protocol = pool_name.split("::", 1)[0]
+    else:
+        protocol = pool_name
+    # VM pools can use prefixes like vm:maverick_v2; normalize for stable assertions.
+    if protocol.startswith("vm:"):
+        return protocol.split(":", 1)[1]
+    return protocol
 
 
 def main() -> int:
     parser = argparse.ArgumentParser(description="Coverage sweep for POST /simulate")
     parser.add_argument("--url", default="http://localhost:3000/simulate")
+    parser.add_argument("--chain-id", help="Runtime chain id (1 or 8453); overrides CHAIN_ID env")
     parser.add_argument("--suite", default="core", help="Named pair suite from presets")
     parser.add_argument("--pair", action="append", help="token_in:token_out (symbol or address)")
     parser.add_argument("--pairs", help="Comma-separated token_in:token_out pairs")
     parser.add_argument("--amounts", help="Comma-separated amounts in wei")
     parser.add_argument("--allow-status", default="ready", help="Comma-separated allowed meta.status values")
     parser.add_argument("--allow-failures", action="store_true", help="Allow meta.failures to be non-empty")
+    parser.add_argument(
+        "--allow-no-pools",
+        action="store_true",
+        help="Allow no_liquidity responses that only report no_pools failures",
+    )
     parser.add_argument("--expect-protocols", help="Comma-separated protocol names expected in pool_name")
     parser.add_argument("--out", help="Write JSON report to this path")
     parser.add_argument("--timeout", type=float, default=15.0)
@@ -58,20 +74,26 @@ def main() -> int:
 
     args = parser.parse_args()
 
+    try:
+        chain_id = resolve_chain_id(args.chain_id)
+    except ValueError as exc:
+        print(f"Error: {exc}", file=sys.stderr)
+        return 2
+
     if args.list_suites:
-        for name in list_suites():
+        for name in list_suites(chain_id):
             print(name)
         return 0
 
     if args.list_tokens:
-        for symbol, address in list_tokens():
+        for symbol, address in list_tokens(chain_id):
             print(f"{symbol} {address}")
         return 0
 
     try:
-        pairs = parse_pairs(args.pair, args.pairs)
+        pairs = parse_pairs(args.pair, args.pairs, chain_id)
         if not pairs:
-            pairs = suite_pairs(args.suite)
+            pairs = suite_pairs(args.suite, chain_id)
         amounts_override = parse_amounts(args.amounts) if args.amounts else None
     except ValueError as exc:
         print(f"Error: {exc}", file=sys.stderr)
@@ -87,6 +109,8 @@ def main() -> int:
         expected_protocols = {p.strip().lower() for p in args.expect_protocols.split(",") if p.strip()}
 
     report = {
+        "chain_id": chain_id,
+        "chain": chain_label(chain_id),
         "url": args.url,
         "suite": args.suite,
         "pairs": [asdict(p) for p in pairs],
@@ -103,9 +127,10 @@ def main() -> int:
     failure_kinds: Counter[str] = Counter()
 
     failures = 0
+    allowed_no_pools = 0
 
     for idx, pair in enumerate(pairs, start=1):
-        amounts = amounts_override or default_amounts_for_token(pair.token_in)
+        amounts = amounts_override or default_amounts_for_token(pair.token_in, chain_id)
         payload = {
             "request_id": f"{args.request_id_prefix}-{idx}-{uuid.uuid4().hex[:8]}",
             "token_in": pair.token_in,
@@ -172,11 +197,36 @@ def main() -> int:
             continue
 
         if failures_list and not args.allow_failures:
-            failures += 1
-            report["requests"].append(
-                {"pair": asdict(pair), "elapsed_s": elapsed, "meta": meta, "error": "meta_failures"}
-            )
-            continue
+            if args.allow_no_pools and status == "no_liquidity":
+                only_no_pools = all(
+                    isinstance(item, dict) and item.get("kind") == "no_pools"
+                    for item in failures_list
+                )
+                if only_no_pools:
+                    allowed_no_pools += 1
+                    failures_list = []
+                else:
+                    failures += 1
+                    report["requests"].append(
+                        {
+                            "pair": asdict(pair),
+                            "elapsed_s": elapsed,
+                            "meta": meta,
+                            "error": "meta_failures",
+                        }
+                    )
+                    continue
+            else:
+                failures += 1
+                report["requests"].append(
+                    {
+                        "pair": asdict(pair),
+                        "elapsed_s": elapsed,
+                        "meta": meta,
+                        "error": "meta_failures",
+                    }
+                )
+                continue
 
         data = response_json.get("data", [])
         pool_count = len(data) if isinstance(data, list) else 0
@@ -202,7 +252,13 @@ def main() -> int:
                     )
 
         report["requests"].append(
-            {"pair": asdict(pair), "elapsed_s": elapsed, "meta": meta, "pool_count": pool_count, "amounts": amounts}
+            {
+                "pair": asdict(pair),
+                "elapsed_s": elapsed,
+                "meta": meta,
+                "pool_count": pool_count,
+                "amounts": amounts,
+            }
         )
 
         if args.verbose:
@@ -219,19 +275,27 @@ def main() -> int:
         "failures": failures,
         "meta_statuses": dict(meta_statuses),
         "failure_kinds": dict(failure_kinds),
+        "allowed_no_pools": allowed_no_pools,
         "unique_pools": len(pools_seen),
         "observed_protocols": observed_protocols,
         "protocol_counts": dict(pool_protocols),
         "missing_expected_protocols": missing_expected,
     }
-    report["pools"] = sorted(pools_seen.values(), key=lambda item: (item.get("protocol", ""), item.get("pool_name", "")))
+    report["pools"] = sorted(
+        pools_seen.values(), key=lambda item: (item.get("protocol", ""), item.get("pool_name", ""))
+    )
 
-    print("Coverage sweep summary")
+    print(f"Coverage sweep summary ({chain_label(chain_id)}:{chain_id})")
     print("=====================")
     print(f"Pairs: {len(pairs)}")
     print(f"Failures: {failures}")
+    if allowed_no_pools:
+        print(f"Allowed no_pools: {allowed_no_pools}")
     print(f"Unique pools observed: {len(pools_seen)}")
     print(f"Protocols observed: {', '.join(observed_protocols) if observed_protocols else '(none)'}")
+    if meta_statuses:
+        status_summary = ", ".join(f"{status}={count}" for status, count in meta_statuses.items())
+        print(f"Meta statuses: {status_summary}")
 
     if pool_protocols:
         print("\nTop protocols by pool appearances (top 10):")
````

## skills/simulation-service-tests/scripts/latency_percentiles.py
Change summary: Synchronizes skill-owned script/docs mirrors with repository runtime and chain-aware workflow updates.
Risk level: Low
Review focus: Check parity between repo scripts and skill mirrors to avoid workflow drift.
Hunk context notes: These files are guidance/tooling mirrors; correctness matters for agent/operator UX more than runtime behavior.
````diff
diff --git a/skills/simulation-service-tests/scripts/latency_percentiles.py b/skills/simulation-service-tests/scripts/latency_percentiles.py
index 46dd49a..8a37950 100755
--- a/skills/simulation-service-tests/scripts/latency_percentiles.py
+++ b/skills/simulation-service-tests/scripts/latency_percentiles.py
@@ -11,19 +11,23 @@ import statistics
 import sys
 import time
 import uuid
+from collections import Counter
 from concurrent.futures import ThreadPoolExecutor, as_completed
+from threading import Lock
 from urllib import request
 from urllib.error import HTTPError, URLError
 
 from presets import (
-    TOKENS,
+    chain_label,
     default_amounts_for_token,
     list_suites,
     list_tokens,
     parse_amounts,
     parse_pairs,
+    resolve_chain_id,
     resolve_token,
     suite_pairs,
+    tokens_for_chain,
 )
 
 
@@ -51,18 +55,19 @@ def request_simulate(url, payload, timeout):
         return response.status, body, elapsed
 
 
-def parse_tokens(tokens_csv: str | None) -> list[str]:
+def parse_tokens(tokens_csv: str | None, chain_id: int) -> list[str]:
     if not tokens_csv:
-        return list(TOKENS.values())
+        return list(tokens_for_chain(chain_id).values())
     tokens = [token.strip() for token in tokens_csv.split(",") if token.strip()]
     if len(tokens) < 2:
         raise ValueError("Provide at least two tokens")
-    return [resolve_token(token) for token in tokens]
+    return [resolve_token(token, chain_id) for token in tokens]
 
 
 def main() -> int:
     parser = argparse.ArgumentParser(description="Latency percentiles for POST /simulate")
     parser.add_argument("--url", default="http://localhost:3000/simulate")
+    parser.add_argument("--chain-id", help="Runtime chain id (1 or 8453); overrides CHAIN_ID env")
     parser.add_argument("--requests", type=int, default=200)
     parser.add_argument("--concurrency", type=int, default=50)
     parser.add_argument("--suite", default="core", help="Named pair suite from presets (used unless --random)")
@@ -73,6 +78,11 @@ def main() -> int:
     parser.add_argument("--amounts", help="Comma-separated amounts in wei")
     parser.add_argument("--allow-status", default="ready", help="Comma-separated allowed meta.status values")
     parser.add_argument("--allow-failures", action="store_true", help="Allow meta.failures to be non-empty")
+    parser.add_argument(
+        "--allow-no-pools",
+        action="store_true",
+        help="Allow no_liquidity responses that only report no_pools failures",
+    )
     parser.add_argument("--list-suites", action="store_true", help="List available suites and exit")
     parser.add_argument("--list-tokens", action="store_true", help="List available token symbols and exit")
     parser.add_argument("--timeout", type=float, default=10.0)
@@ -82,13 +92,19 @@ def main() -> int:
 
     args = parser.parse_args()
 
+    try:
+        chain_id = resolve_chain_id(args.chain_id)
+    except ValueError as exc:
+        print(f"Error: {exc}", file=sys.stderr)
+        return 2
+
     if args.list_suites:
-        for name in list_suites():
+        for name in list_suites(chain_id):
             print(name)
         return 0
 
     if args.list_tokens:
-        for symbol, address in list_tokens():
+        for symbol, address in list_tokens(chain_id):
             print(f"{symbol} {address}")
         return 0
 
@@ -103,9 +119,9 @@ def main() -> int:
         random.seed(args.seed)
 
     try:
-        explicit_pairs = parse_pairs(args.pair, args.pairs)
+        explicit_pairs = parse_pairs(args.pair, args.pairs, chain_id)
         amounts_override = parse_amounts(args.amounts) if args.amounts else None
-        tokens = parse_tokens(args.tokens)
+        tokens = parse_tokens(args.tokens, chain_id)
     except ValueError as exc:
         print(f"Error: {exc}", file=sys.stderr)
         return 2
@@ -124,7 +140,7 @@ def main() -> int:
         pairs = explicit_pairs
     elif not args.random:
         try:
-            pairs = suite_pairs(args.suite)
+            pairs = suite_pairs(args.suite, chain_id)
         except ValueError as exc:
             print(f"Error: {exc}", file=sys.stderr)
             return 2
@@ -136,7 +152,7 @@ def main() -> int:
         return tuple(random.sample(tokens, 2))
 
     def amounts_for_token(token_in: str) -> list[str]:
-        return amounts_override or default_amounts_for_token(token_in)
+        return amounts_override or default_amounts_for_token(token_in, chain_id)
 
     if args.dry_run:
         sample_pair = pick_pair()
@@ -147,22 +163,28 @@ def main() -> int:
             "amounts": amounts_for_token(sample_pair[0]),
         }
         print("Dry run configuration:")
-        print(json.dumps(
-            {
-                "url": args.url,
-                "requests": args.requests,
-                "concurrency": args.concurrency,
-                "timeout": args.timeout,
-                "sample_payload": payload,
-            },
-            indent=2,
-        ))
+        print(
+            json.dumps(
+                {
+                    "chain_id": chain_id,
+                    "chain": chain_label(chain_id),
+                    "url": args.url,
+                    "requests": args.requests,
+                    "concurrency": args.concurrency,
+                    "timeout": args.timeout,
+                    "sample_payload": payload,
+                },
+                indent=2,
+            )
+        )
         return 0
 
     start_time = time.perf_counter()
     response_times = []
     failures = 0
     failure_reasons: dict[str, int] = {}
+    status_counts: Counter[str] = Counter()
+    status_lock = Lock()
 
     def worker(index):
         token_in, token_out = pick_pair()
@@ -185,11 +207,24 @@ def main() -> int:
             status = meta.get("status")
             failures_list = meta.get("failures", [])
 
+            with status_lock:
+                status_counts[str(status)] += 1
+
             if status not in allowed_statuses:
                 return None, f"meta_status:{status}"
 
             if failures_list and not args.allow_failures:
-                return None, "meta_failures"
+                if args.allow_no_pools and status == "no_liquidity":
+                    only_no_pools = all(
+                        isinstance(item, dict) and item.get("kind") == "no_pools"
+                        for item in failures_list
+                    )
+                    if only_no_pools:
+                        failures_list = []
+                    else:
+                        return None, "meta_failures"
+                else:
+                    return None, "meta_failures"
 
             return elapsed, None
         except HTTPError as exc:
@@ -213,7 +248,7 @@ def main() -> int:
     total_time = time.perf_counter() - start_time
 
     if not response_times:
-        print("No successful requests.")
+        print(f"No successful requests ({chain_label(chain_id)}:{chain_id}).")
         print(f"Failures: {failures}")
         return 1
 
@@ -223,7 +258,7 @@ def main() -> int:
     p90 = percentile(response_times, 0.90)
     p99 = percentile(response_times, 0.99)
 
-    print("Latency results (seconds)")
+    print(f"Latency results ({chain_label(chain_id)}:{chain_id})")
     print("========================")
     print(f"Requests: {args.requests}")
     print(f"Successes: {len(response_times)}")
@@ -244,6 +279,11 @@ def main() -> int:
         for reason, count in sorted(failure_reasons.items(), key=lambda kv: kv[1], reverse=True)[:8]:
             print(f"- {reason}: {count}")
 
+    if status_counts:
+        print("\nStatus counts:")
+        for status, count in status_counts.most_common():
+            print(f"- {status}: {count}")
+
     return 0 if failures == 0 else 1
 
 
````

## skills/simulation-service-tests/SKILL.md
Change summary: Synchronizes skill-owned script/docs mirrors with repository runtime and chain-aware workflow updates.
Risk level: Low
Review focus: Check parity between repo scripts and skill mirrors to avoid workflow drift.
Hunk context notes: These files are guidance/tooling mirrors; correctness matters for agent/operator UX more than runtime behavior.
````diff
diff --git a/skills/simulation-service-tests/SKILL.md b/skills/simulation-service-tests/SKILL.md
index 1aafe64..0c1f4d4 100644
--- a/skills/simulation-service-tests/SKILL.md
+++ b/skills/simulation-service-tests/SKILL.md
@@ -11,14 +11,15 @@ metadata:
 
 1. Confirm the repo root (expect `Cargo.toml` and `src/`).
 2. Ensure `.env` exists and contains `TYCHO_API_KEY` (avoid logging it). For manual health checks, use `Authorization: <TYCHO_API_KEY>` (no `Bearer` prefix).
-3. Start the server (keeps a PID file in the repo root):
+3. Pick a chain context for this run (`--chain-id 1` for Ethereum, `--chain-id 8453` for Base). Scripts fail fast if neither `--chain-id` nor `CHAIN_ID` is set.
+4. Start the server (keeps a PID file in the repo root):
    ```bash
    cd /path/to/tycho-simulation-server
-   scripts/start_server.sh --repo .
+   scripts/start_server.sh --repo . --chain-id 1
    ```
-4. Wait for readiness:
+5. Wait for readiness and verify chain:
    ```bash
-   scripts/wait_ready.sh --url http://localhost:3000/status
+   scripts/wait_ready.sh --url http://localhost:3000/status --expect-chain-id 1
    ```
    - If it stays `warming_up`, wait longer (3–5+ minutes on a cold start, longer with VM pools) or re-run with a higher `--timeout`.
 
@@ -27,89 +28,91 @@ metadata:
 Run start → wait_ready → smoke → coverage → latency:
 ```bash
 cd /path/to/tycho-simulation-server
-scripts/run_suite.sh --repo . --suite core --stop
+scripts/run_suite.sh --repo . --chain-id 1 --suite core --stop
 ```
 
-`run_suite.sh` smoke checks now require non-empty `data` and validate pool entry schema (`amounts_out`, `gas_used`, `gas_in_sell`, monotonicity, and `block_number`). `gas_in_sell` is a decimal-string sell-token amount computed from request-scoped pricing inputs and can legitimately be `"0"` when gas reporting or pricing inputs are unavailable.
+`run_suite.sh` smoke checks require non-empty `data` and validate pool entry schema (`amounts_out`, `gas_used`, `gas_in_sell`, monotonicity, and `block_number`). `gas_in_sell` is a decimal-string sell-token amount computed from request-scoped pricing inputs and can legitimately be `"0"` when gas reporting or pricing inputs are unavailable.
 
-VM pools (Curve/Balancer/Maverick feeds) are enabled by default. To exclude them:
+VM pools (Curve/Balancer/Maverick feeds) are enabled by default, but runtime VM checks are only enforced when `vm_enabled=true` in `/status`.
+
+Exclude VM feeds:
 ```bash
-scripts/run_suite.sh --repo . --suite core --disable-vm-pools --stop
+scripts/run_suite.sh --repo . --chain-id 1 --suite core --disable-vm-pools --stop
 ```
 
-If you need to tolerate partial failures or empty-liquidity responses while still running the suite:
+Tolerate partial failures/no-liquidity while still running the suite:
 ```bash
-scripts/run_suite.sh --repo . --suite core --allow-partial --allow-no-liquidity --stop
+scripts/run_suite.sh --repo . --chain-id 8453 --suite core --allow-partial --allow-no-liquidity --stop
 ```
 
 ## Smoke test /simulate
 
-- Use the curated presets (supports many mainnet tokens + suites).
+- Use curated chain-aware presets (token/suite sets differ by chain).
 - Remember: `/simulate` returns `200 OK` even on partial failure; use `meta.status` / `meta.failures`.
 
 Examples:
 ```bash
-python3 scripts/simulate_smoke.py --suite smoke
-python3 scripts/simulate_smoke.py --pair DAI:USDC --pair WETH:USDC
-python3 scripts/simulate_smoke.py --list-tokens
+python3 scripts/simulate_smoke.py --chain-id 1 --suite smoke
+python3 scripts/simulate_smoke.py --chain-id 8453 --pair USDC:WETH --pair WETH:USDC
+python3 scripts/simulate_smoke.py --chain-id 1 --list-tokens
 ```
 
 For stricter checks (fail on empty data and validate pool entries, including `gas_in_sell`; `"0"` is valid):
 ```bash
-python3 scripts/simulate_smoke.py --suite smoke --require-data --validate-data
+python3 scripts/simulate_smoke.py --chain-id 1 --suite smoke --require-data --validate-data
 ```
 
 Allow partial responses explicitly:
 ```bash
-python3 scripts/simulate_smoke.py --suite smoke --allow-status ready,partial_success --allow-failures
+python3 scripts/simulate_smoke.py --chain-id 8453 --suite smoke --allow-status ready,partial_success --allow-failures
 ```
 
 ## Smoke test /encode
 
 - `/encode` follows the latest schema (singleSwap-only execution).
 - The smoke test performs two `/simulate` calls to pick pools, then posts a 2-hop route.
+- Route tokens are chain-aware (`scripts/presets.py` `default_encode_route`).
 - Default addresses can be overridden via `.env` (`COW_SETTLEMENT_CONTRACT`, `TYCHO_ROUTER_ADDRESS`).
 
 Examples:
 ```bash
-python3 scripts/encode_smoke.py --encode-url http://localhost:3000/encode --simulate-url http://localhost:3000/simulate --repo .
-```
-```bash
-python3 scripts/encode_smoke.py --allow-status ready,partial_success --allow-failures --verbose
+python3 scripts/encode_smoke.py --chain-id 1 --encode-url http://localhost:3000/encode --simulate-url http://localhost:3000/simulate --repo .
+python3 scripts/encode_smoke.py --chain-id 8453 --allow-status ready,partial_success --allow-failures --verbose
 ```
 
 ## Pool/protocol coverage sweeps
 
 `coverage_sweep.py` runs a suite of pairs and reports which protocols/pools appear in responses:
 ```bash
-python3 scripts/coverage_sweep.py --suite core --out logs/coverage_sweep.json
-python3 scripts/coverage_sweep.py --suite v4_candidates
+python3 scripts/coverage_sweep.py --chain-id 1 --suite core --out logs/coverage_sweep.json
+python3 scripts/coverage_sweep.py --chain-id 8453 --suite v4_candidates
 ```
 
-VM pool feeds (Curve/Balancer/Maverick) are controlled by `ENABLE_VM_POOLS` (default: `true`). Use `ENABLE_VM_POOLS=false` (or `scripts/run_suite.sh --disable-vm-pools ...`) to turn them off. See `references/protocols.md`.
+VM pool feeds are controlled by `ENABLE_VM_POOLS` (default: `true`). Use `ENABLE_VM_POOLS=false` (or `scripts/run_suite.sh --chain-id 1 --disable-vm-pools ...`) to turn them off. See `references/protocols.md`.
 
-To assert specific protocol presence (derived from `pool_name` prefixes), use:
+Protocol assertions should be chain-aware. Example (Ethereum VM probe):
 ```bash
-python3 scripts/coverage_sweep.py --suite core --expect-protocols uniswap_v3,uniswap_v4,maverick_v2
+python3 scripts/coverage_sweep.py --chain-id 1 --pair USDC:USDT --pair USDT:USDC --pair ETH:RETH --expect-protocols maverick_v2
 ```
 
 Allow `no_liquidity` responses with only `no_pools` failures:
 ```bash
-python3 scripts/coverage_sweep.py --suite core --allow-no-pools
+python3 scripts/coverage_sweep.py --chain-id 8453 --suite core --allow-no-pools
 ```
 
 ## Latency percentiles (p50/p90/p99)
 
-`latency_percentiles.py` measures only “good” responses by default (`meta.status=ready`, no `meta.failures`):
+`latency_percentiles.py` measures only "good" responses by default (`meta.status=ready`, no `meta.failures`):
 ```bash
-python3 scripts/latency_percentiles.py --suite core --requests 300 --concurrency 50
+python3 scripts/latency_percentiles.py --chain-id 1 --suite core --requests 300 --concurrency 50
+python3 scripts/latency_percentiles.py --chain-id 8453 --suite core --requests 300 --concurrency 50
 ```
 
 ## Load / stress testing
 
 - Prefer the repo suite + percentile runner:
-  - `scripts/run_suite.sh --repo . --suite core` (smoke + coverage + p50/p90/p99)
-  - `python3 scripts/latency_percentiles.py --requests 2000 --concurrency 50 --suite core` (heavier load)
+  - `scripts/run_suite.sh --repo . --chain-id 1 --suite core`
+  - `python3 scripts/latency_percentiles.py --chain-id 1 --requests 2000 --concurrency 50 --suite core`
 - See `STRESS_TEST_README.md` for defaults and knobs (`LATENCY_REQUESTS`, `LATENCY_CONCURRENCY`, `LATENCY_CONCURRENCY_VM`).
 
 ## Maintenance checks
@@ -121,7 +124,7 @@ zsh skills/simulation-service-tests/scripts/run_checks.zsh --repo /path/to/tycho
 
 ## Memory tracking (jemalloc)
 
-1. Run the ignored harnesses and record the output lines in `.codex/memory-task/service-memory-tracking.md`:
+1. Run the ignored harnesses and record output lines in `.codex/memory-task/service-memory-tracking.md`:
    ```bash
    cargo test --test integration protocol_reset_memory::memory_spike_breakdown_harness -- --ignored --nocapture
    cargo test --test integration protocol_reset_memory::shared_db_rebuild_stress_harness -- --ignored --nocapture
@@ -135,7 +138,7 @@ zsh skills/simulation-service-tests/scripts/run_checks.zsh --repo /path/to/tycho
 
 1. Bump `tycho-simulation` tag/version (or other deps).
 2. Run: `cargo fmt`, `cargo clippy ...`, `cargo test`, `cargo build --release`.
-3. Run the end-to-end suite (`run_suite.zsh`) with and without VM pools.
+3. Run the end-to-end suite (`run_suite.zsh`) on both chains (typically one run with `--chain-id 1`, one with `--chain-id 8453`; with/without VM pools as needed).
 4. If infra changes are involved: `npm ci`, `npx cdk synth`, `npx cdk diff`.
 
 ## Deploy workflow (CDK + Docker)
@@ -144,14 +147,15 @@ See `references/deploy.md`.
 
 ## Included scripts
 
-- Repo (source of truth): `scripts/start_server.sh`, `scripts/stop_server.sh`, `scripts/wait_ready.sh`, `scripts/run_suite.sh`, plus the Python runners in `scripts/`.
+- Repo (source of truth): `scripts/start_server.sh`, `scripts/stop_server.sh`, `scripts/wait_ready.sh`, `scripts/run_suite.sh`, plus Python runners in `scripts/`.
+- Skill fallback scripts: chain-aware mirrors under `skills/simulation-service-tests/scripts/` (used when repo scripts are unavailable).
 - Skill utilities: `scripts/run_checks.zsh` (CI-like `cargo fmt/clippy/test/build` + optional `cdk synth`/`docker build`).
 
 ## References
 
 - `references/project.md` – repo commands, endpoints, and stress-test details.
 - `references/encode.md` – `/encode` schema and smoke-testing notes.
-- `references/protocols.md` – which exchanges/protocol feeds this server subscribes to (and how to test them).
+- `references/protocols.md` – exchange/protocol feeds per chain and test implications.
 - `references/tycho-deps.md` – Tycho/Propeller Heads context and docs.
 - `references/upgrade.md` – checklist for dependency upgrades.
 - `references/deploy.md` – CDK + Docker deployment notes.
````

## skills/simulation-service-tests/references/project.md
Change summary: Synchronizes skill-owned script/docs mirrors with repository runtime and chain-aware workflow updates.
Risk level: Low
Review focus: Check parity between repo scripts and skill mirrors to avoid workflow drift.
Hunk context notes: These files are guidance/tooling mirrors; correctness matters for agent/operator UX more than runtime behavior.
````diff
diff --git a/skills/simulation-service-tests/references/project.md b/skills/simulation-service-tests/references/project.md
index e9a2c97..841aed1 100644
--- a/skills/simulation-service-tests/references/project.md
+++ b/skills/simulation-service-tests/references/project.md
@@ -6,6 +6,7 @@
 
 ## Local run
 - Create `.env` from `.env.example` and set `TYCHO_API_KEY` (required).
+- Set `CHAIN_ID` (`1` for Ethereum, `8453` for Base) or pass `--chain-id` to scripts.
 - Tycho health checks expect `Authorization: <TYCHO_API_KEY>` (no `Bearer` prefix).
 - Start the server:
   ```bash
@@ -15,9 +16,10 @@
 
 ## Readiness
 - `GET /status` returns:
-  - `200 OK` with `{ "status": "ready", "block": <u64>, "pools": <usize> }`
+  - `200 OK` with `{ "status": "ready", "chain_id": <u64>, "block": <u64>, "pools": <usize>, ... }`
   - `503 Service Unavailable` with `{ "status": "warming_up", ... }`
 - Cold starts can take several minutes (3–5+ mins; longer with VM pools); wait before concluding readiness is stuck.
+- `scripts/wait_ready.sh --expect-chain-id <id>` should be used to guard against hitting the wrong deployment.
 
 ## Quote simulation
 - `POST /simulate` request body:
@@ -33,16 +35,16 @@
 
 ## Repo verification suite (recommended)
 - One-shot suite (start → wait_ready → smoke → coverage → latency):
-  - `scripts/run_suite.sh --repo . --suite core --stop`
-  - VM pools are enabled by default; add `--disable-vm-pools` to skip VM feeds (Curve/Balancer/Maverick).
+  - `scripts/run_suite.sh --repo . --chain-id 1 --suite core --stop`
+  - `scripts/run_suite.sh --repo . --chain-id 8453 --suite core --stop`
+  - VM pools are enabled by default; add `--disable-vm-pools` to skip VM feeds.
   - Use `--allow-partial` or `--allow-no-liquidity` if you expect partial/no-liquidity responses.
   - Smoke validation checks non-empty `data` and pool fields including `gas_in_sell` (decimal string, `"0"` is valid when reporting is disabled/unavailable; pricing inputs are request-scoped).
-  - Core coverage now includes `GHO:USDC`, `ETH:RETH`, and `RETH:ETH`.
 - Individual runners:
-  - `python3 scripts/simulate_smoke.py --suite smoke`
-  - `python3 scripts/encode_smoke.py --encode-url http://localhost:3000/encode --simulate-url http://localhost:3000/simulate --repo .`
-  - `python3 scripts/coverage_sweep.py --suite core --out logs/coverage_sweep.json`
-  - `python3 scripts/latency_percentiles.py --suite core --requests 300 --concurrency 50`
+  - `python3 scripts/simulate_smoke.py --chain-id 1 --suite smoke`
+  - `python3 scripts/encode_smoke.py --chain-id 1 --encode-url http://localhost:3000/encode --simulate-url http://localhost:3000/simulate --repo .`
+  - `python3 scripts/coverage_sweep.py --chain-id 1 --suite core --out logs/coverage_sweep.json`
+  - `python3 scripts/latency_percentiles.py --chain-id 1 --suite core --requests 300 --concurrency 50`
 - See `STRESS_TEST_README.md` for suites, defaults, and latency knobs.
 
 ## Useful commands
````

## skills/simulation-service-tests/references/protocols.md
Change summary: Synchronizes skill-owned script/docs mirrors with repository runtime and chain-aware workflow updates.
Risk level: Low
Review focus: Check parity between repo scripts and skill mirrors to avoid workflow drift.
Hunk context notes: These files are guidance/tooling mirrors; correctness matters for agent/operator UX more than runtime behavior.
````diff
diff --git a/skills/simulation-service-tests/references/protocols.md b/skills/simulation-service-tests/references/protocols.md
index 45671d4..f6a9f82 100644
--- a/skills/simulation-service-tests/references/protocols.md
+++ b/skills/simulation-service-tests/references/protocols.md
@@ -1,8 +1,10 @@
 # Protocol feeds (what this server subscribes to)
 
-The service subscribes to specific Tycho exchanges at startup (see `src/services/stream_builder.rs`).
+The service subscribes to chain-specific Tycho exchanges at startup (see `src/config/mod.rs` and `src/services/stream_builder.rs`).
 
-## Always enabled (native feeds)
+## Native feeds by chain
+
+### Ethereum (`CHAIN_ID=1`)
 - `uniswap_v2`
 - `sushiswap_v2`
 - `pancakeswap_v2`
@@ -10,27 +12,40 @@ The service subscribes to specific Tycho exchanges at startup (see `src/services
 - `pancakeswap_v3`
 - `uniswap_v4`
 - `ekubo_v2`
-- `ekubo_v3`
 - `fluid_v1`
-- `rocketpool`
+- `ekubo_v3`
+
+### Base (`CHAIN_ID=8453`)
+- `uniswap_v2`
+- `uniswap_v3`
+- `uniswap_v4`
+- `pancakeswap_v3`
+
+## VM feeds by chain
+
+### Ethereum (`CHAIN_ID=1`)
+- `vm:curve`
+- `vm:balancer_v2`
+- `vm:maverick_v2`
+
+### Base (`CHAIN_ID=8453`)
+- No VM protocols configured in this iteration.
+
+## Effective VM enablement
 
-## Optional (VM feeds)
-- `vm:curve` (enabled when `ENABLE_VM_POOLS=true`, default)
-- `vm:balancer_v2` (enabled when `ENABLE_VM_POOLS=true`, default)
-- `vm:maverick_v2` (enabled when `ENABLE_VM_POOLS=true`, default)
+- Runtime VM state is `effective_vm_enabled = ENABLE_VM_POOLS && vm_protocols_not_empty`.
+- This means Base reports `vm_enabled=false` even if `ENABLE_VM_POOLS=true`.
+- `scripts/run_suite.sh` and `wait_ready.sh` now key VM assertions off runtime `/status.vm_enabled`.
 
 ## Notes that affect test coverage
 
-- **VM pools are on by default**. Disable them if you want to validate native-only behavior:
-  - `scripts/run_suite.sh --repo . --suite core --disable-vm-pools --stop`
-  - Or start the server with `ENABLE_VM_POOLS=false`:
-    - `zsh skills/simulation-service-tests/scripts/start_server.zsh --repo /path/to/tycho-simulation-server --env ENABLE_VM_POOLS=false`
-- **Core suite coverage** includes `GHO:USDC`, `ETH:RETH`, and `RETH:ETH` to exercise Maverick/Rocketpool/native paths in normal runs.
-- **TVL filtering matters**: pools are included/removed based on `TVL_THRESHOLD` + `TVL_KEEP_RATIO`.
-  - If your tests miss protocols/pools, try lowering `TVL_THRESHOLD` (at the cost of ingesting more pools).
-- **Uniswap v4 pairs**: some v4 pools may use native ETH (often represented as `0x000...000`).
-  - The `v4_candidates` suite includes `ETH:USDC` alongside `WETH:USDC`.
-- **Protocol names in reports**: `coverage_sweep.py` derives “protocol” from `pool_name` (the prefix before `::`).
-  - Treat it as best-effort labeling; if upstream changes formatting, update `protocol_from_pool_name`.
-  - When using `--expect-protocols`, use the lowercased `pool_name` prefixes from the sweep output, not request protocol IDs like `uniswap_v3`.
-  - For hard CI/local gates, prefer stable expectations for your probe pairs (for example: `maverick_v2`).
+- Always pass chain context to scripts (`--chain-id` or env `CHAIN_ID`).
+- To test native-only behavior on Ethereum:
+  - `scripts/run_suite.sh --repo . --chain-id 1 --suite core --disable-vm-pools --stop`
+- Or start with VM disabled:
+  - `zsh skills/simulation-service-tests/scripts/start_server.zsh --repo /path/to/tycho-simulation-server --chain-id 1 --env ENABLE_VM_POOLS=false`
+- TVL filtering matters: pools are included/removed based on `TVL_THRESHOLD` + `TVL_KEEP_RATIO`.
+- Protocol labels in `coverage_sweep.py` are derived from `pool_name` prefixes (best effort).
+- Chain-aware protocol assertions are recommended:
+  - Ethereum VM gate example: `--expect-protocols maverick_v2`
+  - Base currently has no default VM protocol gate.
````

## skills/simulation-service-tests/references/encode.md
Change summary: Synchronizes skill-owned script/docs mirrors with repository runtime and chain-aware workflow updates.
Risk level: Low
Review focus: Check parity between repo scripts and skill mirrors to avoid workflow drift.
Hunk context notes: These files are guidance/tooling mirrors; correctness matters for agent/operator UX more than runtime behavior.
````diff
diff --git a/skills/simulation-service-tests/references/encode.md b/skills/simulation-service-tests/references/encode.md
index 403eb42..aa51465 100644
--- a/skills/simulation-service-tests/references/encode.md
+++ b/skills/simulation-service-tests/references/encode.md
@@ -9,13 +9,16 @@
 ## Smoke test helper
 
 `scripts/encode_smoke.py`:
+- Resolves chain from `--chain-id` (or env `CHAIN_ID`).
 - Calls `/simulate` for each hop to pick candidate pools.
 - Builds a 2-hop `MultiSwap` request and posts to `/encode`.
 - Verifies interactions shape, approvals, and router calldata.
+- Uses chain-specific route defaults from `scripts/presets.py` (`default_encode_route`).
 
-Example:
+Examples:
 ```bash
-python3 scripts/encode_smoke.py --encode-url http://localhost:3000/encode --simulate-url http://localhost:3000/simulate --repo .
+python3 scripts/encode_smoke.py --chain-id 1 --encode-url http://localhost:3000/encode --simulate-url http://localhost:3000/simulate --repo .
+python3 scripts/encode_smoke.py --chain-id 8453 --encode-url http://localhost:3000/encode --simulate-url http://localhost:3000/simulate --repo .
 ```
 
 ## Common pitfalls
@@ -25,6 +28,7 @@ python3 scripts/encode_smoke.py --encode-url http://localhost:3000/encode --simu
   - `TYCHO_ROUTER_ADDRESS`
 - `/encode` fails if the resimulated route `expectedAmountOut < minAmountOut`.
 - Timeout behavior differs from `/simulate`: `/encode` returns `408` with `{ error, requestId }`.
+- Chain mismatch between request `chainId` and server runtime chain fails validation.
 
 ## Reference docs
 
````

## skills/simulation-service-tests/references/upgrade.md
Change summary: Synchronizes skill-owned script/docs mirrors with repository runtime and chain-aware workflow updates.
Risk level: Low
Review focus: Check parity between repo scripts and skill mirrors to avoid workflow drift.
Hunk context notes: These files are guidance/tooling mirrors; correctness matters for agent/operator UX more than runtime behavior.
````diff
diff --git a/skills/simulation-service-tests/references/upgrade.md b/skills/simulation-service-tests/references/upgrade.md
index 885be92..341ebf0 100644
--- a/skills/simulation-service-tests/references/upgrade.md
+++ b/skills/simulation-service-tests/references/upgrade.md
@@ -12,22 +12,35 @@ Use this when bumping `tycho-simulation` (or changing stream logic, timeouts, or
 3. `cargo test`
 
 ## End-to-end API verification
+
+Run verification per target chain.
+
+### Ethereum (`CHAIN_ID=1`)
 1. Start and wait for readiness:
    - `cd /path/to/tycho-simulation-server`
-   - `scripts/start_server.sh --repo .`
-   - `scripts/wait_ready.sh --url http://localhost:3000/status`
+   - `scripts/start_server.sh --repo . --chain-id 1`
+   - `scripts/wait_ready.sh --url http://localhost:3000/status --expect-chain-id 1`
 2. Run smoke tests + pool/protocol sweep:
-   - `python3 scripts/simulate_smoke.py --suite smoke`
-   - `python3 scripts/coverage_sweep.py --suite core --out logs/coverage_sweep.json`
-3. Run latency percentiles (keep the config with results):
-   - `python3 scripts/latency_percentiles.py --suite core --requests 300 --concurrency 50`
+   - `python3 scripts/simulate_smoke.py --chain-id 1 --suite smoke`
+   - `python3 scripts/coverage_sweep.py --chain-id 1 --suite core --out logs/coverage_sweep.json`
+3. Run latency percentiles:
+   - `python3 scripts/latency_percentiles.py --chain-id 1 --suite core --requests 300 --concurrency 50`
 
-## VM pools (Curve/Balancer/Maverick)
-VM pools are enabled by default; keep a run with them disabled for comparison:
-- `scripts/run_suite.sh --repo . --suite core --disable-vm-pools --stop`
+### Base (`CHAIN_ID=8453`)
+1. Start and wait for readiness:
+   - `scripts/start_server.sh --repo . --chain-id 8453`
+   - `scripts/wait_ready.sh --url http://localhost:3000/status --expect-chain-id 8453`
+2. Run smoke tests + sweep:
+   - `python3 scripts/simulate_smoke.py --chain-id 8453 --suite smoke --allow-status ready,partial_success --allow-failures`
+   - `python3 scripts/coverage_sweep.py --chain-id 8453 --suite core --allow-status ready,partial_success --allow-failures --out logs/coverage_sweep.base.json`
+3. Run latency percentiles:
+   - `python3 scripts/latency_percentiles.py --chain-id 8453 --suite core --requests 300 --concurrency 50 --allow-status ready,partial_success --allow-failures`
 
-Core coverage should exercise representative native/VM pairs in normal runs (`GHO:USDC`, `ETH:RETH`, `RETH:ETH`).
+## VM pools (Curve/Balancer/Maverick)
+- VM checks are meaningful only when `/status.vm_enabled=true`.
+- For Ethereum comparison runs, include one VM-disabled pass:
+  - `scripts/run_suite.sh --repo . --chain-id 1 --suite core --disable-vm-pools --stop`
 
 ## Load testing / regressions
 - Prefer `scripts/run_suite.sh` and/or `scripts/latency_percentiles.py` with higher `--requests`/`--concurrency`.
-- Keep note of the machine profile, `LATENCY_REQUESTS`, and concurrency when comparing results.
+- Keep note of chain id, machine profile, `LATENCY_REQUESTS`, and concurrency when comparing results.
````

## README.md
Change summary: Documentation/configuration update related to chain-aware runtime rollout.
Risk level: Low
Review focus: Verify wording and examples reflect the new required `CHAIN_ID` behavior.
Hunk context notes: These changes reduce operator confusion during rollout and incident response.
````diff
diff --git a/README.md b/README.md
index 459d4e8..b75ff79 100644
--- a/README.md
+++ b/README.md
@@ -43,7 +43,8 @@ The following environment variables are read at startup:
 
 - `TYCHO_URL` – Tycho API base URL (default: `tycho-beta.propellerheads.xyz`)
 - `TYCHO_API_KEY` – API key for authenticated Tycho access (**required**)
-- `RPC_URL` – Optional Ethereum JSON-RPC endpoint used for background `eth_gasPrice` refresh
+- `CHAIN_ID` – Runtime chain ID (**required**); supported values: `1` (Ethereum), `8453` (Base)
+- `RPC_URL` – Optional JSON-RPC endpoint matching `CHAIN_ID`, used for background `eth_gasPrice` refresh
 - `GAS_PRICE_REFRESH_INTERVAL_MS` – Poll interval for `eth_gasPrice` refresh task (default: `5000`)
 - `GAS_PRICE_FAILURE_TOLERANCE` – Disable gas reporting when consecutive refresh failures exceed this value (default: `50`)
 - `TVL_THRESHOLD` – Minimum TVL (in native units) for adding a pool to the stream (default: `100`)
@@ -79,6 +80,19 @@ The following environment variables are read at startup:
 
 Note: when concurrency caps are saturated or a pool would exceed the quote deadline, pools are skipped instead of queued. `meta.status` remains an operational/reliability signal, while `meta.result_quality` and `meta.pool_results` explain quote completeness and per-pool anomalies.
 
+## Scripted Testing
+
+Repo test scripts are chain-aware and require chain context via `--chain-id` or `CHAIN_ID`.
+
+Examples:
+
+```bash
+python3 scripts/simulate_smoke.py --chain-id 1 --suite smoke
+python3 scripts/encode_smoke.py --chain-id 1 --repo .
+scripts/run_suite.sh --repo . --chain-id 1 --enable-vm-pools --stop
+scripts/run_suite.sh --repo . --chain-id 8453 --enable-vm-pools --stop
+```
+
 ## Docs
 
 - `docs/encode_example.md` – `/encode` schema walkthrough with shape-focused request/response examples.
````

## STRESS_TEST_README.md
Change summary: Documentation/configuration update related to chain-aware runtime rollout.
Risk level: Low
Review focus: Verify wording and examples reflect the new required `CHAIN_ID` behavior.
Hunk context notes: These changes reduce operator confusion during rollout and incident response.
````diff
diff --git a/STRESS_TEST_README.md b/STRESS_TEST_README.md
index d0ee393..decbecc 100644
--- a/STRESS_TEST_README.md
+++ b/STRESS_TEST_README.md
@@ -16,47 +16,47 @@ The suite (`scripts/run_suite.sh`) performs:
 1. **Python 3** (stdlib only)
 2. **Rust toolchain** for `cargo run --release`
 3. **Tycho API key** in `.env` (`TYCHO_API_KEY=...`)
+4. **Chain context** via `--chain-id` or `CHAIN_ID` (`1` Ethereum, `8453` Base)
 
 ## Quick Start
 
-Run the full suite (start → wait → smoke → coverage → latency):
+Run the full suite on Ethereum (start → wait → smoke → coverage → latency):
 
 ```bash
-scripts/run_suite.sh --repo . --stop
+scripts/run_suite.sh --repo . --chain-id 1 --stop
 ```
 
-Run with VM pools enabled:
+Run the full suite on Base:
 
 ```bash
-scripts/run_suite.sh --repo . --enable-vm-pools --stop
+scripts/run_suite.sh --repo . --chain-id 8453 --stop
 ```
 
 ## Suite Configuration
 
-- **Suites** (see `scripts/presets.py`):
+- **Suites** (see `scripts/presets.py`, chain-aware):
   - `smoke`, `core`, `extended`, `stables`, `lst`, `governance`, `v4_candidates`
-  - `extended` includes lower-liquidity pairs removed from `core`.
 - **Latency defaults** (override via env):
   - `LATENCY_REQUESTS` (default: 200)
   - `LATENCY_CONCURRENCY` (default: 8)
   - `LATENCY_CONCURRENCY_VM` (default: 4)
-- **Amounts**: per-token default ladders (e.g., 6-decimal stables, capped WBTC/WETH). Override with `--amounts` on each script.
+- **Amounts**: per-token default ladders. Override with `--amounts` on each script.
 
 ## Running Individual Steps
 
 Smoke test:
 ```bash
-python3 scripts/simulate_smoke.py --suite smoke
+python3 scripts/simulate_smoke.py --chain-id 1 --suite smoke
 ```
 
 Coverage sweep (writes JSON report):
 ```bash
-python3 scripts/coverage_sweep.py --suite core --out logs/coverage_sweep.json
+python3 scripts/coverage_sweep.py --chain-id 1 --suite core --out logs/coverage_sweep.json
 ```
 
 Latency percentiles:
 ```bash
-python3 scripts/latency_percentiles.py --suite core --requests 200 --concurrency 8
+python3 scripts/latency_percentiles.py --chain-id 1 --suite core --requests 200 --concurrency 8
 ```
 
 ## Output
@@ -67,12 +67,13 @@ python3 scripts/latency_percentiles.py --suite core --requests 200 --concurrency
 ## Troubleshooting
 
 - **Server not running**: `scripts/run_suite.sh` starts it for you. For manual control:
-  - `scripts/start_server.sh --repo .`
+  - `scripts/start_server.sh --repo . --chain-id 1`
   - `scripts/stop_server.sh --repo .`
 - **Readiness timeouts**: check `logs/tycho-sim-server.log` for startup errors.
+- **Wrong chain target**: use `scripts/wait_ready.sh --expect-chain-id <id>` to assert the running deployment.
 - **Partial successes**: `/simulate` returns `200 OK` even when `meta.status=partial_success`. The suite requires `ready` by default.
 
 ## Customization Notes
 
-- Use `--allow-status ready,partial_success` and `--allow-failures` on the Python scripts if you want to tolerate partial successes.
-- Change suites or token lists in `scripts/presets.py` to match your coverage needs.
+- Use `--allow-status ready,partial_success` and `--allow-failures` on Python scripts if you want to tolerate partial successes.
+- Change suites or token lists in `scripts/presets.py` per chain.
````

## lib/tycho-sim-stack.ts
Change summary: Adds mandatory `chainId` stack prop and injects `CHAIN_ID` into ECS task environment.
Risk level: Medium
Review focus: Confirm type restrictions and environment propagation through deployment stack synthesis.
Hunk context notes: Infra now controls required runtime chain selection; missing propagation would break startup in production.
````diff
diff --git a/lib/tycho-sim-stack.ts b/lib/tycho-sim-stack.ts
index c54b584..6a3c58c 100644
--- a/lib/tycho-sim-stack.ts
+++ b/lib/tycho-sim-stack.ts
@@ -15,6 +15,7 @@ export interface AppServiceStackProps extends cdk.StackProps {
   serviceName: string;
   tycho_tvl: string;
   tycho_url: string;
+  chainId: "1";
   containerPort?: number;
   environment: string;
   cpu?: number;
@@ -112,6 +113,7 @@ export class AppServiceStack extends cdk.Stack {
           environment: {
             TVL_THRESHOLD: props.tycho_tvl,
             TYCHO_URL: props.tycho_url,
+            CHAIN_ID: props.chainId,
             QUOTE_TIMEOUT_MS: "4000",
             POOL_TIMEOUT_NATIVE_MS: "250",
             POOL_TIMEOUT_VM_MS: "1000",
````

## lib/pipeline/pipeline-stack.ts
Change summary: Wires `chainId: "1"` into pipeline stack service instantiations.
Risk level: Low
Review focus: Check all stack call-sites that instantiate `AppServiceStack` remain updated when new services are added.
Hunk context notes: Current change is small, but this file is a central deployment fan-out point.
````diff
diff --git a/lib/pipeline/pipeline-stack.ts b/lib/pipeline/pipeline-stack.ts
index fa5a5df..d2e0599 100644
--- a/lib/pipeline/pipeline-stack.ts
+++ b/lib/pipeline/pipeline-stack.ts
@@ -75,6 +75,7 @@ export class PipelineStack extends cdk.Stack {
         desiredCount: 13,
         tycho_tvl: "10",
         tycho_url: "tycho-beta.propellerheads.xyz",
+        chainId: "1",
       }),
     );
 
@@ -89,6 +90,7 @@ export class PipelineStack extends cdk.Stack {
         memoryMiB: 1024,
         tycho_tvl: "50",
         tycho_url: "tycho-beta.propellerheads.xyz",
+        chainId: "1",
       }),
     );
   }
````

## Reproduce Patch Locally
```bash
git diff --no-color --unified=3 master...HEAD
```

```bash
# File-level parity check (full-fidelity blocks in this guide should match command output)
git diff --no-color --unified=3 master...HEAD -- <path>
```