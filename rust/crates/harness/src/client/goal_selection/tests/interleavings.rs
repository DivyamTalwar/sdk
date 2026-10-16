use super::*;
use crate::{
    client::{GoalFileView, GoalPanel},
    task_volume::Change,
};
use acyclic_harness_tui::ratatui::{buffer::Buffer, layout::Rect, widgets::Widget};

fn publication(source: &Source, query: Query, position: u64) -> Change {
    let snapshot = source.snapshot(query);
    let mut cursor = snapshot.cursor;
    cursor.position = position;
    cursor.accepted = position;
    cursor.durable = position;
    cursor.commit = [position as u8; 32];
    Change {
        cursor,
        file: snapshot.files.into_iter().next(),
        files: vec![],
        removed: vec![],
    }
}

fn copy(change: &Change) -> Change {
    Change {
        cursor: change.cursor.clone(),
        file: change.file.clone(),
        files: change.files.clone(),
        removed: change.removed.clone(),
    }
}

fn rows(view: crate::client::GoalQueryView<'_>) -> Vec<String> {
    let area = Rect::new(0, 0, 100, 5);
    let mut buffer = Buffer::empty(area);
    GoalPanel { view }.render(area, &mut buffer);
    (1..3)
        .map(|y| {
            (1..99)
                .map(|x| buffer[(x, y)].symbol())
                .collect::<String>()
                .trim_end()
                .to_owned()
        })
        .collect()
}

// Check every prefix of every merge preserving each volume's cursor order.
fn check_interleavings(views: ClientViews, goal: &[Change], head: &[Change], cap: u32) -> usize {
    let (identity, _) = views
        .queries
        .iter()
        .find(|(_, view)| view.query() == &Query::File(FileKey::Goal))
        .unwrap();
    let view = views.goal_view(identity.0, identity.1).unwrap();
    let blocked =
        matches!(view.goal, GoalFileView::Present { goal } if goal.status == GoalStatus::Blocked);
    let rendered = rows(view.clone());
    let retained = views
        .queries
        .values()
        .find(|view| view.query() == &Query::File(FileKey::RequestHead))
        .unwrap()
        .progress()
        .unwrap();
    let mut stale_witnesses = 0;
    if blocked {
        assert_eq!(rendered[0], "Status: blocked | Continuations: pending");
        assert_eq!(
            rendered[1],
            format!("Reason: Maximum goal continuations reached (limit {cap}).")
        );
        assert!(serde_json::to_value(&view).unwrap()["progress"].is_null());
        // Negative control: remove the production gate, joining the independently
        // accepted request cut directly. The same rendering invariant must fail.
        let mut ungated = view;
        ungated.progress = Some(retained);
        let observed = ungated.progress.unwrap().continuations;
        assert_ne!(
            rows(ungated)[0],
            rendered[0],
            "ungated negative control escaped the invariant"
        );
        stale_witnesses += usize::from(observed < cap);
    } else {
        assert_eq!(
            rendered[0],
            format!("Status: active | Continuations: {}", retained.continuations)
        );
    }
    for (updates, other, is_goal) in [(goal, head, true), (head, goal, false)] {
        if let Some((first, rest)) = updates.split_first() {
            let mut next = views.clone();
            next.query_change(copy(first)).unwrap();
            stale_witnesses += if is_goal {
                check_interleavings(next, rest, other, cap)
            } else {
                check_interleavings(next, other, rest, cap)
            };
        }
    }
    stale_witnesses
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(32))]
    #[test]
    fn every_rendered_cut_keeps_unlinked_blocked_counts_pending(
        cap in prop_oneof![Just(2u32), 1u32..=u32::MAX], fraction in any::<u32>(),
    ) {
        futures::executor::block_on(async {
            let mut views = ClientViews::default();
            let _follow = views.select_goal_view(&Source::goal("objective"), &Source::head(0), 4096).await.unwrap();
            let goal = [
                publication(&Source::goal("edited objective"), Query::File(FileKey::Goal), 2),
                publication(&Source::goal(&format!("---\nstatus: blocked\nreason: Maximum goal continuations reached (limit {cap}).\n---\nobjective")), Query::File(FileKey::Goal), 3),
            ];
            let head = [
                publication(&Source::head(fraction % cap), Query::File(FileKey::RequestHead), 2),
                publication(&Source::head(cap), Query::File(FileKey::RequestHead), 3),
            ];
            assert!(check_interleavings(views, &goal, &head, cap) > 0,
                "must exercise a blocked publication while the head follow is stalled below cap");
        });
    }
}
