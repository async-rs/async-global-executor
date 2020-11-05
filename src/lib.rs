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
    fmt,
    future::Future,
    io,
    sync::atomic::{AtomicUsize, Ordering},
    thread,
};

pub use async_executor::Task;

// Global state

static GLOBAL_EXECUTOR_CONFIG: OnceCell<Config> = OnceCell::new();
static GLOBAL_EXECUTOR_THREADS: Lazy<()> = Lazy::new(init);

// The current number of threads (some might be shutting down and not in the pool anymore)
static GLOBAL_EXECUTOR_THREADS_NUMBER: Mutex<usize> = Mutex::new(0);
// The expected number of threads (excluding the one that are shutting down)
static GLOBAL_EXECUTOR_EXPECTED_THREADS_NUMBER: Mutex<usize> = Mutex::new(0);

static GLOBAL_EXECUTOR: Executor<'_> = Executor::new();

thread_local! {
    static LOCAL_EXECUTOR: LocalExecutor<'static> = LocalExecutor::new();
    // Used to shutdown a thread when we receive a message from the Sender.
    // We send an ack using to the Receiver once we're finished shutting down.
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
#[derive(Default)]
pub struct GlobalExecutorConfig {
    /// The environment variable from which we'll try to parse the number of threads to spawn.
    env_var: Option<&'static str>,
    /// The minimum number of threads to spawn.
    min_threads: Option<usize>,
    /// The maximum number of threads to spawn.
    max_threads: Option<usize>,
    /// The name to us fo the threads.
    thread_name_fn: Option<Box<dyn Fn() -> String + Send + Sync>>,
}

impl fmt::Debug for GlobalExecutorConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("GlobalExecutorConfig")
            .field("env_var", &self.env_var)
            .field("min_threads", &self.min_threads)
            .field("max_threads", &self.max_threads)
            .finish()
    }
}

impl GlobalExecutorConfig {
    /// Use the specified environment variable to find the number of threads to spawn.
    pub fn with_env_var(mut self, env_var: &'static str) -> Self {
        self.env_var = Some(env_var);
        self
    }

    /// Use the specified value as the minimum number of threads.
    pub fn with_min_threads(mut self, min_threads: usize) -> Self {
        self.min_threads = Some(min_threads);
        self
    }

    /// Use the specified value as the maximum number of threads.
    pub fn with_max_threads(mut self, max_threads: usize) -> Self {
        self.max_threads = Some(max_threads);
        self
    }

    /// Use the specified prefix to name the threads.
    pub fn with_thread_name_fn(
        mut self,
        thread_name_fn: impl Fn() -> String + Send + Sync + 'static,
    ) -> Self {
        self.thread_name_fn = Some(Box::new(thread_name_fn));
        self
    }

    fn seal(self) -> Config {
        let min_threads = std::env::var(self.env_var.unwrap_or("ASYNC_GLOBAL_EXECUTOR_THREADS"))
            .ok()
            .and_then(|threads| threads.parse().ok())
            .or(self.min_threads)
            .unwrap_or_else(num_cpus::get)
            .max(1);
        let max_threads = self.max_threads.unwrap_or(min_threads * 4).max(min_threads);
        Config {
            min_threads,
            max_threads,
            thread_name_fn: self.thread_name_fn.unwrap_or_else(|| {
                Box::new(|| {
                    static GLOBAL_EXECUTOR_NEXT_THREAD: AtomicUsize = AtomicUsize::new(1);
                    format!(
                        "async-global-executor-{}",
                        GLOBAL_EXECUTOR_NEXT_THREAD.fetch_add(1, Ordering::SeqCst)
                    )
                })
            }),
        }
    }
}

// The actual configuration, computed from the given GlobalExecutorConfig
struct Config {
    min_threads: usize,
    max_threads: usize,
    thread_name_fn: Box<dyn Fn() -> String + Send + Sync>,
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
///         .with_min_threads(4)
///         .with_max_threads(6)
///         .with_thread_name_fn(Box::new(|| "worker".to_string()))
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
/// Returns how many threads we spawned.
///
/// # Examples
///
/// ```
/// async_global_executor::spawn_more_threads(2);
/// ```
pub async fn spawn_more_threads(count: usize) -> io::Result<usize> {
    // Get the current configuration, or initialize the thread pool.
    let config = GLOBAL_EXECUTOR_CONFIG.get().unwrap_or_else(|| {
        Lazy::force(&GLOBAL_EXECUTOR_THREADS);
        GLOBAL_EXECUTOR_CONFIG.get().unwrap()
    });
    // How many threads do we have (including shutting down)
    let mut threads_number = GLOBAL_EXECUTOR_THREADS_NUMBER.lock().await;
    // How many threads are we supposed to have (when all shutdowns are complete)
    let mut expected_threads_number = GLOBAL_EXECUTOR_EXPECTED_THREADS_NUMBER.lock().await;
    // Ensure we don't exceed configured max threads (including shutting down)
    let count = count.min(config.max_threads - *threads_number);
    for _ in 0..count {
        thread::Builder::new()
            .name((config.thread_name_fn)())
            .spawn(thread_main_loop)?;
        *threads_number += 1;
        *expected_threads_number += 1;
    }
    Ok(count)
}

/// Stop one of the executor threads, down to configured min value
///
/// Returns whether a thread has been stopped.
///
/// # Examples
///
/// ```
/// async_global_executor::stop_thread();
/// ```
pub fn stop_thread() -> Task<bool> {
    spawn(stop_current_executor_thread())
}

/// Stop the current executor thread, if we exceed the configured min value
///
/// Returns whether the thread has been stopped.
///
/// # Examples
///
/// ```
/// async_global_executor::stop_current_thread();
/// ```
pub fn stop_current_thread() -> Task<bool> {
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
        let run = || crate::tokio02::enter(run);
        #[cfg(feature = "tokio03")]
        let _tokio03_enter = crate::tokio03::enter();
        run()
    }
}

fn thread_main_loop() {
    // This will be used to ask for shutdown.
    let (s, r) = async_channel::bounded(1);
    // This wil be used to ack once shutdown is complete.
    let (s_ack, r_ack) = async_channel::bounded(1);
    THREAD_SHUTDOWN.with(|thread_shutdown| drop(thread_shutdown.set((s, r_ack))));

    loop {
        let _ = std::panic::catch_unwind(|| {
            LOCAL_EXECUTOR.with(|executor| {
                let shutdown = async {
                    // Wait until we're asked to shutdown.
                    let _ = r.recv().await;
                    // FIXME: wait for spawned tasks completion
                    // Ack that we're done shutting down.
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

async fn stop_current_executor_thread() -> bool {
    // How many threads are we supposed to have (when all shutdowns are complete)
    let mut expected_threads_number = GLOBAL_EXECUTOR_EXPECTED_THREADS_NUMBER.lock().await;
    // Ensure we don't go below the configured min_threads (ignoring shutting down)
    if *expected_threads_number > GLOBAL_EXECUTOR_CONFIG.get().unwrap().min_threads {
        let (s, r_ack) =
            THREAD_SHUTDOWN.with(|thread_shutdown| thread_shutdown.get().unwrap().clone());
        let _ = s.send(()).await;
        // We now expect to have one less thread (this one is shutting down)
        *expected_threads_number -= 1;
        // Unlock the Mutex
        drop(expected_threads_number);
        let _ = r_ack.recv().await;
        // This thread is done shutting down
        *GLOBAL_EXECUTOR_THREADS_NUMBER.lock().await -= 1;
        true
    } else {
        false
    }
}

// Tokio integration

#[cfg(feature = "tokio02")]
mod tokio02;
#[cfg(feature = "tokio03")]
mod tokio03;
