use std::future::Future;

use axum::Router;
use tokio::net::TcpListener;

/// Serves `router` on `listener` until `shutdown` resolves.
pub async fn serve(
    listener: TcpListener,
    router: Router,
    shutdown: impl Future<Output = ()> + Send + 'static,
) -> std::io::Result<()> {
    axum::serve(listener, router)
        .with_graceful_shutdown(shutdown)
        .await
}
