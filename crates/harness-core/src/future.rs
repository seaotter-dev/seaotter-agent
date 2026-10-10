use std::future::Future;
use std::pin::Pin;

/// Boxed `Send` future returned by the harness traits so they stay usable as
/// trait objects.
pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;
