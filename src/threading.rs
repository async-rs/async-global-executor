use async_channel::{Receiver, Sender};
use async_executor::Task;
use async_mutex::Mutex;
use futures_lite::future;
use once_cell::sync::OnceCell;
use std::{io, thread};

// The current number of threads (some might be shutting down and not in the pool anymore)
static GLOBAL_EXECUTOR_THREADS_NUMBER: Mutex<usize> = Mutex::new(0);
// The expected number of threads (excluding the one that are shutting down)
static GLOBAL_EXECUTOR_EXPECTED_THREADS_NUMBER: Mutex<usize> = Mutex::new(0);

thread_local! {
    // Used to shutdown a thread when we receive a message from the Sender.
    // We send an ack using to the Receiver once we're finished shutting down.
    static THREAD_SHUTDOWN: OnceCell<(Sender<()>, Receiver<()>)> = OnceCell::new();
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
    let config = crate::config::GLOBAL_EXECUTOR_CONFIG
        .get()
        .unwrap_or_else(|| {
            crate::init();
            crate::config::GLOBAL_EXECUTOR_CONFIG.get().unwrap()
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
    crate::spawn(stop_current_executor_thread())
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
    crate::spawn_local(stop_current_executor_thread())
}

fn thread_main_loop() {
    // This will be used to ask for shutdown.
    let (s, r) = async_channel::bounded(1);
    // This wil be used to ack once shutdown is complete.
    let (s_ack, r_ack) = async_channel::bounded(1);
    THREAD_SHUTDOWN.with(|thread_shutdown| drop(thread_shutdown.set((s, r_ack))));

    loop {
        let _ = std::panic::catch_unwind(|| {
            crate::executor::LOCAL_EXECUTOR.with(|executor| {
                let shutdown = async {
                    // Wait until we're asked to shutdown.
                    let _ = r.recv().await;
                    // Wait for spawned tasks completion
                    while !executor.is_empty() {
                        executor.tick().await;
                    }
                    // Ack that we're done shutting down.
                    let _ = s_ack.send(()).await;
                };
                let local = executor.run(shutdown);
                let global = crate::executor::GLOBAL_EXECUTOR.run(future::pending::<()>());
                crate::reactor::block_on(future::or(local, global));
            })
        });
    }
}

async fn stop_current_executor_thread() -> bool {
    // How many threads are we supposed to have (when all shutdowns are complete)
    let mut expected_threads_number = GLOBAL_EXECUTOR_EXPECTED_THREADS_NUMBER.lock().await;
    // Ensure we don't go below the configured min_threads (ignoring shutting down)
    if *expected_threads_number
        > crate::config::GLOBAL_EXECUTOR_CONFIG
            .get()
            .unwrap()
            .min_threads
    {
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
