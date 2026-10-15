use std::future::Future;
use tokio::sync::Mutex;

// Lock order: this is the sole refresh lock, scoped to one resolved login. The
// guarded operation never acquires another refresh lock; account initialization
// does not call user code. Hold it across I/O and release it on every exit/drop.
pub(crate) async fn serialized<T>(lock: &Mutex<()>, operation: impl Future<Output = T>) -> T {
    let _guard = lock.lock().await;
    operation.await
}
