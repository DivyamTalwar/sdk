use super::*;
use crate::service::model;
use std::sync::Arc;

#[test]
fn start_stop_and_follow_exhaust_all_async_interleavings() {
    model::check(|| async {
        let lifetime = Arc::new(Lifetime::new(tempfile::tempfile().map_err(storage)?));
        let serving = lifetime.clone();
        let starter = shuttle::future::spawn(async move {
            if let Ok(mut phase) = serving.begin() {
                let _stop = serving.stop_on_drop();
                phase
                    .wait_for(|state| state.phase == Phase::Stopping)
                    .await
                    .map_err(storage)?;
                serving.release();
            }
            Ok(())
        });
        let stopping = lifetime.clone();
        let stop = shuttle::future::spawn(async move {
            if stopping.request_stop() {
                stopping.release();
            }
            stopping.stopped().await?;
            assert!(stopping.state.borrow().instance.is_none());
            Ok(())
        });
        let following = lifetime.clone();
        let follower = shuttle::future::spawn(async move {
            following.stopped().await?;
            assert!(following.state.borrow().instance.is_none());
            assert!(following.begin().is_err());
            Ok(())
        });
        model::join(starter).await?;
        model::join(stop).await?;
        model::join(follower).await?;
        assert!(lifetime.begin().is_err());
        Ok(())
    });
}

#[test]
fn cancelled_serving_notifies_shutdown_followers() {
    model::check(|| async {
        let lifetime = Arc::new(Lifetime::new(tempfile::tempfile().map_err(storage)?));
        let mut phase = lifetime.begin()?;
        let serving = lifetime.clone();
        let drain = shuttle::future::spawn(async move {
            phase
                .wait_for(|state| state.phase == Phase::Stopping)
                .await
                .map_err(storage)?;
            serving.release();
            Ok(())
        });
        let stopping = lifetime.clone();
        let cancellation = shuttle::future::spawn(async move {
            drop(stopping.stop_on_drop());
            Ok(())
        });
        let following = lifetime.clone();
        let follow = shuttle::future::spawn(async move {
            following.stopped().await?;
            assert!(following.state.borrow().instance.is_none());
            Ok(())
        });
        model::join(cancellation).await?;
        model::join(drain).await?;
        model::join(follow).await?;
        Ok(())
    });
}
