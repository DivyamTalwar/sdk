struct PathState {
    history: Option<Arc<History>>,
    tail: u64,
    changed: watch::Sender<u64>,
    children: ChildIndex,
    next_sibling: Option<StreamPath>,
    _allocation: Allocation,
}
