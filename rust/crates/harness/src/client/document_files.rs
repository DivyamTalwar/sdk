//! GOAL.md and plans/ are ordinary authorized snapshot-plus-follow queries.
use super::ViewUpdate;
use crate::{
    Error, Result,
    task_volume::{FileKey, Query, TaskFiles},
    wire,
};
use acyclic_stream::{BoxProviderStream, StreamProvider};
use futures::{StreamExt, future, stream};

/// Follow only document files; no task transcript or completed history is loaded.
/// Each query retains its atomic snapshot cut and the provider's push follow.
pub async fn document_files<P: StreamProvider + ?Sized>(
    files: &TaskFiles<P>,
    authority: &wire::Authority,
    maximum_plans: usize,
) -> Result<BoxProviderStream<'static, Result<ViewUpdate>>> {
    let goal_query = Query::File(FileKey::Goal);
    let goal = files.observe(goal_query.clone(), None, 1).await?;
    let plans = files.observe(Query::Plans, None, maximum_plans).await?;
    let initial = [
        ViewUpdate::DocumentSnapshot {
            authority: authority.clone(),
            query: goal_query,
            snapshot: goal
                .snapshot
                .ok_or_else(|| Error::Conflict("goal snapshot missing".into()))?,
        },
        ViewUpdate::DocumentSnapshot {
            authority: authority.clone(),
            query: Query::Plans,
            snapshot: plans
                .snapshot
                .ok_or_else(|| Error::Conflict("plans snapshot missing".into()))?,
        },
    ];
    let authority = authority.clone();
    let changes = stream::select(goal.changes, plans.changes).filter_map(move |change| {
        future::ready(match change {
            Ok(change) => change.file.map(|file| {
                Ok(ViewUpdate::DocumentFile {
                    authority: authority.clone(),
                    file,
                })
            }),
            Err(error) => Some(Err(error.into())),
        })
    });
    Ok(Box::pin(
        stream::iter(initial.into_iter().map(Ok)).chain(changes),
    ))
}
