# async-global-executor

[![API Docs](https://docs.rs/async-global-executor/badge.svg)](https://docs.rs/async-global-executor)
[![Build status](https://github.com/Keruspe/async-global-executor/workflows/Build%20and%20test/badge.svg)](https://github.com/Keruspe/async-global-executor/actions)
[![Downloads](https://img.shields.io/crates/d/async-global-executor.svg)](https://crates.io/crates/async-global-executor)
[![Dependency Status](https://deps.rs/repo/github/Keruspe/async-global-executor/status.svg)](https://deps.rs/repo/github/Keruspe/async-global-executor)
[![LICENSE](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)

A global executor built on top of async-executor and async-io

# Examples

```
# use futures_lite::future;

// spawn a task on the multi-threaded executor
let task1 = async_global_executor::spawn(async {
    1 + 2
});
// spawn a task on the local executor (same thread)
let task2 = async_global_executor::spawn_local(async {
    3 + 4
});
let task = future::zip(task1, task2);

// run the executor
async_global_executor::run(async {
    assert_eq!(task.await, (3, 7));
});
```
