//! Real memory streams with replayable dispatch/accept/ack suspension points.
use super::schedule::Schedule;
use acyclic_stream::*;
use std::sync::Arc;

pub(super) struct Stream {
    pub memory: MemoryStream,
    pub schedule: Arc<Schedule>,
}

impl Stream {
    fn describe_commit(&self, request: &CommitRequest) {
        self.schedule.describe(|| {
            let paths: Vec<_> = request
                .mutations
                .iter()
                .map(|mutation| match mutation {
                    CommitMutation::Append { path, .. } => path.as_str(),
                    CommitMutation::Fork { destination, .. } => destination.as_str(),
                })
                .collect();
            format!("commit {:?} to {paths:?}", request.idempotency_key)
        });
    }
}

#[async_trait::async_trait]
impl StreamProvider for Stream {
    async fn inspect_idempotency(
        &self,
        key: IdempotencyKey,
    ) -> Result<Option<IdempotencyObservation>, StreamError> {
        self.schedule.observe("inspect_idempotency:dispatch");
        let active = self.schedule.dispatch("inspect_idempotency");
        let result = self.memory.inspect_idempotency(key).await;
        drop(active);
        self.schedule.observe("inspect_idempotency:accepted");
        result
    }
    async fn tail(&self, path: StreamPath) -> Result<u64, StreamError> {
        self.schedule.observe("tail:dispatch");
        let active = self.schedule.dispatch("tail");
        let result = self.memory.tail(path).await;
        drop(active);
        self.schedule.observe("tail:accepted");
        result
    }
    async fn bounds(&self, path: StreamPath) -> Result<StreamBounds, StreamError> {
        self.schedule.observe("bounds:dispatch");
        let active = self.schedule.dispatch("bounds");
        let result = self.memory.bounds(path).await;
        drop(active);
        self.schedule.observe("bounds:accepted");
        result
    }
    async fn watermarks(&self, path: StreamPath) -> Result<StreamWatermarks, StreamError> {
        self.schedule.observe("watermarks:dispatch");
        let active = self.schedule.dispatch("watermarks");
        let result = self.memory.watermarks(path).await;
        drop(active);
        self.schedule.observe("watermarks:accepted");
        result
    }
    async fn append(&self, request: AppendRequest) -> Result<AppendOutcome, StreamError> {
        self.schedule
            .describe(|| format!("append {} {:?}", request.path, request.idempotency_key));
        self.schedule.point("append:dispatch").await;
        let active = self.schedule.dispatch("append");
        let result = self.memory.append(request).await;
        drop(active);
        self.schedule.point("append:accepted").await;
        result
    }
    async fn fork(&self, request: ForkRequest) -> Result<ForkReceipt, StreamError> {
        self.schedule.describe(|| {
            format!(
                "fork {} to {} {:?}",
                request.source, request.destination, request.idempotency_key
            )
        });
        self.schedule.point("fork:dispatch").await;
        let active = self.schedule.dispatch("fork");
        let result = self.memory.fork(request).await;
        drop(active);
        self.schedule.point("fork:accepted").await;
        result
    }
    async fn read(&self, request: ReadRequest) -> Result<RecordStream, StreamError> {
        self.schedule.observe("read:dispatch");
        let active = self.schedule.dispatch("read");
        let result = self.memory.read(request).await;
        drop(active);
        self.schedule.observe("read:accepted");
        result
    }
    async fn follow(&self, path: StreamPath, from: u64) -> Result<RecordStream, StreamError> {
        self.schedule.observe("follow:dispatch");
        let active = self.schedule.dispatch("follow");
        let result = self.memory.follow(path, from).await;
        drop(active);
        self.schedule.observe("follow:accepted");
        result
    }
    async fn follow_checked(
        &self,
        path: StreamPath,
        from: u64,
        preceding: CommitId,
    ) -> Result<RecordStream, StreamError> {
        self.schedule.observe("follow_checked:dispatch");
        let active = self.schedule.dispatch("follow_checked");
        let result = self.memory.follow_checked(path, from, preceding).await;
        drop(active);
        self.schedule.observe("follow_checked:accepted");
        result
    }
    async fn children(&self, request: ChildrenRequest) -> Result<ChildStream, StreamError> {
        self.schedule.observe("children:dispatch");
        let active = self.schedule.dispatch("children");
        let result = self.memory.children(request).await;
        drop(active);
        self.schedule.observe("children:accepted");
        result
    }
    async fn children_page(
        &self,
        request: ChildrenPageRequest,
    ) -> Result<ChildrenPage, StreamError> {
        self.schedule.observe("children_page:dispatch");
        let active = self.schedule.dispatch("children_page");
        let result = self.memory.children_page(request).await;
        drop(active);
        self.schedule.observe("children_page:accepted");
        result
    }
    async fn commit(&self, request: CommitRequest) -> Result<CommitOutcome, StreamError> {
        self.describe_commit(&request);
        self.schedule.point("commit:dispatch").await;
        let active = self.schedule.dispatch("commit");
        let result = self.memory.commit(request).await;
        drop(active);
        self.schedule.point("commit:accepted").await;
        result
    }
    async fn commit_before(
        &self,
        request: CommitRequest,
        deadline: u64,
    ) -> Result<CommitOutcome, StreamError> {
        self.describe_commit(&request);
        self.schedule.point("commit_before:dispatch").await;
        let active = self.schedule.dispatch("commit_before");
        let result = self.memory.commit_before(request, deadline).await;
        drop(active);
        self.schedule.point("commit_before:accepted").await;
        result
    }
    async fn read_commit(&self, id: CommitId) -> Result<CommittedEnvelope, StreamError> {
        self.schedule.observe("read_commit:dispatch");
        let active = self.schedule.dispatch("read_commit");
        let result = self.memory.read_commit(id).await;
        drop(active);
        self.schedule.observe("read_commit:accepted");
        result
    }
}
