//! One connection per timer in the directory.
//!
//! [`spawn`] starts a task that watches the timer directory. For every timer in it there is
//! one task holding that timer's connection, of the kind the timer is. A timer that is
//! removed, or whose kind or address changes, has its connection dropped; a new one is
//! started for the new record. A change that does not move the connection (the timer's own
//! id being learned) leaves it alone.

use std::collections::BTreeMap;

use gridfpv_events::AdapterId;
use gridfpv_server::events::EventRegistry;
use gridfpv_server::timers::{Timer, TimerKind};
use tokio::task::JoinHandle;

/// Start keeping a connection to every timer in the directory. Runs until aborted.
pub fn spawn(registry: EventRegistry) -> JoinHandle<()> {
    tokio::spawn(supervise(registry))
}

async fn supervise(registry: EventRegistry) {
    let directory = registry.timers();
    let mut changes = directory.watch();
    let mut held: BTreeMap<AdapterId, (Timer, JoinHandle<()>)> = BTreeMap::new();
    loop {
        let wanted: BTreeMap<AdapterId, Timer> = directory
            .records()
            .into_iter()
            .map(|timer| (timer.id.clone(), timer))
            .collect();
        // Drop connections to timers that were removed or changed.
        held.retain(|id, (timer, task)| {
            let keep = wanted
                .get(id)
                .is_some_and(|now| now.kind == timer.kind && now.address == timer.address);
            if !keep {
                task.abort();
            }
            keep
        });
        for (id, timer) in wanted {
            held.entry(id).or_insert_with(|| {
                let registry = registry.clone();
                let task = match timer.kind {
                    TimerKind::Oft => tokio::spawn(crate::oft::hold(registry, timer.clone())),
                    TimerKind::RotorHazard => {
                        tokio::spawn(crate::rotorhazard::hold(registry, timer.clone()))
                    }
                };
                (timer, task)
            });
        }
        if changes.changed().await.is_err() {
            for (_, task) in held.into_values() {
                task.abort();
            }
            return;
        }
    }
}
