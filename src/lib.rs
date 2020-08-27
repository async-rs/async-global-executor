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
//! async_global_executor::run(async {
//!     assert_eq!(task.await, (3, 7));
//! });
//! ```

#![forbid(unsafe_code)]
#![warn(missing_docs, missing_debug_implementations, rust_2018_idioms)]

#[cfg(doctest)]
doc_comment::doctest!("../README.md");

use async_executor::{Executor, LocalExecutor, Task};
use once_cell::sync::Lazy;
use futures_lite::future;
use std::{cell::RefCell, future::Future, thread};

static GLOBAL_EXECUTOR: Lazy<Executor> = Lazy::new(|| {
    let num_cpus = std::env::var("ASYNC_GLOBAL_EXECUTOR_THREADS")
        .ok()
        .and_then(|threads| threads.parse().ok())
        .unwrap_or_else(num_cpus::get)
        .max(1);
    for n in 1..=num_cpus {
        thread::Builder::new()
            .name(format!("async-global-executor-{}", n))
            .spawn(|| run(future::pending::<()>()))
            .expect("cannot spawn executor thread");
    }
    Executor::new()
});

thread_local! {
    static LOCAL_EXECUTOR: RefCell<LocalExecutor> = RefCell::new(LocalExecutor::new());
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
/// async_global_executor::run(async {
///     assert_eq!(task.await, 3);
/// });
/// ```
pub fn run<F: Future<Output = T> + 'static, T: 'static>(future: F) -> T {
    LOCAL_EXECUTOR.with(|executor| {
        let executor = executor.borrow();
        let local = executor.spawn(future);
        let global = GLOBAL_EXECUTOR.run(local);
        async_io::block_on(executor.run(global))
    })
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
/// async_global_executor::run(async {
///     assert_eq!(task.await, (3, 7));
/// });
/// ```
pub fn spawn<F: Future<Output = T> + Send + 'static, T: Send + 'static>(future: F) -> Task<T> {
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
/// async_global_executor::run(async {
///     assert_eq!(task.await, (3, 7));
/// });
/// ```
pub fn spawn_local<F: Future<Output = T> + 'static, T: 'static>(future: F) -> Task<T> {
    Lazy::force(&GLOBAL_EXECUTOR);
    LOCAL_EXECUTOR.with(|executor| executor.borrow().spawn(future))
}
