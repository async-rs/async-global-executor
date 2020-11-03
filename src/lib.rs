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
use once_cell::sync::{Lazy, OnceCell};
use std::{
    future::Future,
    io,
    sync::atomic::{AtomicBool, AtomicUsize, Ordering},
    thread,
};

pub use async_executor::Task;

mod reactor {
    pub(crate) fn block_on<F: std::future::Future<Output = T>, T>(future: F) -> T {
        #[cfg(feature = "async-io")]
        let run = || async_io::block_on(future);
        #[cfg(not(feature = "async-io"))]
        let run = || future::block_on(future);
        #[cfg(feature = "tokio02")]
        let run = || crate::TOKIO02.enter(|| run());
        #[cfg(feature = "tokio03")]
        let _tokio03_enter = crate::TOKIO03.enter();
        run()
    }
}

static GLOBAL_EXECUTOR_CONFIG: OnceCell<Config> = OnceCell::new();
static GLOBAL_EXECUTOR_INIT: AtomicBool = AtomicBool::new(false);
static GLOBAL_EXECUTOR_THREADS: Lazy<()> = Lazy::new(init);

static GLOBAL_EXECUTOR_THREADS_NUMBER: AtomicUsize = AtomicUsize::new(0);
static GLOBAL_EXECUTOR_NEXT_THREAD: AtomicUsize = AtomicUsize::new(1);

static GLOBAL_EXECUTOR: Executor<'_> = Executor::new();

thread_local! {
    static LOCAL_EXECUTOR: LocalExecutor<'static> = LocalExecutor::new();
}

#[cfg(feature = "tokio02")]
static TOKIO02: Lazy<tokio02_crate::runtime::Handle> = Lazy::new(|| {
    let mut rt = tokio02_crate::runtime::Runtime::new().expect("failed to build tokio02 runtime");
    let handle = rt.handle().clone();
    thread::Builder::new()
        .name("async-global-executor/tokio02".to_string())
        .spawn(move || {
            rt.block_on(future::pending::<()>());
        })
        .expect("failed to spawn tokio02 driver thread");
    handle
});

#[cfg(feature = "tokio03")]
static TOKIO03: Lazy<tokio03_crate::runtime::Runtime> = Lazy::new(|| {
    thread::Builder::new()
        .name("async-global-executor/tokio03".to_string())
        .spawn(move || {
            TOKIO03.block_on(future::pending::<()>());
        })
        .expect("failed to spawn tokio03 driver thread");
    tokio03_crate::runtime::Runtime::new().expect("failed to build tokio03 runtime")
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

    fn seal(self) -> Config {
        let num_threads = std::env::var(self.env_var.unwrap_or("ASYNC_GLOBAL_EXECUTOR_THREADS"))
            .ok()
            .and_then(|threads| threads.parse().ok())
            .or(self.default_threads)
            .unwrap_or_else(num_cpus::get)
            .max(1);
        Config {
            min_threads: num_threads,
            max_threads: num_threads * 4, // FIXME: make this configurable in 2.0
            thread_name: self.thread_name, // FIXME: make this a Fn in 2.0
            thread_name_prefix: self.thread_name_prefix,
        }
    }
}

struct Config {
    min_threads: usize,
    max_threads: usize,
    thread_name: Option<String>,
    thread_name_prefix: Option<&'static str>,
}

impl Default for Config {
    fn default() -> Self {
        GlobalExecutorConfig::default().seal()
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
    let _ = GLOBAL_EXECUTOR_CONFIG.set(config.seal());
    init();
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
    let config = GLOBAL_EXECUTOR_CONFIG.get_or_init(Config::default);
    if !GLOBAL_EXECUTOR_INIT.compare_and_swap(false, true, Ordering::AcqRel) {
        spawn_more_threads(config.min_threads).expect("cannot spawn executor threads");
    }
}

/// Spawn more executor threads, up to configured max value.
///
/// # Examples
///
/// ```
/// async_global_executor::spawn_more_threads(2);
/// ```
pub fn spawn_more_threads(count: usize) -> io::Result<()> {
    let config = GLOBAL_EXECUTOR_CONFIG.get().unwrap_or_else(|| {
        Lazy::force(&GLOBAL_EXECUTOR_THREADS);
        GLOBAL_EXECUTOR_CONFIG.get().unwrap()
    });
    let count = count.min(config.max_threads - GLOBAL_EXECUTOR_THREADS_NUMBER.load(Ordering::SeqCst));
    for _ in 0..count {
        thread::Builder::new()
            .name(config.thread_name.clone().unwrap_or_else(|| {
                format!(
                    "{}{}",
                    config
                        .thread_name_prefix
                        .unwrap_or("async-global-executor-"),
                    GLOBAL_EXECUTOR_NEXT_THREAD.fetch_add(1, Ordering::SeqCst),
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
            })?;
            if GLOBAL_EXECUTOR_THREADS_NUMBER.fetch_add(1, Ordering::SeqCst) >= config.max_threads {
                break;
            }
    }
    Ok(())
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
