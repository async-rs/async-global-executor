# Version 1.4.3

- switch to multi threaded tokio schedulers when enabled

# Version 1.4.2

- Drop an Arc

# Version 1.4.1

- switch back to manual implementation for tokio02 integration

# Version 1.4.0

- add tokio03 integration

# Version 1.3.0

- use async-compat for tokio02 integration

# Version 1.2.1

- tokio02 fix

# Version 1.2.0

- Add tokio02 feature

# Version 1.1.1

- Update `async-executor`.

# Version 1.1.0

- Update async-executor

# Version 1.0.2

- Do not run global tasks in `block_on()`

# Version 1.0.1

- Update dependencies

# Version 1.0.0

- Update dependencies
- Make async-io support optional

# Version 0.2.3

- Change license to MIT or Apache-2.0

# Version 0.2.2

- Reexport `async_executor::Task`

# Version 0.2.1

- Make sure we spawn at least one thread

# Version 0.2.0

- Rename `run` to `block_on` and drop `'static` requirement
- Add `GlobalExecutorConfig::with_thread_name`

# Version 0.1.4

- Add init functions

# Version 0.1.3

- `run`: do not require `Future` to be `Send`

# Version 0.1.2

- Adjust dependencies

# Versio 0.1.1

- Fix the number of spawned threads

# Version 0.1.0

- Initial release
