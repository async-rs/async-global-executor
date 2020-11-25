use std::{
    fmt,
    future::Future,
    ops::{Deref, DerefMut},
    pin::Pin,
    task::{Context, Poll},
};

/// A spawned task.
///
/// A [`Task`] can be awaited to retrieve the output of its future.
///
/// Dropping a [`Task`] cancels it, which means its future won't be polled again. To drop the
/// [`Task`] handle without canceling it, use [`detach()`][`Task::detach()`] instead. To cancel a
/// task gracefully and wait until it is fully destroyed, use the [`cancel()`][Task::cancel()]
/// method.
///
/// Note that canceling a task actually wakes it and reschedules one last time. Then, the executor
/// can destroy the task by simply dropping its [`Runnable`][`super::Runnable`] or by invoking
/// [`run()`][`super::Runnable::run()`].
///
/// # Examples
///
/// ```
/// // Spawn a future onto the executor.
/// let task = async_global_executor::spawn(async {
///     println!("Hello from a task!");
///     1 + 2
/// });
///
/// // Wait for the task's output.
/// assert_eq!(async_global_executor::block_on(task), 3);
/// ```
#[must_use = "tasks get canceled when dropped, use `.detach()` to run them in the background"]
pub struct Task<T>(async_executor::Task<T>);

impl<T> Task<T> {
    /// Detaches the task to let it keep running in the background.
    ///
    /// # Examples
    ///
    /// ```
    /// use async_io::Timer;
    /// use std::time::Duration;
    ///
    /// // Spawn a deamon future.
    /// async_global_executor::spawn(async {
    ///     loop {
    ///         println!("I'm a daemon task looping forever.");
    ///         Timer::after(Duration::from_secs(1)).await;
    ///     }
    /// })
    /// .detach();
    pub fn detach(self) {
        self.0.detach();
    }

    /// Cancels the task and waits for it to stop running.
    ///
    /// Returns the task's output if it was completed just before it got canceled, or [`None`] if
    /// it didn't complete.
    ///
    /// While it's possible to simply drop the [`Task`] to cancel it, this is a cleaner way of
    /// canceling because it also waits for the task to stop running.
    ///
    /// # Examples
    ///
    /// ```
    /// use async_io::Timer;
    /// use std::time::Duration;
    ///
    /// // Spawn a deamon future.
    /// let task = async_global_executor::spawn(async {
    ///     loop {
    ///         println!("Even though I'm in an infinite loop, you can still cancel me!");
    ///         Timer::after(Duration::from_secs(1)).await;
    ///     }
    /// });
    ///
    /// async_global_executor::block_on(async {
    ///     Timer::after(Duration::from_secs(3)).await;
    ///     task.cancel().await;
    /// });
    /// ```
    pub async fn cancel(self) -> Option<T> {
        self.0.cancel().await
    }
}

impl<T> Unpin for Task<T> {}

impl<T> From<async_executor::Task<T>> for Task<T> {
    fn from(task: async_executor::Task<T>) -> Self {
        Self(task)
    }
}

impl<T> Deref for Task<T> {
    type Target = async_executor::Task<T>;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl<T> DerefMut for Task<T> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}

impl<T> Future for Task<T> {
    type Output = T;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        Pin::new(&mut self.0).poll(cx)
    }
}

impl<T> fmt::Debug for Task<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}
