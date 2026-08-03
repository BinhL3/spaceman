use crate::{
    Model, NativeObservation, Reconciliation, ReconciliationEvidence, Settings, reconcile,
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CycleTicket {
    pub cycle: u64,
    pub revision: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct StaleCycle {
    pub ticket: CycleTicket,
    pub current_revision: u64,
}

#[derive(Clone, Debug, Default)]
pub struct Engine {
    model: Model,
    evidence: ReconciliationEvidence,
    revision: u64,
    next_cycle: u64,
    pending: Option<CycleTicket>,
}

impl Engine {
    pub fn invalidate(&mut self) -> u64 {
        self.revision = self.revision.saturating_add(1);
        self.revision
    }

    pub fn begin_cycle(&mut self) -> CycleTicket {
        self.next_cycle = self.next_cycle.saturating_add(1);
        let ticket = CycleTicket {
            cycle: self.next_cycle,
            revision: self.revision,
        };
        self.pending = Some(ticket);
        ticket
    }

    pub fn accept(
        &mut self,
        ticket: CycleTicket,
        observation: &NativeObservation,
        settings: &Settings,
    ) -> Result<Reconciliation, StaleCycle> {
        if self.pending != Some(ticket)
            || ticket.revision != self.revision
            || observation.cycle != ticket.cycle
        {
            return Err(StaleCycle {
                ticket,
                current_revision: self.revision,
            });
        }
        self.pending = None;

        let result = reconcile(&self.model, observation, &self.evidence, settings);
        self.evidence.previous_placements = result
            .placements
            .iter()
            .map(|placement| (placement.slot, placement.frame))
            .collect();
        self.model = result.model.clone();
        Ok(result)
    }

    #[must_use]
    pub const fn model(&self) -> &Model {
        &self.model
    }

    #[must_use]
    pub const fn revision(&self) -> u64 {
        self.revision
    }
}

#[cfg(test)]
mod tests {
    use crate::{
        AppInstanceId, AppObservation, NativeObservation, ObservedApplication, ObservedWindow,
        PhysicalWindowId, Rect,
    };

    use super::*;

    fn observation(cycle: u64, window_id: u32) -> NativeObservation {
        let app = AppInstanceId {
            pid: 10,
            launched_at: 1,
        };
        let window = PhysicalWindowId {
            app: app.clone(),
            window_id,
        };
        NativeObservation {
            cycle,
            mouse_down: false,
            applications: vec![AppObservation::Complete {
                app: ObservedApplication {
                    id: app,
                    bundle_id: "com.example.Editor".into(),
                },
                focused: Some(window.clone()),
                windows: vec![ObservedWindow {
                    id: window,
                    frame: Rect {
                        width: 100,
                        height: 100,
                        ..Rect::default()
                    },
                }],
            }],
        }
    }

    fn settings() -> Settings {
        Settings {
            output: Rect {
                width: 100,
                height: 100,
                ..Rect::default()
            },
            frame_tolerance: 2,
        }
    }

    #[test]
    fn accepted_cycle_commits_model_and_evidence() {
        let mut engine = Engine::default();
        engine.invalidate();
        let ticket = engine.begin_cycle();
        let result = engine.accept(ticket, &observation(ticket.cycle, 1), &settings());

        assert_eq!(result.unwrap().model.workspace.slots.len(), 1);
        assert_eq!(engine.model().workspace.slots.len(), 1);
    }

    #[test]
    fn invalidation_rejects_an_in_flight_cycle_without_mutation() {
        let mut engine = Engine::default();
        engine.invalidate();
        let ticket = engine.begin_cycle();
        engine.invalidate();

        let error = engine
            .accept(ticket, &observation(ticket.cycle, 1), &settings())
            .unwrap_err();
        assert_eq!(error.current_revision, 2);
        assert!(engine.model().workspace.slots.is_empty());
    }

    #[test]
    fn mismatched_observation_cycle_is_rejected() {
        let mut engine = Engine::default();
        let ticket = engine.begin_cycle();

        assert!(
            engine
                .accept(ticket, &observation(ticket.cycle + 1, 1), &settings())
                .is_err()
        );
    }

    #[test]
    fn newer_cycle_supersedes_an_older_ticket_in_the_same_revision() {
        let mut engine = Engine::default();
        let older = engine.begin_cycle();
        let current = engine.begin_cycle();

        assert!(
            engine
                .accept(older, &observation(older.cycle, 1), &settings())
                .is_err()
        );
        assert!(
            engine
                .accept(current, &observation(current.cycle, 1), &settings())
                .is_ok()
        );
    }

    #[test]
    fn accepted_ticket_cannot_be_committed_twice() {
        let mut engine = Engine::default();
        let ticket = engine.begin_cycle();
        let observation = observation(ticket.cycle, 1);

        assert!(engine.accept(ticket, &observation, &settings()).is_ok());
        assert!(engine.accept(ticket, &observation, &settings()).is_err());
    }
}
