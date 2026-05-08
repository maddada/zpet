//! Plugin widget runtime.
//!
//! Plugins describe panels as a [`WidgetSpec`](fresh_core::api::WidgetSpec)
//! tree. The runtime in this module owns the panel registry, runs the
//! reconciler against the previous spec, renders the resulting tree
//! into [`TextPropertyEntry`]s, and (in later phases) routes events
//! back through the hook system.
//!
//! v1 supports the `Row` / `Col` / `HintBar` / `Raw` widget kinds.
//! Additional kinds (`Toggle`, `Button`, `TextInput`, `List`, `Tree`,
//! `Layer`, `Transient`, `Table`) plug into the `render` dispatch
//! without changing the IPC shape.
//!
//! See `docs/internal/plugin-widget-library-design.md` for the full
//! design.

mod registry;
mod render;

pub use registry::{PanelId, WidgetPanelState, WidgetRegistry};
pub use render::render_spec;
