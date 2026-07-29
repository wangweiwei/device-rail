use std::{net::SocketAddr, sync::Arc, time::Duration};

use devicerail_core::ExecutionControl;
use futures_util::future::join_all;
use thiserror::Error;
use tokio::net::TcpStream;

use crate::{
    ConfiguredPeer, ConfiguredPeers, MAX_PEER_TRANSPORT_SHARDS, MemoryTelemetry,
    NdjsonPeerTransport, PeerSecurity, PeerTransport, RemoteDeviceDriver, RemoteNode, RouteError,
    RouterConfig, TelemetrySink,
};

const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const CONNECT_RETRY_BACKOFF: Duration = Duration::from_millis(25);

#[derive(Debug, Error, Clone, Copy, PartialEq, Eq)]
pub enum ConnectorError {
    #[error("configured peer tunnel could not be connected")]
    Connect,
    #[error("configured peer tunnel setup is invalid")]
    Security,
    #[error("configured peer discovery or route construction failed")]
    Discovery,
    #[error("configured peer discovery was cancelled")]
    Cancelled,
    #[error("configured peer discovery timed out")]
    TimedOut,
}

/// Connects only to owner-validated loopback tunnel endpoints and discovers
/// authenticated remote Drivers. There is no public bind path in this crate.
/// Every configured peer is mandatory: one failure rejects the whole startup
/// set rather than silently returning a partial distributed inventory.
pub async fn connect_configured_peers(
    config: &ConfiguredPeers,
    router_config: RouterConfig,
    telemetry: Option<Arc<dyn TelemetrySink>>,
    control: &ExecutionControl,
) -> Result<Vec<RemoteDeviceDriver>, ConnectorError> {
    let telemetry =
        telemetry.unwrap_or_else(|| Arc::new(MemoryTelemetry::default()) as Arc<dyn TelemetrySink>);
    let discoveries =
        config.peers().iter().cloned().map(|peer| {
            discover_configured_peer(peer, router_config, Arc::clone(&telemetry), control)
        });
    // Every peer remains mandatory. `join_all` preserves configuration order,
    // so simultaneous failures still map deterministically to the first
    // declared peer while startup latency becomes the slowest peer rather than
    // the sum of all peer latencies.
    let mut drivers = Vec::new();
    for discovered in join_all(discoveries).await {
        drivers.extend(discovered?);
    }
    drivers.sort_by(|left, right| {
        devicerail_core::DeviceDriver::id(left).cmp(devicerail_core::DeviceDriver::id(right))
    });
    Ok(drivers)
}

async fn discover_configured_peer(
    peer: ConfiguredPeer,
    router_config: RouterConfig,
    telemetry: Arc<dyn TelemetrySink>,
    control: &ExecutionControl,
) -> Result<Vec<RemoteDeviceDriver>, ConnectorError> {
    if control.is_cancelled() {
        return Err(ConnectorError::Cancelled);
    }
    if control.is_expired() {
        return Err(ConnectorError::TimedOut);
    }
    let security =
        PeerSecurity::external_tunnel(peer.tunnel_id()).map_err(|_| ConnectorError::Security)?;
    let primary = RemoteNode::discover(
        connect_peer_transport(&peer, security.clone(), control).await?,
        Some(Arc::clone(&telemetry)),
        router_config,
        control,
    )
    .await
    .map_err(map_route)?;
    let shard_count = primary
        .inventory()
        .devices
        .len()
        .min(MAX_PEER_TRANSPORT_SHARDS);
    let additional = (1..shard_count).map(|_| async {
        RemoteNode::discover(
            connect_peer_transport(&peer, security.clone(), control).await?,
            Some(Arc::clone(&telemetry)),
            router_config,
            control,
        )
        .await
        .map_err(map_route)
    });
    let mut shard_nodes = Vec::with_capacity(shard_count);
    shard_nodes.push(primary);
    shard_nodes.extend(
        join_all(additional)
            .await
            .into_iter()
            .collect::<Result<Vec<_>, _>>()?,
    );
    let node = RemoteNode::with_transport_shards(shard_nodes).map_err(map_route)?;
    node.drivers(peer.owner_id(), peer.driver_config(), control)
        .await
        .map_err(map_route)
}

async fn connect_peer_transport(
    peer: &ConfiguredPeer,
    security: PeerSecurity,
    control: &ExecutionControl,
) -> Result<Arc<dyn PeerTransport>, ConnectorError> {
    let stream = connect_loopback_with_retry(peer.endpoint(), control, CONNECT_TIMEOUT).await?;
    stream
        .set_nodelay(true)
        .map_err(|_| ConnectorError::Connect)?;
    Ok(NdjsonPeerTransport::new(
        stream,
        peer.node_id().clone(),
        security,
    ))
}

async fn connect_loopback_with_retry(
    endpoint: SocketAddr,
    control: &ExecutionControl,
    connect_timeout: Duration,
) -> Result<TcpStream, ConnectorError> {
    if control.is_cancelled() {
        return Err(ConnectorError::Cancelled);
    }
    if control.is_expired() {
        return Err(ConnectorError::TimedOut);
    }

    let control_remaining = control.remaining();
    let control_is_deadline =
        control_remaining.is_some_and(|remaining| remaining <= connect_timeout);
    let budget =
        control_remaining.map_or(connect_timeout, |remaining| remaining.min(connect_timeout));
    let deadline = tokio::time::Instant::now() + budget;
    let deadline_error = if control_is_deadline {
        ConnectorError::TimedOut
    } else {
        ConnectorError::Connect
    };

    loop {
        let connected = tokio::select! {
            biased;
            _ = control.cancelled() => return Err(ConnectorError::Cancelled),
            _ = tokio::time::sleep_until(deadline) => return Err(deadline_error),
            connected = TcpStream::connect(endpoint) => connected,
        };
        if let Ok(stream) = connected {
            return Ok(stream);
        }

        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            return Err(deadline_error);
        }
        tokio::select! {
            biased;
            _ = control.cancelled() => return Err(ConnectorError::Cancelled),
            _ = tokio::time::sleep(CONNECT_RETRY_BACKOFF.min(remaining)) => {}
        }
    }
}

fn map_route(error: RouteError) -> ConnectorError {
    match error {
        RouteError::Transport(
            crate::TransportError::Cancelled | crate::TransportError::CancelledAfterSend,
        ) => ConnectorError::Cancelled,
        RouteError::Transport(
            crate::TransportError::TimedOut | crate::TransportError::TimedOutAfterSend,
        ) => ConnectorError::TimedOut,
        _ => ConnectorError::Discovery,
    }
}

#[cfg(test)]
mod tests {
    use std::{net::TcpListener as StdTcpListener, time::Duration};

    use devicerail_core::{CancellationReason, ExecutionController, TimeoutScope};
    use tokio::net::TcpSocket;

    use super::{ConnectorError, connect_loopback_with_retry};

    fn reserve_loopback_address() -> std::net::SocketAddr {
        let listener = StdTcpListener::bind("127.0.0.1:0").expect("reserve loopback address");
        let address = listener.local_addr().expect("reserved loopback address");
        drop(listener);
        address
    }

    #[tokio::test]
    async fn connect_retries_until_a_later_loopback_listener_is_ready() {
        let socket = TcpSocket::new_v4().expect("create delayed loopback socket");
        socket
            .bind("127.0.0.1:0".parse().expect("loopback address"))
            .expect("reserve delayed loopback socket");
        let address = socket.local_addr().expect("delayed loopback address");
        let (_controller, control) =
            ExecutionController::with_timeout(2_000, TimeoutScope::Request);
        let connector = tokio::spawn(async move {
            connect_loopback_with_retry(address, &control, Duration::from_secs(1)).await
        });

        tokio::time::sleep(Duration::from_millis(75)).await;
        let listener = socket
            .listen(128)
            .expect("listen on reserved loopback socket");
        let client = tokio::time::timeout(Duration::from_secs(1), connector)
            .await
            .expect("connector remained bounded")
            .expect("connector task")
            .expect("connector retried after connection refusal");
        let (server, peer) = tokio::time::timeout(Duration::from_secs(1), listener.accept())
            .await
            .expect("delayed listener accepted in time")
            .expect("accept delayed connection");
        assert!(peer.ip().is_loopback());
        drop((client, server));
    }

    #[tokio::test]
    async fn connect_retry_stops_immediately_when_cancelled() {
        let address = reserve_loopback_address();
        let (controller, control) = ExecutionController::new();
        let connector = tokio::spawn(async move {
            connect_loopback_with_retry(address, &control, Duration::from_secs(1)).await
        });

        tokio::time::sleep(Duration::from_millis(50)).await;
        assert!(controller.cancel(CancellationReason::Shutdown));
        let result = tokio::time::timeout(Duration::from_secs(1), connector)
            .await
            .expect("cancelled connector remained bounded")
            .expect("connector task");
        assert!(matches!(result, Err(ConnectorError::Cancelled)));
    }

    #[tokio::test]
    async fn caller_deadline_bounds_connect_retries() {
        let address = reserve_loopback_address();
        let (_controller, control) = ExecutionController::with_timeout(75, TimeoutScope::Request);
        let result = tokio::time::timeout(
            Duration::from_secs(1),
            connect_loopback_with_retry(address, &control, Duration::from_secs(1)),
        )
        .await
        .expect("deadline-bounded connector stopped");
        assert!(matches!(result, Err(ConnectorError::TimedOut)));
    }

    #[tokio::test]
    async fn connect_budget_exhaustion_preserves_connect_error() {
        let address = reserve_loopback_address();
        let control = devicerail_core::ExecutionControl::unbounded();
        let result = tokio::time::timeout(
            Duration::from_secs(1),
            connect_loopback_with_retry(address, &control, Duration::from_millis(75)),
        )
        .await
        .expect("connect-budget-bounded connector stopped");
        assert!(matches!(result, Err(ConnectorError::Connect)));
    }
}
