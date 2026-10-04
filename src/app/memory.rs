use std::collections::BTreeMap;

use crate::window_memory::{RememberedWindow, WindowMemory};

use super::*;

/// How long a newly launched application's windows may claim its remembered
/// placements. Windows it opens later are adopted normally.
pub(super) const RECALL_WINDOW: Duration = Duration::from_secs(60);
/// How long a changed arrangement must hold before it replaces an
/// application's memory. Quitting closes windows one by one before the
/// process exits; those intermediate states must not overwrite the memory.
pub(super) const MEMORY_SETTLE: Duration = Duration::from_secs(10);

/// One relaunched application instance claiming its remembered placements.
pub(super) struct Recall {
    bundle_id: String,
    pub(super) opened: Instant,
    /// Remembered placements not yet claimed by a window.
    remaining: Vec<RememberedWindow>,
    claimed: HashMap<crate::PhysicalWindowId, RememberedWindow>,
}

#[derive(Default)]
pub(super) struct MemoryState {
    pub(super) memory: WindowMemory,
    pub(super) recalls: HashMap<AppInstanceId, Recall>,
    pub(super) pending: HashMap<String, (Vec<RememberedWindow>, Instant)>,
    /// Every observed instance's bundle. An instance without an open recall
    /// may have its ownership recorded.
    pub(super) bundles: HashMap<AppInstanceId, String>,
}

impl MemoryState {
    pub(super) fn recalled(&self, window: &crate::PhysicalWindowId) -> Option<RememberedWindow> {
        self.recalls
            .get(&window.app)
            .and_then(|recall| recall.claimed.get(window))
            .copied()
    }
}

impl<B: WorkspaceBackend> WorkspaceRuntime<B> {
    /// Close recalls whose window lapsed, whose placements are all claimed,
    /// or whose process exited.
    pub(super) fn close_recalls(&mut self) {
        let backend = &mut self.backend;
        let closed: Vec<_> = self
            .memory
            .recalls
            .extract_if(|app, recall| {
                recall.opened.elapsed() >= RECALL_WINDOW
                    || recall.remaining.is_empty()
                    || !backend.is_process_running(app)
            })
            .map(|(_, recall)| recall)
            .filter(|recall| !recall.remaining.is_empty())
            .collect();
        for recall in closed {
            log_event!(
                "recall closed: {} left {} remembered placement(s) unclaimed",
                recall.bundle_id,
                recall.remaining.len()
            );
            self.journal_event(crate::journal::JournalEvent::RecallExpired {
                bundle_id: recall.bundle_id,
                unclaimed: recall.remaining.len(),
            });
        }
    }

    /// Decide whether an observed application instance is a relaunch that may
    /// recall remembered placements, and assign them to its unowned windows.
    pub(super) fn prepare_recall(
        &mut self,
        app: &crate::ObservedApplication,
        windows: &[crate::ObservedWindow],
    ) {
        if self
            .memory
            .bundles
            .insert(app.id.clone(), app.bundle_id.clone())
            .is_none()
        {
            self.open_recall(app);
        }
        let Some(recall) = self.memory.recalls.get_mut(&app.id) else {
            return;
        };
        let candidates: Vec<_> = windows
            .iter()
            .filter(|window| {
                !recall.claimed.contains_key(&window.id)
                    && self.workspaces.workspace_of_any(&window.id).is_none()
                    && self
                        .workspaces
                        .replacement_workspaces(&window.id)
                        .next()
                        .is_none()
                    && !matches!(
                        self.config.disposition(&app.bundle_id, window),
                        WindowDisposition::Ignored | WindowDisposition::Route(_)
                    )
            })
            .map(|window| (window.id.clone(), window.frame))
            .collect();
        if candidates.is_empty() {
            return;
        }
        let assignments = crate::window_memory::assign(&recall.remaining, &candidates);
        let mut claimed_indices = Vec::new();
        let mut events = Vec::new();
        for (window, index) in assignments {
            let placement = recall.remaining[index];
            log_event!(
                "recall: {:?} ({}) -> workspace {}{}",
                window,
                recall.bundle_id,
                placement.workspace,
                if placement.floating {
                    " (floating)"
                } else {
                    ""
                }
            );
            events.push(crate::journal::JournalEvent::Recall {
                bundle_id: recall.bundle_id.clone(),
                window: window.window_id,
                workspace: placement.workspace,
                floating: placement.floating,
            });
            recall.claimed.insert(window, placement);
            claimed_indices.push(index);
        }
        claimed_indices.sort_unstable();
        for index in claimed_indices.into_iter().rev() {
            recall.remaining.remove(index);
        }
        for event in events {
            self.journal_event(event);
        }
    }

    /// Open a recall for an instance seen for the first time if it owns no
    /// windows yet and its bundle has remembered placements it may claim.
    fn open_recall(&mut self, app: &crate::ObservedApplication) {
        let owns_windows = WorkspaceId::all().any(|workspace| {
            self.workspaces
                .app_instances(workspace)
                .any(|owner| owner == &app.id)
        }) || !self.workspaces.floating_windows_for_app(&app.id).is_empty();
        if owns_windows {
            return;
        }
        let remembered = self
            .memory
            .memory
            .get(&app.bundle_id)
            .map(|remembered| {
                remembered
                    .windows
                    .iter()
                    .filter(|placement| self.can_recall_to(placement.workspace))
                    .copied()
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        if remembered.is_empty() {
            return;
        }
        log_event!(
            "recall: {} launched as {:?}; {} remembered placement(s) available",
            app.bundle_id,
            app.id,
            remembered.len()
        );
        self.memory.recalls.insert(
            app.id.clone(),
            Recall {
                bundle_id: app.bundle_id.clone(),
                opened: Instant::now(),
                remaining: remembered,
                claimed: HashMap::new(),
            },
        );
    }

    fn can_recall_to(&self, workspace: WorkspaceId) -> bool {
        self.workspaces.is_enabled(workspace)
            && self
                .scope
                .manages_selector(self.workspaces.assignment(workspace))
    }

    /// Fold settled ownership back into the memory once it has held for
    /// [`MEMORY_SETTLE`], then persist any change.
    pub(super) fn record_window_memory(&mut self, server_frames: &HashMap<u32, crate::Rect>) {
        let recalling: HashSet<_> = self
            .memory
            .recalls
            .values()
            .map(|recall| recall.bundle_id.clone())
            .collect();
        let mut current: BTreeMap<String, Vec<(u32, RememberedWindow)>> = BTreeMap::new();
        let mut add = |window: &crate::PhysicalWindowId, workspace, floating| {
            let Some(bundle_id) = self
                .memory
                .bundles
                .get(&window.app)
                .filter(|bundle_id| !bundle_id.is_empty() && !recalling.contains(*bundle_id))
            else {
                return;
            };
            let Some(frame) = self
                .parked
                .get(window)
                .or_else(|| server_frames.get(&window.window_id))
                .copied()
            else {
                return;
            };
            current.entry(bundle_id.clone()).or_default().push((
                window.window_id,
                RememberedWindow {
                    workspace,
                    frame,
                    floating,
                },
            ));
        };
        for workspace in WorkspaceId::all() {
            for window in self.workspaces.engine(workspace).bound_windows() {
                add(&window, workspace, false);
            }
        }
        for (window, workspace) in self.workspaces.floating_entries() {
            add(window, workspace, true);
        }
        let now = Instant::now();
        self.memory
            .pending
            .retain(|bundle_id, _| current.contains_key(bundle_id));
        let mut recorded = Vec::new();
        for (bundle_id, mut windows) in current {
            windows.sort_by_key(|(window_id, _)| *window_id);
            let windows: Vec<_> = windows
                .into_iter()
                .map(|(_, placement)| placement)
                .take(crate::window_memory::MAX_WINDOWS_PER_APP)
                .collect();
            if self
                .memory
                .memory
                .get(&bundle_id)
                .is_some_and(|remembered| remembered.windows == windows)
            {
                self.memory.pending.remove(&bundle_id);
                continue;
            }
            match self.memory.pending.get(&bundle_id) {
                Some((pending, since)) if *pending == windows => {
                    if now.duration_since(*since) >= MEMORY_SETTLE {
                        self.memory.pending.remove(&bundle_id);
                        let count = windows.len();
                        if self.memory.memory.record(
                            &bundle_id,
                            windows,
                            crate::journal::unix_millis() / 1_000,
                        ) {
                            recorded.push((bundle_id, count));
                        }
                    }
                }
                _ => {
                    self.memory.pending.insert(bundle_id, (windows, now));
                }
            }
        }
        if recorded.is_empty() {
            return;
        }
        for (bundle_id, windows) in recorded {
            self.journal_event(crate::journal::JournalEvent::Remembered { bundle_id, windows });
        }
        // Advisory state: a failed write is not retried until the memory
        // changes again, since retrying every cycle would only repeat the error.
        if let Err(error) = self.backend.persist_window_memory(&self.memory.memory) {
            log_event!("failed to persist window memory: {error}");
        }
    }
}
