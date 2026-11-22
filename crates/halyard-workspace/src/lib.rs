//! Windows, tabs and split panes for Halyard, as plain data.
//!
//! No UI code: the app crate draws what this crate describes, and every
//! operation (split, close, focus by direction, resize, zoom, move and rename
//! tabs) is unit tested here.
//!
//! - [`tree`]: one tab's split tree and its geometry.
//! - [`workspace`]: one window's tabs and the operations on them.
//! - [`persist`]: saving and restoring layouts and working directories.

pub mod persist;
pub mod tree;
pub mod workspace;

pub use persist::{LoadResult, Restored, SavedState, SavedWindow};
pub use tree::{Axis, Direction, Divider, MinSize, Node, PaneId, Rect};
pub use workspace::{Closed, Tab, Workspace};

impl LoadResult {
    /// The windows to open: the restored ones, or one new window with a
    /// single pane when nothing usable was saved.
    #[must_use]
    pub fn windows_or_default(self) -> Vec<Restored> {
        if self.windows.is_empty() {
            let (workspace, pane) = Workspace::new();
            vec![Restored { workspace, cwds: vec![(pane, None)] }]
        } else {
            self.windows
        }
    }
}
