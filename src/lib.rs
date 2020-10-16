//! A global executor built on top of async-executor and async_io
//!
//! The global executor is lazily spawned on first use. It spawns as many threads
//! as the number of cpus by default. You can override this using the
//! `ASYNC_GLOBAL_EXECUTOR_THREADS` environment variable.
//!
//! # Examples
//!
//! ```
//! # use futures_lite::future;
//!
//! // spawn a task on the multi-threaded executor
//! let task1 = async_global_executor::spawn(async {
//!     1 + 2
//! });
//! // spawn a task on the local executor (same thread)
//! let task2 = async_global_executor::spawn_local(async {
//!     3 + 4
//! });
//! let task = future::zip(task1, task2);
//!
//! // run the executor
//! async_global_executor::block_on(async {
//!     assert_eq!(task.await, (3, 7));
//! });
//! ```

#![forbid(unsafe_code)]
#![warn(missing_docs, missing_debug_implementations, rust_2018_idioms)]

#[cfg(doctest)]
doc_comment::doctest!("../README.md");

use async_executor::{Executor, LocalExecutor};
use futures_lite::future;
use once_cell::sync::Lazy;
use std::{
    future::Future,
    sync::atomic::{AtomicBool, Ordering},
    thread,
};

pub use async_executor::Task;

mod reactor {
    pub(crate) fn block_on<F: std::future::Future<Output = T>, T>(future: F) -> T {
        #[cfg(feature = "async-io")]
        use async_io::block_on;
        #[cfg(not(feature = "async-io"))]
        use future::block_on;
        #[cfg(feature = "tokio02")]
        let future = async_compat::Compat::new(future);
        #[cfg(feature = "tokio03")]
        let _tokio93_enter = crate::TOKIO03.enter();
        block_on(future)
    }
}

static GLOBAL_EXECUTOR_INIT: AtomicBool = AtomicBool::new(false);
static GLOBAL_EXECUTOR_THREADS: Lazy<()> = Lazy::new(init);

static GLOBAL_EXECUTOR: Executor<'_> = Executor::new();

thread_local! {
    static LOCAL_EXECUTOR: LocalExecutor<'static> = LocalExecutor::new();
}

#[cfg(feature = "tokio03")]
static TOKIO03: Lazy<std::sync::Arc<tokio03_crate::runtime::Runtime>> = Lazy::new(|| {
    let rt = std::sync::Arc::new(
        tokio03_crate::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("failed to build tokio03 runtime"),
    );
    {
        let rt = rt.clone();
        thread::Builder::new()
            .name("async-global-executor/tokio03".to_string())
            .spawn(move || {
                rt.block_on(futures_lite::future::pending::<()>());
            })
            .unwrap();
    }
    rt
});

/// Configuration to init the thread pool for the multi-threaded global executor.
#[derive(Default, Debug)]
pub struct GlobalExecutorConfig {
    /// The environment variable from which we'll try to parse the number of threads to spawn.
    pub env_var: Option<&'static str>,
    /// The default number of threads to spawn.
    pub default_threads: Option<usize>,
    /// The name to us fo the threads.
    pub thread_name: Option<String>,
    /// The prefix used to name the threads.
    pub thread_name_prefix: Option<&'static str>,
}

impl GlobalExecutorConfig {
    /// Use the specified environment variable to find the number of threads to spawn.
    pub fn with_env_var(mut self, env_var: &'static str) -> Self {
        self.env_var = Some(env_var);
        self
    }

    /// Use the specified value as the default number of threads.
    pub fn with_default_threads(mut self, default_threads: usize) -> Self {
        self.default_threads = Some(default_threads);
        self
    }

    /// Use the specified prefix to name the threads.
    pub fn with_thread_name(mut self, thread_name: String) -> Self {
        self.thread_name = Some(thread_name);
        self
    }

    /// Use the specified prefix to name the threads.
    pub fn with_thread_name_prefix(mut self, thread_name_prefix: &'static str) -> Self {
        self.thread_name_prefix = Some(thread_name_prefix);
        self
    }
}

/// Init the global executor, spawning as many threads as specified or
/// the value specified by the specified environment variable.
///
/// # Examples
///
/// ```
/// async_global_executor::init_with_config(
///     async_global_executor::GlobalExecutorConfig::default()
///         .with_env_var("NUMBER_OF_THREADS")
///         .with_default_threads(4)
///         .with_thread_name_prefix("thread-")
/// );
/// ```
pub fn init_with_config(config: GlobalExecutorConfig) {
    if !GLOBAL_EXECUTOR_INIT.compare_and_swap(false, true, Ordering::AcqRel) {
        let num_cpus = std::env::var(config.env_var.unwrap_or("ASYNC_GLOBAL_EXECUTOR_THREADS"))
            .ok()
            .and_then(|threads| threads.parse().ok())
            .or(config.default_threads)
            .unwrap_or_else(num_cpus::get)
            .max(1);
        for n in 1..=num_cpus {
            thread::Builder::new()
                .name(config.thread_name.clone().unwrap_or_else(|| {
                    format!(
                        "{}{}",
                        config
                            .thread_name_prefix
                            .unwrap_or("async-global-executor-"),
                        n
                    )
                }))
                .spawn(|| loop {
                    let _ = std::panic::catch_unwind(|| {
                        LOCAL_EXECUTOR.with(|executor| {
                            let local = executor.run(future::pending::<()>());
                            let global = GLOBAL_EXECUTOR.run(future::pending::<()>());
                            reactor::block_on(future::or(local, global))
                        })
                    });
                })
                .expect("cannot spawn executor thread");
        }
    }
}

/// Init the global executor, spawning as many threads as the number or cpus or
/// the value specified by the `ASYNC_GLOBAL_EXECUTOR_THREADS` environment variable
/// if specified.
///
/// # Examples
///
/// ```
/// async_global_executor::init();
/// ```
pub fn init() {
    init_with_config(GlobalExecutorConfig::default());
}

/// Runs the global and the local executor on the current thread
///
/// Note: this calls `async_io::block_on` underneath.
///
/// # Examples
///
/// ```
/// let task = async_global_executor::spawn(async {
///     1 + 2
/// });
/// async_global_executor::block_on(async {
///     assert_eq!(task.await, 3);
/// });
/// ```
pub fn block_on<F: Future<Output = T>, T>(future: F) -> T {
    LOCAL_EXECUTOR.with(|executor| reactor::block_on(executor.run(future)))
}

/// Spawns a task onto the multi-threaded global executor.
///
/// # Examples
///
/// ```
/// # use futures_lite::future;
///
/// let task1 = async_global_executor::spawn(async {
///     1 + 2
/// });
/// let task2 = async_global_executor::spawn(async {
///     3 + 4
/// });
/// let task = future::zip(task1, task2);
///
/// async_global_executor::block_on(async {
///     assert_eq!(task.await, (3, 7));
/// });
/// ```
pub fn spawn<F: Future<Output = T> + Send + 'static, T: Send + 'static>(future: F) -> Task<T> {
    Lazy::force(&GLOBAL_EXECUTOR_THREADS);
    GLOBAL_EXECUTOR.spawn(future)
}

/// Spawns a task onto the local executor.
///
///
/// The task does not need to be `Send` as it will be spawned on the same thread.
///
/// # Examples
///
/// ```
/// # use futures_lite::future;
///
/// let task1 = async_global_executor::spawn_local(async {
///     1 + 2
/// });
/// let task2 = async_global_executor::spawn_local(async {
///     3 + 4
/// });
/// let task = future::zip(task1, task2);
///
/// async_global_executor::block_on(async {
///     assert_eq!(task.await, (3, 7));
/// });
/// ```
pub fn spawn_local<F: Future<Output = T> + 'static, T: 'static>(future: F) -> Task<T> {
    LOCAL_EXECUTOR.with(|executor| executor.spawn(future))
}

#[cfg(all(test, feature = "tokio02"))]
mod test_tokio02 {
    use super::*;
    use tokio02_crate as tokio;

    async fn compute() -> u8 {
        tokio::spawn(async { 1 + 2 }).await.unwrap()
    }

    #[test]
    fn spawn_tokio() {
        block_on(async {
            assert_eq!(
                spawn(compute()).await
                    + spawn_local(compute()).await
                    + tokio::spawn(compute()).await.unwrap(),
                9
            );
        });
    }
}

#[cfg(all(test, feature = "tokio03"))]
mod test_tokio03 {
    use super::*;
    use tokio03_crate as tokio;

    async fn compute() -> u8 {
        tokio::spawn(async { 1 + 2 }).await.unwrap()
    }

    #[test]
    fn spawn_tokio() {
        block_on(async {
            assert_eq!(
                spawn(compute()).await
                    + spawn_local(compute()).await
                    + tokio::spawn(compute()).await.unwrap(),
                9
            );
        });
    }
}
