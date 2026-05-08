//! Panel registry — maps plugin-allocated `panel_id` to mounted spec.
//!
//! The registry is the source of truth for "which panels exist and
//! what spec are they currently rendering." It does *not* own the
//! virtual buffer the rendered output goes into — the plugin still
//! owns the virtual buffer and passes its `BufferId` at mount time.

use fresh_core::api::WidgetSpec;
use fresh_core::BufferId;
use std::collections::HashMap;

/// Plugin-allocated panel identifier. Unique within a plugin; the
/// editor does not interpret the value.
pub type PanelId = u64;

/// Per-panel state retained between renders. The reconciler will use
/// the previous spec to compute the minimum mutation when a future
/// `UpdateWidgetPanel` arrives.
#[derive(Debug, Clone)]
pub struct WidgetPanelState {
    /// The virtual buffer this panel renders into.
    pub buffer_id: BufferId,
    /// The currently-mounted spec.
    pub spec: WidgetSpec,
}

/// Global registry of mounted widget panels.
#[derive(Debug, Default)]
pub struct WidgetRegistry {
    panels: HashMap<PanelId, WidgetPanelState>,
}

impl WidgetRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Mount or replace a panel. Returns the previous state if the
    /// panel was already mounted (the dispatcher may use this to
    /// detect re-mounts on the same id).
    pub fn mount(
        &mut self,
        panel_id: PanelId,
        buffer_id: BufferId,
        spec: WidgetSpec,
    ) -> Option<WidgetPanelState> {
        self.panels
            .insert(panel_id, WidgetPanelState { buffer_id, spec })
    }

    /// Replace the spec on an already-mounted panel.
    /// Returns `Ok(buffer_id)` to render into, or `Err(())` if no
    /// panel exists for that id (caller should drop the update —
    /// the plugin re-emitted after unmount).
    pub fn update(&mut self, panel_id: PanelId, spec: WidgetSpec) -> Result<BufferId, ()> {
        match self.panels.get_mut(&panel_id) {
            Some(state) => {
                state.spec = spec;
                Ok(state.buffer_id)
            }
            None => Err(()),
        }
    }

    /// Tear down a panel. Returns the buffer_id the panel was
    /// rendering into, so the caller can clear the buffer if it
    /// owns it.
    pub fn unmount(&mut self, panel_id: PanelId) -> Option<BufferId> {
        self.panels.remove(&panel_id).map(|s| s.buffer_id)
    }

    /// Read-only access to a panel's current state.
    pub fn get(&self, panel_id: PanelId) -> Option<&WidgetPanelState> {
        self.panels.get(&panel_id)
    }

    /// All currently-mounted panel ids — useful for theme-change
    /// re-render passes (every panel re-renders against the new
    /// theme without plugin involvement).
    pub fn panel_ids(&self) -> Vec<PanelId> {
        self.panels.keys().copied().collect()
    }
}
