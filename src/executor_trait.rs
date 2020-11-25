use async_trait::async_trait;
use executor_trait_crate::{Executor, Task};
use std::{future::Future, pin::Pin};

/// Dummy object implementing executor-trait common interfaces on top of async-global-executor
#[derive(Debug, Default, Copy, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct AsyncGlobalExecutor;

#[async_trait]
impl Executor for AsyncGlobalExecutor {
    fn spawn<T: Send + 'static>(
        &self,
        f: Pin<Box<dyn Future<Output = T> + Send>>,
    ) -> Box<dyn Task<T>> {
        Box::new(crate::spawn(f))
    }

    fn spawn_local<T: 'static>(&self, f: Pin<Box<dyn Future<Output = T>>>) -> Box<dyn Task<T>> {
        Box::new(crate::spawn_local(f))
    }

    async fn spawn_blocking<F: FnOnce() -> T + Send + 'static, T: Send + 'static>(
        &self,
        f: F,
    ) -> T {
        crate::spawn_blocking(f).await
    }
}
