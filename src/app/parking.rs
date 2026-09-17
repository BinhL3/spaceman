use super::*;

/// How long a fresh park must settle before an on-screen observation of that
/// window is trusted as a genuine reappearance rather than in-flight movement.
const PARK_SETTLE_GRACE: Duration = Duration::from_secs(2);
/// Two confirmed self-un-parks within this window quarantines the window:
/// its application insists on managing it, and fighting back loses.
const QUARANTINE_WINDOW: Duration = Duration::from_secs(300);
/// A restore is confirmed when the window server reports the window near its
/// target. Loose on purpose: apps may clamp the exact position, while a
/// still-parked window sits a full display dimension away.
pub(super) const RESTORE_CONFIRM_TOLERANCE: i32 = 64;
/// After this many undone re-parks in a row the window's reappearance is
/// honored (re-homed) instead of fought; repeated honors quarantine it.
pub(super) const REPARK_ATTEMPT_LIMIT: u8 = 3;

/// Shared per-cycle context for parking decisions.
struct ParkContext<'a> {
    displays: &'a [crate::Rect],
    frontmost: &'a Option<AppInstanceId>,
    server_frames: &'a HashMap<u32, crate::Rect>,
    visible: &'a [ResolvedWorkspace],
}

impl<B: WorkspaceBackend> WorkspaceRuntime<B> {
    /// A window observed on-screen while still recorded as parked was brought back by
    /// its app or the user. Accessibility frames can be stale right after a park, so
    /// the reappearance must be confirmed by the window server before trusting it.
    /// Fighting a genuinely reappeared window back off-screen loses, so drop the
    /// stale recovery frame and re-home the window to the workspace it reappeared on.
    pub(super) fn heal_reappeared_window(
        &mut self,
        window: &crate::ObservedWindow,
        visible: &[ResolvedWorkspace],
        server_frames: &HashMap<u32, crate::Rect>,
    ) {
        let Some(original) = self.parked.get(&window.id).copied() else {
            return;
        };
        if self
            .recently_parked
            .get(&window.id)
            .is_some_and(|parked_at| parked_at.elapsed() < PARK_SETTLE_GRACE)
        {
            // The park write may still be applying inside the app; wait it out.
            return;
        }
        // The observation may be stale; only the window server is trusted.
        let Some(frame) = server_frames.get(&window.id.window_id).copied() else {
            return;
        };
        let Some(target) = visible
            .iter()
            .find(|resolved| resolved.output.contains_center(frame))
            .map(|resolved| resolved.workspace)
        else {
            return;
        };
        if self.backend.frontmost_application().as_ref() != Some(&window.id.app) {
            // A background reappearance is an app re-asserting its saved bounds
            // (Electron does this on screen-parameter changes), not the user
            // summoning the window. Keep the recovery frame; parking pushes it
            // back off-screen instead of re-homing it.
            return;
        }
        self.honor_reappearance(&window.id, frame, original, target);
    }

    /// Honor a window that will not stay parked: drop the recovery frame and
    /// re-home it to the workspace it insists on appearing on. A second honor
    /// within [`QUARANTINE_WINDOW`] quarantines the window for the session.
    fn honor_reappearance(
        &mut self,
        window: &crate::PhysicalWindowId,
        frame: crate::Rect,
        original: crate::Rect,
        target: WorkspaceId,
    ) {
        self.commit_unparked(window);
        log_event!(
            "heal: {:?} reappeared at {:?}; dropping recovery frame {:?}",
            window,
            frame,
            original
        );
        self.persist_removals_or_rollback(vec![(window.clone(), original)]);
        if self.parked.contains_key(window) {
            return;
        }
        self.recent_heals
            .retain(|_, healed_at| healed_at.elapsed() < QUARANTINE_WINDOW);
        if self
            .recent_heals
            .insert(window.clone(), Instant::now())
            .is_some()
        {
            // Second confirmed self-un-park in a short window: the app insists
            // on managing this window. Stop fighting it for the session.
            log_event!(
                "quarantine: {:?} keeps un-parking itself; leaving it unmanaged",
                window
            );
            if let Some(owner) = self.workspaces.workspace_of_any(window) {
                self.forget_window(owner, window);
            }
            self.quarantined.insert(window.clone());
            return;
        }
        if self.workspaces.floating_workspace_of(window).is_some() {
            log_event!(
                "heal: re-home floating {:?} -> workspace {}",
                window,
                target
            );
            self.workspaces.own_floating(window.clone(), target);
        } else if let Some(source) = self.workspaces.move_window(window, target) {
            self.clear_fullscreen_restore_if_inactive(source);
            self.clear_relocation_evidence(source, target, window);
            log_event!(
                "heal: re-home tiled {:?} workspace {} -> {}",
                window,
                source,
                target
            );
        }
    }

    pub(super) fn park_inactive(
        &mut self,
        outputs: &macos::DisplayOutputs,
        visible: &[ResolvedWorkspace],
        server_frames: &HashMap<u32, crate::Rect>,
        cancelled: &AtomicBool,
        failed: &mut usize,
    ) {
        let visible_workspaces: HashSet<_> =
            visible.iter().map(|resolved| resolved.workspace).collect();
        let inactive = self.workspaces.inactive_windows_except(&visible_workspaces);
        let displays = outputs.frames().collect::<Vec<_>>();
        let frontmost = self.backend.frontmost_application();
        let context = ParkContext {
            displays: &displays,
            frontmost: &frontmost,
            server_frames,
            visible,
        };
        let mut candidates = Vec::new();
        for (workspace, window) in inactive {
            if cancelled.load(Ordering::Acquire) {
                return;
            }
            let output = output_for(&self.workspaces, workspace, outputs);
            if let Some(original) = self.parked.get(&window).copied() {
                self.repark_drifted_window(&window, original, output, &context);
                continue;
            }
            let frame = match self.backend.frame(&window) {
                Ok(frame) => frame,
                Err(macos::WindowAccessError::Gone) => {
                    self.forget_window(workspace, &window);
                    continue;
                }
                Err(macos::WindowAccessError::Unavailable) => {
                    *failed += 1;
                    continue;
                }
            };
            let frame = server_frames
                .get(&window.window_id)
                .copied()
                .unwrap_or(frame);
            if !displays
                .iter()
                .any(|display| display.contains_center(frame))
            {
                // Already off-screen by the app's own doing; leave it alone rather
                // than recording an off-screen frame as recovery data.
                continue;
            }
            let target = crate::geometry::parked_frame(output, &displays, frame);
            candidates.push((workspace, window, frame, target));
        }
        if candidates.is_empty() {
            return;
        }
        for (_, window, frame, _) in &candidates {
            self.parked.insert(window.clone(), *frame);
        }
        if let Err(error) = self.backend.persist_parked(&self.parked) {
            for (_, window, _, _) in &candidates {
                self.parked.remove(window);
            }
            *failed += candidates.len();
            log_event!("failed to persist parked windows: {error}");
            return;
        }

        let mut restored = Vec::new();
        for (workspace, window, frame, target) in candidates {
            log_event!(
                "park inactive: {:?} (workspace {}) from {:?}",
                window,
                workspace,
                frame
            );
            match self.backend.set_position(&window, target) {
                macos::ApplyOutcome::Applied => {
                    self.recently_parked.insert(window.clone(), Instant::now());
                }
                macos::ApplyOutcome::Gone => {
                    self.parked.remove(&window);
                    restored.push((window.clone(), frame));
                    self.forget_window(workspace, &window);
                }
                _ => {
                    *failed += 1;
                    if matches!(
                        self.restore_window_uncommitted(&window),
                        macos::ApplyOutcome::Applied | macos::ApplyOutcome::Gone
                    ) && !self.parked.contains_key(&window)
                    {
                        restored.push((window, frame));
                    }
                }
            }
        }
        self.persist_removals_or_rollback(restored);
    }

    /// A parked window observed on-screen in the background re-asserted its
    /// saved bounds (Electron does this on screen-parameter changes). Push it
    /// back to its parked spot; the recovery frame is already recorded. A
    /// frontmost app's window is left for heal to re-home instead.
    fn repark_drifted_window(
        &mut self,
        window: &crate::PhysicalWindowId,
        original: crate::Rect,
        output: crate::Rect,
        context: &ParkContext<'_>,
    ) {
        let Some(server) = context.server_frames.get(&window.window_id).copied() else {
            return;
        };
        if !context
            .displays
            .iter()
            .any(|display| display.contains_center(server))
        {
            // Only a window that STAYED parked past the settle grace ends a
            // fight; right after a re-park write the app may still undo it.
            if self
                .recently_parked
                .get(window)
                .is_none_or(|parked_at| parked_at.elapsed() >= PARK_SETTLE_GRACE)
            {
                self.repark_attempts.remove(window);
            }
            return;
        }
        if context.frontmost.as_ref() == Some(&window.app) {
            return;
        }
        if self
            .recently_parked
            .get(window)
            .is_some_and(|parked_at| parked_at.elapsed() < PARK_SETTLE_GRACE)
        {
            return;
        }
        let attempts = self.repark_attempts.entry(window.clone()).or_insert(0);
        *attempts += 1;
        if *attempts > REPARK_ATTEMPT_LIMIT {
            // The app undoes every park. Fighting on would flicker forever;
            // honor the reappearance instead (re-home, then quarantine if it
            // keeps happening).
            log_event!(
                "re-park escalation: {:?} will not stay parked after {} attempts",
                window,
                REPARK_ATTEMPT_LIMIT
            );
            if let Some(target) = context
                .visible
                .iter()
                .find(|resolved| resolved.output.contains_center(server))
                .map(|resolved| resolved.workspace)
            {
                self.honor_reappearance(window, server, original, target);
            }
            return;
        }
        let target = crate::geometry::parked_frame(output, context.displays, original);
        log_event!(
            "re-park: {:?} drifted back on-screen to {:?}",
            window,
            server
        );
        if matches!(
            self.backend.set_position(window, target),
            macos::ApplyOutcome::Applied
        ) {
            self.recently_parked.insert(window.clone(), Instant::now());
        }
    }

    pub(super) fn restore_all(&mut self) -> usize {
        let parked: Vec<_> = self.parked.keys().cloned().collect();
        self.restore_windows(parked)
    }

    pub(super) fn restore_windows(&mut self, windows: Vec<crate::PhysicalWindowId>) -> usize {
        let mut restored = Vec::new();
        for window in &windows {
            let Some(frame) = self.parked.get(window).copied() else {
                continue;
            };
            match self.restore_window_uncommitted(window) {
                macos::ApplyOutcome::Applied | macos::ApplyOutcome::Gone
                    if !self.parked.contains_key(window) =>
                {
                    restored.push((window.clone(), frame));
                }
                _ => {}
            }
        }
        self.persist_removals_or_rollback(restored);
        // Persistence failure rolls recovery entries back into this map. The
        // caller may only exit when both native restoration and durable state
        // removal have completed.
        windows
            .iter()
            .filter(|window| self.parked.contains_key(*window))
            .count()
    }

    fn mark_parked(&mut self, window: crate::PhysicalWindowId, frame: crate::Rect) -> bool {
        if self.parked.contains_key(&window) {
            return true;
        }
        self.parked.insert(window.clone(), frame);
        if let Err(error) = self.backend.persist_parked(&self.parked) {
            self.parked.remove(&window);
            log_event!("failed to persist parked window: {error}");
            return false;
        }
        true
    }

    pub(super) fn park_window(
        &mut self,
        window: &crate::PhysicalWindowId,
        output: crate::Rect,
        outputs: &macos::DisplayOutputs,
    ) -> macos::ApplyOutcome {
        let frame = match self.backend.frame(window) {
            Ok(frame) => frame,
            Err(macos::WindowAccessError::Gone) => return macos::ApplyOutcome::Gone,
            Err(macos::WindowAccessError::Unavailable) => {
                return macos::ApplyOutcome::Unavailable;
            }
        };
        // Prefer the window server's frame: Accessibility reads can be stale
        // right after a move, which would misclassify the window and record a
        // wrong recovery frame.
        let frame = self.backend.window_server_frame(window).unwrap_or(frame);
        if !outputs
            .frames()
            .any(|display| display.contains_center(frame))
            && !self.parked.contains_key(window)
        {
            // The app keeps this window off-screen itself. There is nothing to hide
            // and no on-screen frame worth recording for recovery.
            log_event!("park skip: {:?} already off-screen at {:?}", window, frame);
            return macos::ApplyOutcome::Applied;
        }
        if !self.mark_parked(window.clone(), frame) {
            return macos::ApplyOutcome::Unavailable;
        }
        let displays = outputs.frames().collect::<Vec<_>>();
        let parked = crate::geometry::parked_frame(output, &displays, frame);
        let outcome = self.backend.set_position(window, parked);
        log_event!("park: {:?} from {:?} ({:?})", window, frame, outcome);
        if matches!(outcome, macos::ApplyOutcome::Applied) {
            self.recently_parked.insert(window.clone(), Instant::now());
        } else {
            let _ = self.restore_window(window);
        }
        outcome
    }

    pub(super) fn restore_window(
        &mut self,
        window: &crate::PhysicalWindowId,
    ) -> macos::ApplyOutcome {
        let frame = self.parked.get(window).copied();
        let outcome = self.restore_window_uncommitted(window);
        if !self.parked.contains_key(window)
            && let Some(frame) = frame
        {
            self.persist_removals_or_rollback(vec![(window.clone(), frame)]);
        }
        outcome
    }

    fn restore_window_uncommitted(
        &mut self,
        window: &crate::PhysicalWindowId,
    ) -> macos::ApplyOutcome {
        let Some(frame) = self.parked.get(window).copied() else {
            return macos::ApplyOutcome::Applied;
        };
        let outcome = self.write_restore_position(window, frame);
        if let macos::ApplyOutcome::Applied = outcome {
            // Accessibility read-back is optimistic for apps that apply
            // moves asynchronously. Keep the recovery frame until the
            // window server confirms arrival; cycles retry until it does.
            // A window the window server cannot see is not confirmed.
            let confirmed = self
                .backend
                .window_server_frame(window)
                .is_some_and(|actual| {
                    actual.position_approximately_equals(frame, RESTORE_CONFIRM_TOLERANCE)
                });
            if confirmed {
                self.commit_unparked(window);
            } else {
                log_event!(
                    "restore pending: {:?} not yet at {:?} per the window server",
                    window,
                    frame
                );
            }
        }
        outcome
    }

    /// Apply a restore write and update transient parking state. Confirmation
    /// and durable removal remain the caller's responsibility so bulk paths
    /// can share one window-server snapshot and one persistence transaction.
    pub(super) fn write_restore_position(
        &mut self,
        window: &crate::PhysicalWindowId,
        frame: crate::Rect,
    ) -> macos::ApplyOutcome {
        let outcome = self.backend.set_position(window, frame);
        log_event!("restore: {:?} to {:?} ({:?})", window, frame, outcome);
        match outcome {
            macos::ApplyOutcome::Applied => {
                // Also throttles the next retry and heal consideration.
                self.recently_parked.insert(window.clone(), Instant::now());
            }
            macos::ApplyOutcome::Gone => self.commit_unparked(window),
            _ => {}
        }
        outcome
    }

    /// The window is confirmed no longer parked; retire all parking state.
    pub(super) fn commit_unparked(&mut self, window: &crate::PhysicalWindowId) {
        self.parked.remove(window);
        self.recently_parked.remove(window);
        self.repark_attempts.remove(window);
    }

    pub(super) fn persist_removals_or_rollback(
        &mut self,
        restored: Vec<(crate::PhysicalWindowId, crate::Rect)>,
    ) {
        if restored.is_empty() {
            return;
        }
        if let Err(error) = self.backend.persist_parked(&self.parked) {
            self.parked.extend(restored);
            log_event!("failed to persist restored windows: {error}");
        }
    }
}
