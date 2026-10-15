use super::*;
use shuttle::future::{block_on, spawn, yield_now};

// Real Tokio mutex and production guard lifetime; Shuttle explores every
// scheduling of two commands across their publication awaits (no DFS cutoff).
#[test]
fn commands_publish_generation_and_cursor_without_interleaving() {
    shuttle::check_dfs(
        || {
            block_on(async {
                let state = Arc::new(AsyncMutex::new((0, Some(0))));
                let mut commands = Vec::new();
                for _ in 0..2 {
                    let state = state.clone();
                    commands.push(spawn(async move {
                        serialized(&state, async |state| {
                            assert_eq!(state.1.take(), Some(state.0));
                            yield_now().await;
                            state.0 += 1;
                            state.1 = Some(state.0);
                        })
                        .await;
                    }));
                }
                for command in commands {
                    command.await.unwrap();
                }
                assert_eq!(*state.lock().await, (2, Some(2)));
            });
        },
        None,
    );
}

#[test]
fn cancellation_releases_the_guard_and_invalidates_the_cursor() {
    shuttle::check_dfs(
        || {
            block_on(async {
                let state = Arc::new(AsyncMutex::new(Some(0)));
                let writing = state.clone();
                let command = spawn(async move {
                    serialized(&writing, async |cursor| {
                        cursor.take();
                        yield_now().await;
                        *cursor = Some(1);
                    })
                    .await;
                });
                yield_now().await;
                command.abort();
                let _ = command.await;
                // A cancelled capture leaves None, which forces the next
                // command's baseline. An unstarted/completed capture is valid.
                serialized(&state, async |cursor| {
                    assert!(matches!(*cursor, None | Some(0 | 1)));
                    *cursor = Some(2);
                })
                .await;
                assert_eq!(*state.lock().await, Some(2));
            });
        },
        None,
    );
}
