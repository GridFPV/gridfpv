//! **Primary/alternate timer roles + failover** — the single-active-source selection (issue #112).
//!
//! Two timers at the **same gate** give redundancy: among an event's selected
//! [`timers`](gridfpv_server::events::EventMeta::timers) one is the **primary**, the rest are
//! **alternates**. All selected timers stay *connected* (hot standby — the persistent RH connection
//! from #105 connects every selected RH timer), but the per-event source bridge feeds **only the
//! active source's** passes into the event log, so the same physical crossing is never
//! double-counted when more than one timer is selected.
//!
//! # The selection rule (primary-preferred)
//!
//! At any instant exactly one selected timer is the **active source**:
//!
//! - the **primary** (its [`EventMeta::effective_primary`](gridfpv_server::events::EventMeta::effective_primary))
//!   while it is **healthy**, else
//! - the **first healthy alternate** in selection order, else
//! - `None` (no selected timer is healthy — nothing feeds).
//!
//! When the primary recovers it is preferred again (a failover is not sticky), so a primary that
//! drops mid-race and reconnects takes the feed back from the alternate.
//!
//! A timer is **healthy** when it can currently produce passes: a **Mock** is always healthy (it
//! needs nothing external), and a **RotorHazard** timer is healthy only while its persistent
//! connection reports [`TimerStatus::Connected`]. `Connecting`/`Disconnected`/`Error`/`Configured`
//! are all *not healthy* — a timer mid-connect or dropped must not be the active source.
//!
//! This module is pure (no I/O): it maps a selection + the live timer statuses to the one active
//! [`TimerId`], so it is unit-testable without spawning a bridge, and is shared by the bridge (which
//! gates each source's appends on being the active one) and the failover tests.

use gridfpv_server::events::EventMeta;
use gridfpv_server::timers::{TimerId, TimerKind, TimerRegistry, TimerStatus};

/// Whether a timer is **healthy** — able to feed passes right now (issue #112).
///
/// A Mock is always healthy. A RotorHazard timer is healthy only while its persistent connection
/// is [`TimerStatus::Connected`]; any other status (connecting, dropped, errored, resting at
/// `Configured`) is not healthy, so it cannot be the active source. An id that no longer resolves
/// (a since-deleted timer) is treated as not healthy.
pub fn timer_is_healthy(timers: &TimerRegistry, id: &TimerId) -> bool {
    let Some(timer) = timers.get(id) else {
        return false;
    };
    match timer.kind {
        // The synthetic source needs nothing external — always ready to feed.
        TimerKind::Mock { .. } => true,
        // A live RH timer feeds only while its persistent connection is up.
        TimerKind::Rotorhazard { .. } => timer.status == TimerStatus::Connected,
    }
}

/// The **active source** for an event right now (issue #112): the primary while healthy, else the
/// first healthy alternate in selection order, else `None`.
///
/// `meta` carries the selection and the effective primary; `timers` supplies each timer's live
/// status (driven by the #105 persistent-connection reconciler for RH timers). This is the single
/// rule the bridge applies every poll to decide whose passes feed the log — so a primary drop fails
/// over to an alternate and a primary recovery switches back, all without double-counting.
pub fn active_source(meta: &EventMeta, timers: &TimerRegistry) -> Option<TimerId> {
    // Prefer the primary while it is healthy (failover is not sticky — the primary wins back).
    if let Some(primary) = meta.effective_primary() {
        if timer_is_healthy(timers, &primary) {
            return Some(primary);
        }
    }
    // Otherwise the first healthy alternate, scanning the selection in order.
    meta.timers
        .iter()
        .find(|id| timer_is_healthy(timers, id))
        .cloned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use gridfpv_server::events::EventRegistry;
    use gridfpv_server::scope::EventId;
    use gridfpv_server::timers::{CreateTimerRequest, MOCK_TIMER_ID};

    /// Build a registry plus one Mock and one RH timer, select both for Practice (primary = the
    /// first, by default), and return the pieces the selection logic needs.
    fn setup() -> (EventRegistry, EventId, TimerId, TimerId) {
        let registry = EventRegistry::new(None).unwrap();
        let mock = TimerId(MOCK_TIMER_ID.to_string());
        let rh = registry
            .timers()
            .create(&CreateTimerRequest {
                name: "Field RH".into(),
                kind: TimerKind::Rotorhazard {
                    url: "http://rh.local:5000".into(),
                },
                channel_capability: None,
                node_count: None,
                available_channels: None,
            })
            .unwrap();
        let event = EventId(
            registry
                .create(&gridfpv_server::events::CreateEventRequest::named(
                    "Test Event",
                ))
                .expect("create the test event")
                .id
                .0,
        );
        (registry, event, mock, rh.id)
    }

    #[test]
    fn single_mock_is_always_the_active_source() {
        let (registry, practice, mock, _rh) = setup();
        registry.set_timers(&practice, vec![mock.clone()]).unwrap();
        let meta = registry.meta_of(&practice).unwrap();
        // One timer = primary = the only source, exactly as before #112.
        assert_eq!(active_source(&meta, &registry.timers()), Some(mock));
    }

    #[test]
    fn primary_is_active_when_healthy_even_with_a_healthy_alternate() {
        // Primary Mock + alternate Mock: the primary feeds, the alternate is hot standby (this is
        // the double-count fix — only one of two healthy timers is ever the active source).
        let (registry, practice, mock, rh) = setup();
        // Connect the RH alternate so BOTH are healthy.
        registry.timers().set_status(&rh, TimerStatus::Connected);
        // Selection [mock, rh] with the Mock primary (the default first).
        registry
            .set_timers(&practice, vec![mock.clone(), rh.clone()])
            .unwrap();
        let meta = registry.meta_of(&practice).unwrap();
        assert_eq!(active_source(&meta, &registry.timers()), Some(mock));
    }

    #[test]
    fn fails_over_to_alternate_when_primary_drops_then_back_on_recovery() {
        // Primary = RH, alternate = Mock. Healthy RH primary feeds; RH drops → Mock alternate
        // feeds; RH recovers → primary feeds again (primary-preferred, not sticky).
        let (registry, practice, mock, rh) = setup();
        registry
            .set_timers(&practice, vec![rh.clone(), mock.clone()])
            .unwrap();
        registry
            .set_primary_timer(&practice, Some(rh.clone()))
            .unwrap();
        let timers = registry.timers();

        // RH connected → primary RH is the active source.
        timers.set_status(&rh, TimerStatus::Connected);
        let meta = registry.meta_of(&practice).unwrap();
        assert_eq!(active_source(&meta, &timers), Some(rh.clone()));

        // RH drops → fail over to the Mock alternate.
        timers.set_status(&rh, TimerStatus::Disconnected);
        assert_eq!(active_source(&meta, &timers), Some(mock.clone()));

        // RH recovers → switch back to the primary.
        timers.set_status(&rh, TimerStatus::Connected);
        assert_eq!(active_source(&meta, &timers), Some(rh));
    }

    #[test]
    fn no_healthy_timer_yields_no_active_source() {
        // Only a disconnected RH selected → nothing healthy → nothing feeds.
        let (registry, practice, _mock, rh) = setup();
        registry.set_timers(&practice, vec![rh.clone()]).unwrap();
        registry.timers().set_status(&rh, TimerStatus::Disconnected);
        let meta = registry.meta_of(&practice).unwrap();
        assert_eq!(active_source(&meta, &registry.timers()), None);
    }

    #[test]
    fn a_stale_primary_falls_back_to_the_first_selected() {
        // A primary pointing at a timer no longer selected degrades to the first selected timer.
        let (registry, practice, mock, rh) = setup();
        registry
            .set_timers(&practice, vec![mock.clone(), rh.clone()])
            .unwrap();
        registry
            .set_primary_timer(&practice, Some(rh.clone()))
            .unwrap();
        // Deselect the RH (the primary) — set_timers clears the stale primary.
        registry.set_timers(&practice, vec![mock.clone()]).unwrap();
        let meta = registry.meta_of(&practice).unwrap();
        assert_eq!(meta.effective_primary(), Some(mock.clone()));
        assert_eq!(active_source(&meta, &registry.timers()), Some(mock));
    }
}
