use super::*;

impl<P: StreamProvider> Session<P> {
    pub(super) fn refresh(&mut self, id: Option<u64>) {
        let largest = crate::MAX_RECORD_BYTES + 128;
        let readiness = (
            self.pending.len() < self.limits.pending_frames.saturating_sub(1)
                && self
                    .limits
                    .pending_bytes
                    .checked_sub(2 * largest)
                    .is_some_and(|remaining| self.pending_bytes <= remaining),
            self.pending.len() < self.limits.pending_frames
                && self.pending_bytes <= self.limits.pending_bytes.saturating_sub(largest),
            self.credit.frames > 0 && self.credit.bytes > 0,
        );
        let changed = readiness != self.readiness;
        self.readiness = readiness;
        if changed {
            for channel in self.channels.values() {
                update(channel, readiness);
            }
        } else if let Some(channel) = id.and_then(|id| self.channels.get(&id)) {
            update(channel, readiness);
        }
    }
}

fn update(channel: &Channel, readiness: (bool, bool, bool)) {
    // Latest lookahead is bounded per channel, and leaves a maximum-record
    // slot reserved for independent channels with receive credit.
    let enabled = channel.ending.is_none()
        && ((channel.latest && channel.pending < 3 && readiness.0)
            || (channel.pending == 0
                && readiness.1
                && readiness.2
                && channel.credit.frames > 0
                && channel.credit.bytes > 0));
    channel.enabled.send_if_modified(|value| {
        if *value == enabled {
            false
        } else {
            *value = enabled;
            true
        }
    });
}

pub(super) async fn next_record(
    records: &mut crate::RecordStream,
    enabled: &mut watch::Receiver<bool>,
) -> Result<Option<crate::Record>, StreamError> {
    loop {
        let ready = *enabled.borrow_and_update();
        if !ready {
            enabled
                .changed()
                .await
                .map_err(|_| StreamError::Unavailable)?;
            continue;
        }
        // Check gate changes before polling the source. Disabling an idle
        // source cancels its wait without reading or parking an uncharged record.
        let changed = enabled.changed();
        futures::pin_mut!(changed);
        match future::select(changed, records.next()).await {
            future::Either::Left((changed, _)) => changed.map_err(|_| StreamError::Unavailable)?,
            future::Either::Right((record, _)) => return record.transpose(),
        }
    }
}
