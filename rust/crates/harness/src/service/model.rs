use crate::Result;
use std::{fmt::Display, future::Future};

pub(super) fn check<F, Fut>(run: F)
where
    F: Fn() -> Fut + Send + Sync + 'static,
    Fut: Future<Output = Result<()>> + Send + 'static,
{
    shuttle::check_dfs(
        move || {
            let result = shuttle::future::block_on(run());
            assert!(result.is_ok(), "model failed: {result:?}");
        },
        None,
    );
}

pub(super) async fn join<T, E: Display>(
    task: impl Future<Output = std::result::Result<Result<T>, E>>,
) -> Result<T> {
    task.await.map_err(super::storage)?
}
