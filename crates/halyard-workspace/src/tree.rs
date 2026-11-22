//! The split tree of one tab and its geometry.
//!
//! A tab's panes form a binary tree: a leaf is a pane, an inner node splits
//! its rectangle in two along one axis at a ratio. Geometry is computed in
//! whatever unit the caller passes (the app uses logical pixels), so the
//! tree itself holds no sizes, only ratios.

use serde::{Deserialize, Serialize};

/// Identifies a pane within one window. Never reused within a window.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct PaneId(pub u64);

/// How a split divides its rectangle.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Axis {
    /// Children side by side, divider vertical ("split right").
    Horizontal,
    /// Children stacked, divider horizontal ("split down").
    Vertical,
}

/// A direction for focus moves and resizing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Direction {
    /// Toward smaller x.
    Left,
    /// Toward larger x.
    Right,
    /// Toward smaller y.
    Up,
    /// Toward larger y.
    Down,
}

impl Direction {
    /// The axis a move in this direction crosses.
    #[must_use]
    pub fn axis(self) -> Axis {
        match self {
            Self::Left | Self::Right => Axis::Horizontal,
            Self::Up | Self::Down => Axis::Vertical,
        }
    }

    fn sign(self) -> f32 {
        match self {
            Self::Left | Self::Up => -1.0,
            Self::Right | Self::Down => 1.0,
        }
    }
}

/// An axis-aligned rectangle.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Rect {
    /// Left edge.
    pub x: f32,
    /// Top edge.
    pub y: f32,
    /// Width.
    pub w: f32,
    /// Height.
    pub h: f32,
}

impl Rect {
    /// Build a rectangle.
    #[must_use]
    pub const fn new(x: f32, y: f32, w: f32, h: f32) -> Self {
        Self { x, y, w, h }
    }

    /// Right edge.
    #[must_use]
    pub fn right(&self) -> f32 {
        self.x + self.w
    }

    /// Bottom edge.
    #[must_use]
    pub fn bottom(&self) -> f32 {
        self.y + self.h
    }

    /// True when `(px, py)` is inside.
    #[must_use]
    pub fn contains(&self, px: f32, py: f32) -> bool {
        px >= self.x && px < self.right() && py >= self.y && py < self.bottom()
    }

    fn start(&self, axis: Axis) -> f32 {
        match axis {
            Axis::Horizontal => self.x,
            Axis::Vertical => self.y,
        }
    }

    fn extent(&self, axis: Axis) -> f32 {
        match axis {
            Axis::Horizontal => self.w,
            Axis::Vertical => self.h,
        }
    }

    /// Split along `axis`: (first, divider, second).
    fn split(&self, axis: Axis, ratio: f32, gap: f32) -> (Rect, Rect, Rect) {
        let avail = (self.extent(axis) - gap).max(0.0);
        let first = (avail * ratio).round().clamp(0.0, avail);
        let second = avail - first;
        match axis {
            Axis::Horizontal => (
                Rect::new(self.x, self.y, first, self.h),
                Rect::new(self.x + first, self.y, gap, self.h),
                Rect::new(self.x + first + gap, self.y, second, self.h),
            ),
            Axis::Vertical => (
                Rect::new(self.x, self.y, self.w, first),
                Rect::new(self.x, self.y + first, self.w, gap),
                Rect::new(self.x, self.y + first + gap, self.w, second),
            ),
        }
    }
}

/// Smallest pane size, per axis.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MinSize {
    /// Minimum width.
    pub w: f32,
    /// Minimum height.
    pub h: f32,
}

impl MinSize {
    fn along(self, axis: Axis) -> f32 {
        match axis {
            Axis::Horizontal => self.w,
            Axis::Vertical => self.h,
        }
    }
}

/// The smallest and largest ratio a split keeps, whatever the sizes.
pub const RATIO_LIMITS: (f32, f32) = (0.05, 0.95);

/// One node of the split tree.
#[derive(Clone, Debug, PartialEq)]
pub enum Node {
    /// A pane.
    Pane(PaneId),
    /// Two children.
    Split {
        /// Which way the rectangle is divided.
        axis: Axis,
        /// Share of the space (after the divider) given to `first`, in
        /// [`RATIO_LIMITS`].
        ratio: f32,
        /// Left or top child.
        first: Box<Node>,
        /// Right or bottom child.
        second: Box<Node>,
    },
}

/// A path from the root to a node: `false` goes to `first`, `true` to `second`.
pub type Path = Vec<bool>;

/// A divider between the two children of a split, for drawing and dragging.
#[derive(Clone, Debug, PartialEq)]
pub struct Divider {
    /// Which split it belongs to.
    pub path: Path,
    /// The split's axis (`Horizontal` means a vertical bar you drag sideways).
    pub axis: Axis,
    /// The strip between the children.
    pub rect: Rect,
}

impl Node {
    /// Every pane, left to right and top to bottom (tree order).
    #[must_use]
    pub fn panes(&self) -> Vec<PaneId> {
        let mut out = Vec::new();
        self.collect(&mut out);
        out
    }

    fn collect(&self, out: &mut Vec<PaneId>) {
        match self {
            Node::Pane(id) => out.push(*id),
            Node::Split { first, second, .. } => {
                first.collect(out);
                second.collect(out);
            }
        }
    }

    /// Number of panes.
    #[must_use]
    pub fn len(&self) -> usize {
        match self {
            Node::Pane(_) => 1,
            Node::Split { first, second, .. } => first.len() + second.len(),
        }
    }

    /// Always false: a tree has at least one pane.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        false
    }

    /// Depth of the deepest pane (a lone pane is 0).
    #[must_use]
    pub fn depth(&self) -> usize {
        match self {
            Node::Pane(_) => 0,
            Node::Split { first, second, .. } => 1 + first.depth().max(second.depth()),
        }
    }

    /// Path to a pane.
    #[must_use]
    pub fn path_to(&self, id: PaneId) -> Option<Path> {
        match self {
            Node::Pane(p) => (*p == id).then(Vec::new),
            Node::Split { first, second, .. } => {
                if let Some(mut p) = first.path_to(id) {
                    p.insert(0, false);
                    Some(p)
                } else if let Some(mut p) = second.path_to(id) {
                    p.insert(0, true);
                    Some(p)
                } else {
                    None
                }
            }
        }
    }

    /// The node at a path.
    #[must_use]
    pub fn at(&self, path: &[bool]) -> Option<&Node> {
        match (path.split_first(), self) {
            (None, n) => Some(n),
            (Some((side, rest)), Node::Split { first, second, .. }) => {
                if *side {
                    second.at(rest)
                } else {
                    first.at(rest)
                }
            }
            (Some(_), Node::Pane(_)) => None,
        }
    }

    fn at_mut(&mut self, path: &[bool]) -> Option<&mut Node> {
        match path.split_first() {
            None => Some(self),
            Some((side, rest)) => match self {
                Node::Split { first, second, .. } => {
                    if *side {
                        second.at_mut(rest)
                    } else {
                        first.at_mut(rest)
                    }
                }
                Node::Pane(_) => None,
            },
        }
    }

    /// The rectangle of the node at `path` when the tree fills `bounds`.
    #[must_use]
    pub fn rect_of(&self, path: &[bool], bounds: Rect, gap: f32) -> Option<Rect> {
        match (path.split_first(), self) {
            (None, _) => Some(bounds),
            (Some((side, rest)), Node::Split { axis, ratio, first, second }) => {
                let (a, _, b) = bounds.split(*axis, *ratio, gap);
                if *side { second.rect_of(rest, b, gap) } else { first.rect_of(rest, a, gap) }
            }
            (Some(_), Node::Pane(_)) => None,
        }
    }

    /// Replace the pane `target` with a split of it and `new` (new second).
    /// Returns false when `target` is not in the tree.
    pub fn split_pane(&mut self, target: PaneId, new: PaneId, axis: Axis) -> bool {
        let Some(path) = self.path_to(target) else { return false };
        let Some(slot) = self.at_mut(&path) else { return false };
        let old = std::mem::replace(slot, Node::Pane(new));
        *slot = Node::Split {
            axis,
            ratio: 0.5,
            first: Box::new(old),
            second: Box::new(Node::Pane(new)),
        };
        true
    }

    /// Remove a pane; its sibling takes the parent's place (and space).
    /// Returns the pane nearest to where the removed one was, or `None` when
    /// the pane is not found or is the only one (the tree cannot be empty).
    pub fn remove_pane(&mut self, id: PaneId) -> Option<PaneId> {
        let path = self.path_to(id)?;
        let (last, parent_path) = path.split_last()?;
        let parent = self.at_mut(parent_path)?;
        let Node::Split { first, second, .. } = parent else { return None };
        let sibling = if *last {
            std::mem::replace(first.as_mut(), Node::Pane(id))
        } else {
            std::mem::replace(second.as_mut(), Node::Pane(id))
        };
        let panes = sibling.panes();
        // The removed pane was right of / below its sibling when `last`, so
        // the sibling's last pane was its neighbor; otherwise its first.
        let next = if *last { panes.last().copied() } else { panes.first().copied() };
        *parent = sibling;
        next
    }

    /// Lay the tree out in `bounds`: each pane's rectangle in tree order.
    #[must_use]
    pub fn layout(&self, bounds: Rect, gap: f32) -> Vec<(PaneId, Rect)> {
        let mut out = Vec::new();
        self.layout_into(bounds, gap, &mut out);
        out
    }

    fn layout_into(&self, bounds: Rect, gap: f32, out: &mut Vec<(PaneId, Rect)>) {
        match self {
            Node::Pane(id) => out.push((*id, bounds)),
            Node::Split { axis, ratio, first, second } => {
                let (a, _, b) = bounds.split(*axis, *ratio, gap);
                first.layout_into(a, gap, out);
                second.layout_into(b, gap, out);
            }
        }
    }

    /// Every divider when the tree fills `bounds`.
    #[must_use]
    pub fn dividers(&self, bounds: Rect, gap: f32) -> Vec<Divider> {
        let mut out = Vec::new();
        self.dividers_into(bounds, gap, &mut Vec::new(), &mut out);
        out
    }

    fn dividers_into(&self, bounds: Rect, gap: f32, path: &mut Path, out: &mut Vec<Divider>) {
        if let Node::Split { axis, ratio, first, second } = self {
            let (a, d, b) = bounds.split(*axis, *ratio, gap);
            out.push(Divider { path: path.clone(), axis: *axis, rect: d });
            path.push(false);
            first.dividers_into(a, gap, path, out);
            path.pop();
            path.push(true);
            second.dividers_into(b, gap, path, out);
            path.pop();
        }
    }

    /// The smallest extent along `axis` this subtree can shrink to.
    #[must_use]
    pub fn min_extent(&self, axis: Axis, min: MinSize, gap: f32) -> f32 {
        match self {
            Node::Pane(_) => min.along(axis),
            Node::Split { axis: a, first, second, .. } => {
                let f = first.min_extent(axis, min, gap);
                let s = second.min_extent(axis, min, gap);
                if *a == axis { f + gap + s } else { f.max(s) }
            }
        }
    }

    /// Move the divider of the split at `path` so it sits at `pos` (a
    /// coordinate along the split's axis, as from a mouse drag), keeping
    /// every pane at least `min`. Returns true when the ratio changed.
    pub fn drag_divider(
        &mut self,
        path: &[bool],
        pos: f32,
        bounds: Rect,
        gap: f32,
        min: MinSize,
    ) -> bool {
        let Some(rect) = self.rect_of(path, bounds, gap) else { return false };
        let Some(Node::Split { axis, ratio, first, second }) = self.at_mut(path) else {
            return false;
        };
        let avail = rect.extent(*axis) - gap;
        if !(avail > 0.0 && pos.is_finite()) {
            return false;
        }
        let want = pos - rect.start(*axis) - gap / 2.0;
        let lo = first.min_extent(*axis, min, gap);
        let hi = avail - second.min_extent(*axis, min, gap);
        let first_len = if lo <= hi { want.clamp(lo, hi) } else { avail / 2.0 };
        let new = (first_len / avail).clamp(RATIO_LIMITS.0, RATIO_LIMITS.1);
        if (new - *ratio).abs() < f32::EPSILON {
            return false;
        }
        *ratio = new;
        true
    }

    /// Set a split's ratio directly (clamped to [`RATIO_LIMITS`]).
    pub fn set_ratio(&mut self, path: &[bool], value: f32) -> bool {
        match self.at_mut(path) {
            Some(Node::Split { ratio, .. }) if value.is_finite() => {
                *ratio = value.clamp(RATIO_LIMITS.0, RATIO_LIMITS.1);
                true
            }
            _ => false,
        }
    }

    /// Move the nearest divider on `dir`'s axis around `pane` by `step`
    /// toward `dir`. Returns true when something moved.
    pub fn resize_pane(
        &mut self,
        pane: PaneId,
        dir: Direction,
        step: f32,
        bounds: Rect,
        gap: f32,
        min: MinSize,
    ) -> bool {
        let Some(path) = self.path_to(pane) else { return false };
        let axis = dir.axis();
        for depth in (0..path.len()).rev() {
            let split_path = &path[..depth];
            let Some(Node::Split { axis: a, ratio, .. }) = self.at(split_path) else { continue };
            if *a != axis {
                continue;
            }
            let Some(rect) = self.rect_of(split_path, bounds, gap) else { return false };
            let avail = rect.extent(axis) - gap;
            let divider = rect.start(axis) + (avail * ratio).round() + gap / 2.0;
            return self.drag_divider(split_path, divider + dir.sign() * step, bounds, gap, min);
        }
        false
    }

    /// The pane next to `from` in direction `dir`, by geometry: the nearest
    /// pane on that side that overlaps it on the other axis, preferring the
    /// largest overlap, then the one closest to its center.
    #[must_use]
    pub fn neighbor(&self, from: PaneId, dir: Direction, bounds: Rect, gap: f32) -> Option<PaneId> {
        let rects = self.layout(bounds, gap);
        let cur = rects.iter().find(|(id, _)| *id == from)?.1;
        let eps = 0.5;
        let mut best: Option<(f32, f32, f32, PaneId)> = None;
        for (id, r) in &rects {
            if *id == from {
                continue;
            }
            let (dist, overlap, center) = match dir {
                Direction::Right if r.x >= cur.right() - eps => (
                    r.x - cur.right(),
                    cur.bottom().min(r.bottom()) - cur.y.max(r.y),
                    ((r.y + r.h / 2.0) - (cur.y + cur.h / 2.0)).abs(),
                ),
                Direction::Left if r.right() <= cur.x + eps => (
                    cur.x - r.right(),
                    cur.bottom().min(r.bottom()) - cur.y.max(r.y),
                    ((r.y + r.h / 2.0) - (cur.y + cur.h / 2.0)).abs(),
                ),
                Direction::Down if r.y >= cur.bottom() - eps => (
                    r.y - cur.bottom(),
                    cur.right().min(r.right()) - cur.x.max(r.x),
                    ((r.x + r.w / 2.0) - (cur.x + cur.w / 2.0)).abs(),
                ),
                Direction::Up if r.bottom() <= cur.y + eps => (
                    cur.y - r.bottom(),
                    cur.right().min(r.right()) - cur.x.max(r.x),
                    ((r.x + r.w / 2.0) - (cur.x + cur.w / 2.0)).abs(),
                ),
                _ => continue,
            };
            if overlap <= 0.0 {
                continue;
            }
            let better = match best {
                None => true,
                Some((d, o, c, _)) => {
                    (dist, -overlap, center).partial_cmp(&(d, -o, c))
                        == Some(std::cmp::Ordering::Less)
                }
            };
            if better {
                best = Some((dist, overlap, center, *id));
            }
        }
        best.map(|b| b.3)
    }

    /// Every split's ratio, in tree order (for invariant checks).
    #[must_use]
    pub fn ratios(&self) -> Vec<f32> {
        match self {
            Node::Pane(_) => Vec::new(),
            Node::Split { ratio, first, second, .. } => {
                let mut v = vec![*ratio];
                v.extend(first.ratios());
                v.extend(second.ratios());
                v
            }
        }
    }
}
