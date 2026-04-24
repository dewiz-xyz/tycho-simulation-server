use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use tokio::sync::{mpsc, oneshot, Mutex};

use crate::models::broadcaster::{BroadcasterSnapshotExport, BroadcasterSubscriberSnapshot};
use simulator_core::broadcaster::BroadcasterPayload;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionCloseReason {
    Lagged,
    GenerationReset,
    Shutdown,
}

impl SessionCloseReason {
    const fn label(self) -> &'static str {
        match self {
            Self::Lagged => "lagged",
            Self::GenerationReset => "generation_reset",
            Self::Shutdown => "shutdown",
        }
    }
}

pub struct BroadcasterSessionRegistration {
    pub session_id: u64,
    pub stream_id: String,
    pub snapshot_payloads: Vec<BroadcasterPayload>,
    pub receiver: mpsc::Receiver<BroadcasterPayload>,
    pub close_receiver: oneshot::Receiver<SessionCloseReason>,
}

#[derive(Debug, Clone)]
pub struct BroadcasterSubscriberRegistry {
    buffer_capacity: usize,
    next_session_id: Arc<AtomicU64>,
    lag_disconnects: Arc<AtomicU64>,
    last_error: Arc<Mutex<Option<String>>>,
    inner: Arc<Mutex<HashMap<u64, SubscriberHandle>>>,
}

#[derive(Debug)]
struct SubscriberHandle {
    sender: mpsc::Sender<BroadcasterPayload>,
    close_tx: Option<oneshot::Sender<SessionCloseReason>>,
}

impl BroadcasterSubscriberRegistry {
    pub fn new(buffer_capacity: usize) -> Self {
        Self {
            buffer_capacity,
            next_session_id: Arc::new(AtomicU64::new(1)),
            lag_disconnects: Arc::new(AtomicU64::new(0)),
            last_error: Arc::new(Mutex::new(None)),
            inner: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    pub async fn register(
        &self,
        snapshot: BroadcasterSnapshotExport,
    ) -> BroadcasterSessionRegistration {
        let session_id = self.next_session_id.fetch_add(1, Ordering::Relaxed);
        let (sender, receiver) = mpsc::channel(self.buffer_capacity);
        let (close_tx, close_receiver) = oneshot::channel();

        self.inner.lock().await.insert(
            session_id,
            SubscriberHandle {
                sender,
                close_tx: Some(close_tx),
            },
        );

        BroadcasterSessionRegistration {
            session_id,
            stream_id: snapshot.stream_id,
            snapshot_payloads: snapshot.payloads,
            receiver,
            close_receiver,
        }
    }

    pub async fn broadcast(&self, payload: BroadcasterPayload) {
        let mut lagged = Vec::new();
        let mut closed = Vec::new();
        let mut last_error = None;
        let mut guard = self.inner.lock().await;

        for (session_id, handle) in guard.iter_mut() {
            match handle.sender.try_send(payload.clone()) {
                Ok(()) => {}
                Err(mpsc::error::TrySendError::Full(_)) => {
                    self.lag_disconnects.fetch_add(1, Ordering::Relaxed);
                    if let Some(close_tx) = handle.close_tx.take() {
                        let _ = close_tx.send(SessionCloseReason::Lagged);
                    }
                    last_error = Some(format!(
                        "subscriber {session_id} disconnected: {}",
                        SessionCloseReason::Lagged.label()
                    ));
                    lagged.push(*session_id);
                }
                Err(mpsc::error::TrySendError::Closed(_)) => {
                    last_error = Some(format!(
                        "subscriber {session_id} disconnected: channel_closed"
                    ));
                    closed.push(*session_id);
                }
            }
        }

        for session_id in lagged.into_iter().chain(closed) {
            guard.remove(&session_id);
        }
        drop(guard);

        if let Some(last_error) = last_error {
            self.record_last_error(last_error).await;
        }
    }

    pub async fn remove(&self, session_id: u64) {
        self.inner.lock().await.remove(&session_id);
    }

    pub async fn disconnect_all(&self, reason: SessionCloseReason) {
        let mut guard = self.inner.lock().await;
        for handle in guard.values_mut() {
            if let Some(close_tx) = handle.close_tx.take() {
                let _ = close_tx.send(reason);
            }
        }
        guard.clear();
        drop(guard);

        self.record_last_error(format!("all subscribers disconnected: {}", reason.label()))
            .await;
    }

    pub async fn snapshot(&self) -> BroadcasterSubscriberSnapshot {
        BroadcasterSubscriberSnapshot {
            active: self.inner.lock().await.len(),
            lag_disconnects: self.lag_disconnects.load(Ordering::Relaxed),
            last_error: self.last_error.lock().await.clone(),
        }
    }

    async fn record_last_error(&self, message: String) {
        *self.last_error.lock().await = Some(message);
    }
}

#[cfg(test)]
mod tests {
    use anyhow::Result;

    use super::{BroadcasterSubscriberRegistry, SessionCloseReason};
    use crate::models::broadcaster::BroadcasterSnapshotExport;
    use simulator_core::broadcaster::{
        BroadcasterPayload, BroadcasterSnapshotEnd, BroadcasterSnapshotStart,
    };

    fn snapshot_export() -> BroadcasterSnapshotExport {
        BroadcasterSnapshotExport {
            stream_id: "stream-1".to_string(),
            payloads: vec![
                BroadcasterPayload::SnapshotStart(
                    BroadcasterSnapshotStart::new("snapshot-1", 1, vec![], 0)
                        .unwrap_or_else(|_| unreachable!("snapshot_start")),
                ),
                BroadcasterPayload::SnapshotEnd(BroadcasterSnapshotEnd::new("snapshot-1")),
            ],
        }
    }

    #[tokio::test]
    async fn lagged_subscriber_is_disconnected_without_affecting_fast_peer() -> Result<()> {
        let registry = BroadcasterSubscriberRegistry::new(1);
        let lagged = registry.register(snapshot_export()).await;
        let mut fast = registry.register(snapshot_export()).await;

        registry
            .broadcast(BroadcasterPayload::SnapshotEnd(
                BroadcasterSnapshotEnd::new("snapshot-1"),
            ))
            .await;
        assert!(fast.receiver.try_recv().is_ok());
        registry
            .broadcast(BroadcasterPayload::SnapshotEnd(
                BroadcasterSnapshotEnd::new("snapshot-1"),
            ))
            .await;

        let reason = lagged.close_receiver.await?;
        assert_eq!(reason, SessionCloseReason::Lagged);
        assert!(fast.receiver.try_recv().is_ok());
        let snapshot = registry.snapshot().await;
        assert_eq!(snapshot.active, 1);
        assert_eq!(snapshot.lag_disconnects, 1);
        assert_eq!(
            snapshot.last_error.as_deref(),
            Some("subscriber 1 disconnected: lagged")
        );
        Ok(())
    }

    #[tokio::test]
    async fn disconnect_all_clears_registry() -> Result<()> {
        let registry = BroadcasterSubscriberRegistry::new(2);
        let session = registry.register(snapshot_export()).await;

        registry
            .disconnect_all(SessionCloseReason::GenerationReset)
            .await;

        let reason = session.close_receiver.await?;
        assert_eq!(reason, SessionCloseReason::GenerationReset);
        let snapshot = registry.snapshot().await;
        assert_eq!(snapshot.active, 0);
        assert_eq!(
            snapshot.last_error.as_deref(),
            Some("all subscribers disconnected: generation_reset")
        );
        Ok(())
    }
}
