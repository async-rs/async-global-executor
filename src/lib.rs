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

use async_channel::{Receiver, Sender};
use async_executor::{Executor, LocalExecutor};
use async_mutex::Mutex;
use futures_lite::future;
use once_cell::sync::{Lazy, OnceCell};
use std::{
    future::Future,
    io,
    sync::atomic::{AtomicUsize, Ordering},
    thread,
};

pub use async_executor::Task;

// Global state

static GLOBAL_EXECUTOR_CONFIG: OnceCell<Config> = OnceCell::new();
static GLOBAL_EXECUTOR_THREADS: Lazy<()> = Lazy::new(init);

static GLOBAL_EXECUTOR_THREADS_NUMBER: Mutex<usize> = Mutex::new(0);
static GLOBAL_EXECUTOR_NEXT_THREAD: AtomicUsize = AtomicUsize::new(1);

static GLOBAL_EXECUTOR: Executor<'_> = Executor::new();

thread_local! {
    static LOCAL_EXECUTOR: LocalExecutor<'static> = LocalExecutor::new();
    static THREAD_SHUTDOWN: OnceCell<(Sender<()>, Receiver<()>)> = OnceCell::new();
}

// Executor methods

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

// Configuration

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

// Initialization

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
    reactor::block_on(async {
        if current_threads_number().await == 0 {
            spawn_more_threads(config.min_threads)
                .await
                .expect("cannot spawn executor threads");
        }
    });
}

/// Spawn more executor threads, up to configured max value.
///
/// # Examples
///
/// ```
/// async_global_executor::spawn_more_threads(2);
/// ```
pub async fn spawn_more_threads(count: usize) -> io::Result<()> {
    let config = GLOBAL_EXECUTOR_CONFIG.get().unwrap_or_else(|| {
        Lazy::force(&GLOBAL_EXECUTOR_THREADS);
        GLOBAL_EXECUTOR_CONFIG.get().unwrap()
    });
    let mut threads_number = GLOBAL_EXECUTOR_THREADS_NUMBER.lock().await;
    let count = count.min(config.max_threads - *threads_number);
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
            .spawn(thread_main_loop)?;
        *threads_number += 1;
    }
    Ok(())
}

/// Stop one of the executor threads, down to configured min value
///
/// # Examples
///
/// ```
/// async_global_executor::stop_thread();
/// ```
pub fn stop_thread() -> Task<()> {
    spawn(stop_current_executor_thread())
}

/// Stop the current executor thread, if we exceed the configured min value
///
/// # Examples
///
/// ```
/// async_global_executor::stop_current_thread();
/// ```
pub fn stop_current_thread() -> Task<()> {
    spawn_local(stop_current_executor_thread())
}

// Internals

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

fn thread_main_loop() {
    let (s, r) = async_channel::bounded(1);
    let (s_ack, r_ack) = async_channel::bounded(1);
    THREAD_SHUTDOWN.with(|thread_shutdown| drop(thread_shutdown.set((s, r_ack))));

    loop {
        let _ = std::panic::catch_unwind(|| {
            LOCAL_EXECUTOR.with(|executor| {
                let shutdown = async {
                    let _ = r.recv().await;
                    let _ = s_ack.send(()).await;
                };
                let local = executor.run(shutdown);
                let global = GLOBAL_EXECUTOR.run(future::pending::<()>());
                reactor::block_on(future::or(local, global));
            })
        });
    }
}

async fn current_threads_number() -> usize {
    *GLOBAL_EXECUTOR_THREADS_NUMBER.lock().await
}

async fn stop_current_executor_thread() {
    let mut threads_number = GLOBAL_EXECUTOR_THREADS_NUMBER.lock().await;
    if *threads_number > GLOBAL_EXECUTOR_CONFIG.get().unwrap().min_threads {
        let (s, r_ack) =
            THREAD_SHUTDOWN.with(|thread_shutdown| thread_shutdown.get().unwrap().clone());
        let _ = s.send(()).await;
        let _ = r_ack.recv().await;
        *threads_number -= 1;
    }
}

// Tokio integration

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
