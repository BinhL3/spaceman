//! The strip's viewport camera on the worker: when focus or widths change it aims the camera
//! (and any column that moved along the ribbon) with springs, hands the glide to the main thread
//! as a [`StripMotion`], and lays the strip out for good once the springs settle.
//!
//! Every column the glide leaves out of view has its frame recorded durably before the glide
//! starts, exactly as parking does before it moves a window off screen.

use super::*;
use crate::{
    motion::{CameraSpring, ScrollingDisplayBounds, SpringParams},
    strip::{MotionColumn, RibbonColumn, StripLayout, StripMotion},
};

/// How long after the springs settle the worker waits before its own layout pass, so the main
/// thread's final frame lands first.
pub(in crate::app) const SETTLE_GRACE: Duration = Duration::from_millis(40);

/// What the worker tells the main thread about strip glides.
#[derive(Clone, Debug)]
pub(in crate::app) enum StripCommand {
    /// Start or retarget this workspace's glide.
    Glide(Box<StripMotion>),
    /// Stop moving this workspace's windows now; the worker has taken them back.
    Halt(WorkspaceId),
}

/// Where a strip's viewport is.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(in crate::app) enum ViewportCamera {
    Static(f64),
    Animating(CameraSpring),
}

impl ViewportCamera {
    pub(in crate::app) fn target(self) -> f64 {
        match self {
            Self::Static(position) => position,
            Self::Animating(spring) => spring.target(),
        }
    }

    fn spring(self, now: Instant) -> CameraSpring {
        match self {
            Self::Static(position) => CameraSpring::at_rest(SpringParams::default(), position, now),
            Self::Animating(spring) => spring,
        }
    }
}

/// One workspace's transient strip view; nothing here is persisted.
#[derive(Clone, Debug, Default)]
pub(in crate::app) struct StripView {
    /// `None` until the strip is first laid out, which happens without a glide.
    pub(in crate::app) camera: Option<ViewportCamera>,
    /// Each column's ribbon position at the last layout, so a column that moves can glide.
    placed: HashMap<crate::SlotId, i32>,
    /// Offsets springing to zero for columns that moved along the ribbon.
    shifts: HashMap<crate::SlotId, CameraSpring>,
    /// When the running glide settles.
    settles_at: Option<Instant>,
}

/// Every workspace's strip view, plus the glides to hand to the main thread.
#[derive(Debug)]
pub(in crate::app) struct StripViews {
    views: [StripView; WorkspaceId::COUNT],
    pub(in crate::app) outbox: Vec<StripCommand>,
}

impl Default for StripViews {
    fn default() -> Self {
        Self {
            views: std::array::from_fn(|_| StripView::default()),
            outbox: Vec::new(),
        }
    }
}

impl StripViews {
    pub(in crate::app) fn view(&self, workspace: WorkspaceId) -> &StripView {
        &self.views[workspace.index()]
    }

    /// The earliest moment a running glide is due its settling layout.
    pub(in crate::app) fn next_deadline(&self) -> Option<Instant> {
        self.views
            .iter()
            .filter_map(|view| view.settles_at)
            .min()
            .map(|at| at + SETTLE_GRACE)
    }

    fn in_motion(&self, workspace: WorkspaceId) -> bool {
        matches!(
            self.views[workspace.index()].camera,
            Some(ViewportCamera::Animating(_))
        )
    }

    /// Forgets where `workspace`'s camera was, so its next layout rests without a glide.
    pub(in crate::app) fn reset(&mut self, workspace: WorkspaceId) {
        if self.in_motion(workspace) {
            self.outbox.push(StripCommand::Halt(workspace));
        }
        self.views[workspace.index()] = StripView::default();
    }

    /// Stops every glide where it is aimed.
    pub(in crate::app) fn halt_all(&mut self) {
        for (index, view) in self.views.iter_mut().enumerate() {
            if let Some(ViewportCamera::Animating(spring)) = view.camera {
                view.camera = Some(ViewportCamera::Static(spring.target()));
                view.shifts.clear();
                view.settles_at = None;
                self.outbox
                    .push(StripCommand::Halt(WorkspaceId::from_index(index)));
            }
        }
    }
}

/// A strip's rest layout for one camera target.
pub(in crate::app) struct StripPlan {
    pub(in crate::app) output: crate::Rect,
    pub(in crate::app) bounds: ScrollingDisplayBounds,
    pub(in crate::app) ribbon: Vec<RibbonColumn>,
    pub(in crate::app) target: f64,
    pub(in crate::app) layout: StripLayout,
}

impl<B: WorkspaceBackend> WorkspaceRuntime<B> {
    /// Whether `workspace`'s windows belong to a glide right now.
    pub(in crate::app) fn strip_in_motion(&self, workspace: WorkspaceId) -> bool {
        self.strip_views.in_motion(workspace)
    }

    /// Aims `workspace`'s camera at `plan`, starting or bending a glide when the target or a
    /// column's ribbon position changed. Returns whether a glide owns the strip's windows now.
    pub(in crate::app) fn steer_strip(&mut self, workspace: WorkspaceId, plan: &StripPlan) -> bool {
        let now = Instant::now();
        let animates = self.backend.presents_motion();
        let view = &mut self.strip_views.views[workspace.index()];
        let placed: HashMap<_, _> = plan
            .ribbon
            .iter()
            .map(|column| (column.slot, column.x))
            .collect();
        let Some(camera) = view.camera else {
            view.camera = Some(ViewportCamera::Static(plan.target));
            view.placed = placed;
            return false;
        };
        let mut changed = (camera.target() - plan.target).abs() >= 0.5;
        for column in &plan.ribbon {
            let Some(old) = view.placed.get(&column.slot).copied() else {
                continue;
            };
            let current = view.shifts.get(&column.slot).map(|shift| shift.sample(now));
            let offset = f64::from(old - column.x) + current.map_or(0.0, |s| s.position);
            if old != column.x && offset.abs() >= 0.5 {
                let velocity = current.map_or(0.0, |s| s.velocity);
                view.shifts.insert(
                    column.slot,
                    CameraSpring::with_state(SpringParams::default(), offset, velocity, 0.0, now),
                );
                changed = true;
            }
        }
        view.shifts.retain(|slot, _| placed.contains_key(slot));
        view.placed = placed;
        if !animates {
            view.camera = Some(ViewportCamera::Static(plan.target));
            view.shifts.clear();
            return false;
        }
        if !changed {
            return matches!(camera, ViewportCamera::Animating(_));
        }
        let mut spring = camera.spring(now);
        spring.retarget(plan.target, now);
        if !self.prepare_strip_glide(workspace, plan, spring, now) {
            let view = &mut self.strip_views.views[workspace.index()];
            if matches!(view.camera, Some(ViewportCamera::Animating(_))) {
                self.strip_views.outbox.push(StripCommand::Halt(workspace));
            }
            let view = &mut self.strip_views.views[workspace.index()];
            view.camera = Some(ViewportCamera::Static(plan.target));
            view.shifts.clear();
            view.settles_at = None;
            return false;
        }
        true
    }

    /// Records the frames of columns the glide takes out of view, sizes columns whose width
    /// changed, and queues the glide. Returns false when a frame could not be recorded; the
    /// caller then lays the strip out without a glide, parking as usual.
    fn prepare_strip_glide(
        &mut self,
        workspace: WorkspaceId,
        plan: &StripPlan,
        camera: CameraSpring,
        now: Instant,
    ) -> bool {
        let engine = self.workspaces.engine(workspace);
        let columns: Vec<_> = plan
            .ribbon
            .iter()
            .filter_map(|column| {
                let window = engine.model().slot(column.slot)?.bound_window()?.clone();
                Some((*column, window))
            })
            .collect();
        let displays: Vec<_> = self
            .strip_outputs()
            .map(|outputs| outputs.frames().collect())
            .unwrap_or_else(|| vec![plan.output]);
        let view = self.strip_views.view(workspace).clone();
        let shift_at = |slot| {
            view.shifts
                .get(&slot)
                .map_or(0.0, |s: &CameraSpring| s.position(now))
        };
        let current_camera = view
            .camera
            .map_or(plan.target, |camera| camera.spring(now).position(now));
        let mut riders = Vec::new();
        for (column, window) in columns {
            let rest = plan.layout.rest(column.slot);
            let hidden = !matches!(rest, Some(crate::strip::ColumnRest::Visible(_)));
            let current = self
                .backend
                .window_server_frame(&window)
                .or_else(|| self.backend.frame(&window).ok());
            let on_screen =
                current.is_some_and(|frame| crate::geometry::shows_on(frame, &displays));
            if hidden && !self.parked.contains_key(&window) && on_screen {
                let Some(frame) = current else {
                    return false;
                };
                if !self.mark_parked(window.clone(), frame) {
                    return false;
                }
            }
            let x_now = f64::from(column.x) + shift_at(column.slot);
            let in_view_now = {
                let left = f64::from(plan.output.x) + x_now - current_camera;
                left < f64::from(plan.output.x + plan.output.width)
                    && left + f64::from(column.width) > f64::from(plan.output.x)
            };
            if matches!(rest, Some(crate::strip::ColumnRest::Parked))
                && !in_view_now
                && self.parked.contains_key(&window)
            {
                // Parked beyond a shared edge before and after: it stays where it is.
                continue;
            }
            if let Some(frame) = current
                && (frame.width != column.width || frame.height != plan.output.height)
            {
                let sized = crate::Rect {
                    width: column.width,
                    height: plan.output.height,
                    ..frame
                };
                let placement = crate::Placement {
                    slot: column.slot,
                    window: Some(window.clone()),
                    frame: sized,
                };
                let settings = Settings::for_output(plan.output);
                let _ = self.backend.apply_managed(
                    std::slice::from_ref(&placement),
                    &settings,
                    &AtomicBool::new(false),
                );
            }
            riders.push(MotionColumn {
                window,
                x: column.x,
                width: column.width,
                shift: view.shifts.get(&column.slot).copied(),
            });
        }
        let Some(outputs) = self.strip_outputs() else {
            return false;
        };
        let display_id = outputs
            .displays()
            .iter()
            .find(|display| display.frame == plan.output)
            .map_or_else(|| outputs.main().id.clone(), |display| display.id.clone());
        let motion = StripMotion {
            workspace,
            display_id,
            output: plan.output,
            bounds: plan.bounds,
            camera,
            columns: riders,
        };
        let settles_at = motion.settles_at(now);
        log_event!(
            "workspace {workspace}: strip glide {:.0} -> {:.0} over {}ms ({} windows)",
            current_camera,
            plan.target,
            settles_at.saturating_duration_since(now).as_millis(),
            motion.columns.len()
        );
        let view = &mut self.strip_views.views[workspace.index()];
        view.camera = Some(ViewportCamera::Animating(camera));
        view.settles_at = Some(settles_at);
        self.strip_views.outbox.retain(
            |command| !matches!(command, StripCommand::Glide(m) if m.workspace == workspace),
        );
        self.strip_views
            .outbox
            .push(StripCommand::Glide(Box::new(motion)));
        true
    }

    /// Lays out every strip whose glide has settled by `now`: the camera comes to rest and
    /// the strip gets one confirming layout pass, which parks what ended out of view.
    pub(in crate::app) fn settle_strip_glides(&mut self, now: Instant) {
        let mut settled = Vec::new();
        for (index, view) in self.strip_views.views.iter_mut().enumerate() {
            if let (Some(ViewportCamera::Animating(spring)), Some(at)) =
                (view.camera, view.settles_at)
                && now >= at + SETTLE_GRACE
            {
                view.camera = Some(ViewportCamera::Static(spring.target()));
                view.shifts.clear();
                view.settles_at = None;
                settled.push(WorkspaceId::from_index(index));
            }
        }
        let Some(outputs) = self.strip_outputs().cloned() else {
            return;
        };
        let cancelled = AtomicBool::new(false);
        for workspace in settled {
            if !self.is_visible(workspace, &outputs) {
                continue;
            }
            let layout = self.layout_strip_workspace(workspace, &outputs, &cancelled);
            log_event!(
                "workspace {workspace}: strip glide settled ({} failed)",
                layout.failed
            );
        }
        if let Err(error) = self.persist_workspace_state_if_changed() {
            log_event!("failed to persist workspace state: {error}");
        }
    }

    /// The displays as last seen by a command.
    pub(in crate::app) fn strip_outputs(&self) -> Option<&macos::DisplayOutputs> {
        self.displays.known.as_ref()
    }
}
