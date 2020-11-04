use once_cell::sync::Lazy;
use tokio03_crate as tokio;

pub(crate) fn enter() -> tokio::runtime::EnterGuard<'static> {
    RUNTIME.enter()
}

static RUNTIME: Lazy<tokio::runtime::Runtime> = Lazy::new(|| {
    std::thread::Builder::new()
        .name("async-global-executor/tokio03".to_string())
        .spawn(move || {
            RUNTIME.block_on(futures_lite::future::pending::<()>());
        })
        .expect("failed to spawn tokio03 driver thread");
    tokio::runtime::Runtime::new().expect("failed to build tokio03 runtime")
});

#[cfg(test)]
mod test {
    use super::*;

    async fn compute() -> u8 {
        tokio::spawn(async { 1 + 2 }).await.unwrap()
    }

    #[test]
    fn spawn_tokio() {
        crate::block_on(async {
            assert_eq!(
                crate::spawn(compute()).await
                    + crate::spawn_local(compute()).await
                    + tokio::spawn(compute()).await.unwrap(),
                9
            );
        });
    }
}
