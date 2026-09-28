//! Lifecycle spec: AC9. The table is the spec. Change it first.

use autumn_plugin_pingora::{Lifecycle, LifecycleCell, LifecycleEvent};
use proptest::prelude::*;

use Lifecycle::{Draining, Failed, Idle, Serving, Stopped};
use LifecycleEvent::{Bound, Drained, ServerExited, ShutdownRequested, StartFailed};

/// Every legal transition. All other pairs are illegal.
const SPEC: &[(Lifecycle, LifecycleEvent, Lifecycle)] = &[
    (Idle, Bound, Serving),
    (Idle, StartFailed, Failed),
    (Idle, ShutdownRequested, Stopped),
    (Serving, ShutdownRequested, Draining),
    (Serving, ServerExited, Failed),
    (Draining, Drained, Stopped),
    (Draining, ServerExited, Stopped),
];

fn expected(state: Lifecycle, event: LifecycleEvent) -> Option<Lifecycle> {
    SPEC.iter()
        .find(|(from, on, _)| *from == state && *on == event)
        .map(|(_, _, to)| *to)
}

#[test]
fn every_state_event_pair_matches_the_spec() {
    for state in Lifecycle::ALL {
        for event in LifecycleEvent::ALL {
            assert_eq!(
                state.next(event),
                expected(state, event),
                "{state:?} x {event:?}"
            );
        }
    }
}

#[test]
fn terminal_states_accept_no_event() {
    for state in [Stopped, Failed] {
        assert!(state.is_terminal());
        for event in LifecycleEvent::ALL {
            assert_eq!(state.next(event), None);
        }
    }
}

#[test]
fn only_serving_is_ready() {
    for state in Lifecycle::ALL {
        assert_eq!(state.is_ready(), state == Serving, "{state:?}");
    }
}

#[test]
fn names_are_lower_case_and_unique() {
    let names: Vec<&str> = Lifecycle::ALL.iter().map(|s| s.as_str()).collect();
    assert_eq!(names, ["idle", "serving", "draining", "stopped", "failed"]);
    assert_eq!(Serving.to_string(), "serving");
}

#[test]
fn cell_applies_legal_events_and_rejects_illegal_ones() {
    let cell = LifecycleCell::new();
    assert_eq!(cell.get(), Idle);
    assert_eq!(cell.apply(Drained), Err(Idle));
    assert_eq!(cell.apply(Bound), Ok(Serving));
    assert_eq!(cell.apply(ShutdownRequested), Ok(Draining));
    assert_eq!(cell.apply(ShutdownRequested), Err(Draining));
    assert_eq!(cell.apply(Drained), Ok(Stopped));
    assert_eq!(cell.get(), Stopped);
}

fn event() -> impl Strategy<Value = LifecycleEvent> {
    proptest::sample::select(LifecycleEvent::ALL.to_vec())
}

proptest! {
    /// The cell follows `next` for any sequence, and a terminal state
    /// never changes.
    #[test]
    fn cell_follows_the_pure_function(events in proptest::collection::vec(event(), 0..32)) {
        let cell = LifecycleCell::new();
        let mut model = Idle;
        for event in events {
            let result = cell.apply(event);
            match model.next(event) {
                Some(next) => {
                    prop_assert_eq!(result, Ok(next));
                    model = next;
                }
                None => prop_assert_eq!(result, Err(model)),
            }
            prop_assert_eq!(cell.get(), model);
        }
    }

    /// Two threads that race one event: exactly one wins.
    #[test]
    fn one_winner_for_a_race(_seed in 0u8..8) {
        let cell = std::sync::Arc::new(LifecycleCell::new());
        let _ = cell.apply(Bound);
        let handles: Vec<_> = (0..2)
            .map(|_| {
                let cell = cell.clone();
                std::thread::spawn(move || cell.apply(ShutdownRequested).is_ok())
            })
            .collect();
        let wins = handles
            .into_iter()
            .filter_map(|h| h.join().ok())
            .filter(|won| *won)
            .count();
        prop_assert_eq!(wins, 1);
        prop_assert_eq!(cell.get(), Draining);
    }
}
