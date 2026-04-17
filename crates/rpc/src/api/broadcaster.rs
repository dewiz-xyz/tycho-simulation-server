use axum::{routing::get, Router};

use runtime::broadcaster_service::BroadcasterAppState;

use crate::handlers::broadcaster::{status, ws};

pub fn create_broadcaster_router(app_state: BroadcasterAppState) -> Router {
    Router::new()
        .route("/status", get(status))
        .route("/ws", get(ws))
        .with_state(app_state)
}

#[cfg(test)]
mod tests {
    use std::any::Any;
    use std::collections::HashMap;

    use anyhow::{bail, Result};
    use axum::{
        body::{to_bytes, Body},
        http::{Request, StatusCode},
        Router,
    };
    use num_bigint::BigUint;
    use num_traits::Zero;
    use runtime::{
        broadcaster_service::BroadcasterAppState,
        models::broadcaster::{BroadcasterSnapshotCache, BroadcasterUpstreamState},
        services::broadcaster::BroadcasterServiceState,
    };
    use simulator_core::broadcaster::BroadcasterBackend;
    use tokio::task::JoinHandle;
    use tokio_tungstenite::{connect_async, tungstenite};
    use tower::ServiceExt;
    use tycho_simulation::{
        protocol::models::{ProtocolComponent, Update},
        tycho_client::feed::{BlockHeader, SynchronizerState},
        tycho_common::{
            dto::ProtocolStateDelta,
            models::{token::Token, Chain},
            simulation::{
                errors::{SimulationError, TransitionError},
                protocol_sim::{Balances, GetAmountOutResult, ProtocolSim},
            },
            Bytes,
        },
    };

    use super::create_broadcaster_router;

    #[derive(Debug, Clone, serde::Deserialize, serde::Serialize)]
    struct DummySim(u8);

    #[typetag::serde]
    impl ProtocolSim for DummySim {
        fn fee(&self) -> f64 {
            0.0
        }

        fn spot_price(&self, _base: &Token, _quote: &Token) -> Result<f64, SimulationError> {
            Ok(0.0)
        }

        fn get_amount_out(
            &self,
            amount_in: BigUint,
            _token_in: &Token,
            _token_out: &Token,
        ) -> Result<GetAmountOutResult, SimulationError> {
            Ok(GetAmountOutResult::new(
                amount_in,
                BigUint::zero(),
                self.clone_box(),
            ))
        }

        fn get_limits(
            &self,
            _sell_token: Bytes,
            _buy_token: Bytes,
        ) -> Result<(BigUint, BigUint), SimulationError> {
            Ok((BigUint::zero(), BigUint::zero()))
        }

        fn delta_transition(
            &mut self,
            _delta: ProtocolStateDelta,
            _tokens: &HashMap<Bytes, Token>,
            _balances: &Balances,
        ) -> Result<(), TransitionError> {
            Ok(())
        }

        fn clone_box(&self) -> Box<dyn ProtocolSim> {
            Box::new(self.clone())
        }

        fn as_any(&self) -> &dyn Any {
            self
        }

        fn as_any_mut(&mut self) -> &mut dyn Any {
            self
        }

        fn eq(&self, other: &dyn ProtocolSim) -> bool {
            other
                .as_any()
                .downcast_ref::<DummySim>()
                .map(|value| value.0 == self.0)
                .unwrap_or(false)
        }
    }

    #[derive(Clone, Copy)]
    enum SeedMode {
        Disconnected,
        WarmingUp,
        Ready,
    }

    #[tokio::test]
    async fn status_reports_upstream_disconnected() -> Result<()> {
        let app = create_broadcaster_router(build_state(SeedMode::Disconnected).await?);
        let (status, body) = get_json(app, "/status").await?;

        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(body["status"], "upstream_disconnected");
        assert_eq!(body["upstream"]["connected"], false);
        assert_eq!(body["snapshot"]["ready"], false);
        assert_eq!(
            body["backends"]["native"]["block_number"],
            serde_json::Value::Null
        );
        Ok(())
    }

    #[tokio::test]
    async fn status_reports_snapshot_warming_up() -> Result<()> {
        let app = create_broadcaster_router(build_state(SeedMode::WarmingUp).await?);
        let (status, body) = get_json(app, "/status").await?;

        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(body["status"], "snapshot_warming_up");
        assert_eq!(body["upstream"]["connected"], true);
        assert_eq!(body["snapshot"]["ready"], false);
        Ok(())
    }

    #[tokio::test]
    async fn status_reports_ready_once_snapshot_is_bootstrapped() -> Result<()> {
        let app = create_broadcaster_router(build_state(SeedMode::Ready).await?);
        let (status, body) = get_json(app, "/status").await?;

        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["status"], "ready");
        assert_eq!(body["snapshot"]["ready"], true);
        assert_eq!(body["backends"]["native"]["block_number"], 10);
        assert_eq!(body["backends"]["native"]["pool_count"], 1);
        Ok(())
    }

    #[tokio::test]
    async fn websocket_upgrade_is_rejected_until_ready() -> Result<()> {
        let (url, server_task) = spawn_server(
            create_broadcaster_router(build_state(SeedMode::WarmingUp).await?),
            "/ws",
        )
        .await?;
        let result = connect_async(url).await;
        server_task.abort();

        match result {
            Err(tungstenite::Error::Http(response)) => {
                assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
            }
            Err(error) => bail!("unexpected websocket error: {error}"),
            Ok(_) => bail!("expected websocket handshake rejection"),
        }
        Ok(())
    }

    #[tokio::test]
    async fn websocket_upgrade_is_admitted_once_ready() -> Result<()> {
        let (url, server_task) = spawn_server(
            create_broadcaster_router(build_state(SeedMode::Ready).await?),
            "/ws",
        )
        .await?;
        let result = connect_async(url).await;
        server_task.abort();

        let (_stream, response) = match result {
            Ok(result) => result,
            Err(error) => bail!("expected websocket handshake success: {error}"),
        };
        assert_eq!(response.status(), StatusCode::SWITCHING_PROTOCOLS);
        Ok(())
    }

    async fn build_state(mode: SeedMode) -> Result<BroadcasterAppState> {
        let cache = BroadcasterSnapshotCache::new(1, vec![BroadcasterBackend::Native]);
        let upstream = BroadcasterUpstreamState::default();
        let service = BroadcasterServiceState::new(2, 8, cache, upstream);

        match mode {
            SeedMode::Disconnected => {}
            SeedMode::WarmingUp => service.mark_upstream_connected().await,
            SeedMode::Ready => {
                service.mark_upstream_connected().await;
                service.apply_update(&native_only_update()).await?;
            }
        }

        Ok(BroadcasterAppState::new(service))
    }

    async fn get_json(app: Router, uri: &str) -> Result<(StatusCode, serde_json::Value)> {
        let response = app
            .oneshot(Request::builder().uri(uri).body(Body::empty())?)
            .await?;
        let status = response.status();
        let body: serde_json::Value =
            serde_json::from_slice(&to_bytes(response.into_body(), usize::MAX).await?)?;
        Ok((status, body))
    }

    async fn spawn_server(app: Router, path: &str) -> Result<(String, JoinHandle<()>)> {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
        let addr = listener.local_addr()?;
        let server_task = tokio::spawn(async move {
            let _ = axum::serve(listener, app.into_make_service()).await;
        });
        Ok((format!("ws://{addr}{path}"), server_task))
    }

    fn native_only_update() -> Update {
        let mut new_pairs = HashMap::new();
        new_pairs.insert(
            "native-1".to_string(),
            native_component("native-1", "uniswap_v2"),
        );

        let mut states = HashMap::new();
        states.insert(
            "native-1".to_string(),
            Box::new(DummySim(1)) as Box<dyn ProtocolSim>,
        );

        Update::new(10, states, new_pairs).set_sync_states(HashMap::from([(
            "uniswap_v2".to_string(),
            SynchronizerState::Ready(block_header(10, 1)),
        )]))
    }

    fn native_component(_id: &str, protocol: &str) -> ProtocolComponent {
        ProtocolComponent::new(
            Bytes::from([3u8; 20]),
            protocol.to_string(),
            protocol.to_string(),
            Chain::Ethereum,
            vec![dummy_token(1, "TKNA"), dummy_token(2, "TKNB")],
            Vec::new(),
            HashMap::new(),
            Bytes::from([9u8; 32]),
            chrono::DateTime::UNIX_EPOCH.naive_utc(),
        )
    }

    fn dummy_token(seed: u8, symbol: &str) -> Token {
        Token::new(
            &Bytes::from([seed; 20]),
            symbol,
            18,
            0,
            &[],
            Chain::Ethereum,
            1,
        )
    }

    fn block_header(number: u64, seed: u8) -> BlockHeader {
        BlockHeader {
            hash: Bytes::from(vec![seed; 32]),
            number,
            parent_hash: Bytes::from(vec![seed.saturating_add(1); 32]),
            revert: false,
            timestamp: number * 10,
            partial_block_index: None,
        }
    }
}
