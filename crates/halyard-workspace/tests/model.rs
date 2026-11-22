//! Tree invariants, geometry navigation, resizing, tabs.

use halyard_workspace::{Axis, Closed, Direction, MinSize, Node, PaneId, Rect, Workspace};

const B: Rect = Rect::new(0.0, 0.0, 1000.0, 600.0);
const GAP: f32 = 4.0;
const MIN: MinSize = MinSize { w: 60.0, h: 40.0 };

fn ok(ws: &Workspace) {
    ws.check().unwrap_or_else(|e| panic!("invariant broken: {e}\n{ws:#?}"));
}

/// a | b, with b split into c over d:
/// ```text
/// +---+---+
/// |   | b |
/// | a +---+
/// |   | d |
/// +---+---+
/// ```
fn three() -> (Workspace, PaneId, PaneId, PaneId) {
    let (mut ws, a) = Workspace::new();
    let b = ws.split(Axis::Horizontal).unwrap();
    let d = ws.split(Axis::Vertical).unwrap();
    ok(&ws);
    (ws, a, b, d)
}

#[test]
fn split_right_and_down_build_the_tree() {
    let (ws, a, b, d) = three();
    assert_eq!(ws.active().root.panes(), vec![a, b, d]);
    assert_eq!(ws.focused(), Some(d), "the new pane gets focus");
    let rects = ws.active().layout(B, GAP);
    let r = |id| rects.iter().find(|(p, _)| *p == id).unwrap().1;
    assert_eq!(r(a), Rect::new(0.0, 0.0, 498.0, 600.0));
    assert_eq!(r(b), Rect::new(502.0, 0.0, 498.0, 298.0));
    assert_eq!(r(d), Rect::new(502.0, 302.0, 498.0, 298.0));
    // Panes tile the bounds exactly: areas plus gaps.
    let area: f32 = rects.iter().map(|(_, r)| r.w * r.h).sum();
    assert_eq!(area + 4.0 * 600.0 + 4.0 * 498.0, 1000.0 * 600.0);
}

#[test]
fn closing_a_pane_gives_its_space_to_the_sibling() {
    let (mut ws, a, b, d) = three();
    assert_eq!(ws.close_pane(d), Closed::Pane);
    ok(&ws);
    assert_eq!(ws.active().root.panes(), vec![a, b]);
    assert_eq!(ws.focused(), Some(b), "focus goes to the neighbor");
    let rects = ws.active().layout(B, GAP);
    assert_eq!(rects[1].1, Rect::new(502.0, 0.0, 498.0, 600.0), "b fills the right half");
    assert_eq!(ws.close_pane(a), Closed::Pane);
    assert_eq!(ws.active().layout(B, GAP), vec![(b, B)]);
    assert_eq!(ws.close_pane(b), Closed::Window);
    assert!(ws.is_empty());
    assert_eq!(ws.close_pane(b), Closed::NotFound);
}

#[test]
fn closing_the_first_child_focuses_the_siblings_first_pane() {
    let (mut ws, a, b, _d) = three();
    ws.focus_pane(a);
    ws.close_pane(a);
    ok(&ws);
    assert_eq!(ws.focused(), Some(b));
}

#[test]
fn closing_a_second_child_focuses_the_last_pane_of_the_sibling() {
    use halyard_workspace::Node;
    // (a1 | a2) | b, built directly.
    let (a1, a2, b) = (PaneId(1), PaneId(2), PaneId(3));
    let mut root = Node::Split {
        axis: Axis::Horizontal,
        ratio: 0.5,
        first: Box::new(Node::Split {
            axis: Axis::Horizontal,
            ratio: 0.5,
            first: Box::new(Node::Pane(a1)),
            second: Box::new(Node::Pane(a2)),
        }),
        second: Box::new(Node::Pane(b)),
    };
    assert_eq!(root.remove_pane(b), Some(a2), "a2 touches b; a1 does not");
    assert_eq!(root.panes(), vec![a1, a2]);
}

#[test]
fn closing_an_earlier_tab_keeps_the_active_tab() {
    let (mut ws, a) = Workspace::new();
    ws.new_tab();
    let c = ws.new_tab();
    ws.new_tab();
    ws.select_tab(2);
    assert_eq!(ws.focused(), Some(c));
    assert_eq!(ws.close_pane(a), Closed::Tab);
    assert_eq!(ws.active_index(), 1);
    assert_eq!(ws.focused(), Some(c), "still on the same tab");
    ok(&ws);
}

#[test]
fn closing_the_last_pane_of_a_tab_closes_the_tab() {
    let (mut ws, a) = Workspace::new();
    let t2 = ws.new_tab();
    let t3 = ws.new_tab();
    assert_eq!(ws.active_index(), 2);
    ws.select_tab(1);
    assert_eq!(ws.close_pane(t2), Closed::Tab);
    ok(&ws);
    assert_eq!(ws.tabs().len(), 2);
    assert_eq!(ws.focused(), Some(t3), "the next tab slides into place");
    assert_eq!(ws.close_pane(t3), Closed::Tab);
    assert_eq!(ws.focused(), Some(a));
    ok(&ws);
}

#[test]
fn focus_moves_by_geometry() {
    let (mut ws, a, b, d) = three();
    // From d (bottom right): up is b, left is a, right and down are nothing.
    assert_eq!(ws.focus_direction(Direction::Up, B, GAP), Some(b));
    assert_eq!(ws.focus_direction(Direction::Up, B, GAP), None);
    assert_eq!(ws.focus_direction(Direction::Down, B, GAP), Some(d));
    assert_eq!(ws.focus_direction(Direction::Right, B, GAP), None);
    assert_eq!(ws.focus_direction(Direction::Left, B, GAP), Some(a));
    // From a, right goes to the pane overlapping most: equal, so the one
    // whose center is closest; both are equally close, so the first wins.
    let r = ws.focus_direction(Direction::Right, B, GAP);
    assert!(r == Some(b) || r == Some(d));
    ok(&ws);
}

#[test]
fn focus_prefers_the_larger_overlap() {
    let (mut ws, a, b, d) = three();
    // Move the right column's divider up so the lower pane (d) is taller.
    let p = ws.active().root.path_to(d).unwrap();
    assert!(ws.drag_divider(&p[..1], 150.0, B, GAP, MIN));
    ws.focus_pane(a);
    assert_eq!(ws.focus_direction(Direction::Right, B, GAP), Some(d), "d overlaps a more");
    // And the other way round.
    assert!(ws.drag_divider(&p[..1], 450.0, B, GAP, MIN));
    ws.focus_pane(a);
    assert_eq!(ws.focus_direction(Direction::Right, B, GAP), Some(b));
}

#[test]
fn keyboard_resize_moves_the_nearest_divider_and_respects_minimums() {
    let (mut ws, a, _b, d) = three();
    let ratio = |ws: &Workspace| ws.active().root.ratios();
    ws.focus_pane(a);
    assert!(ws.resize_focused(Direction::Right, 50.0, B, GAP, MIN));
    let r = ratio(&ws);
    assert!((r[0] - 548.0 / 996.0).abs() < 1e-3, "{r:?}");
    assert!(ws.resize_focused(Direction::Left, 50.0, B, GAP, MIN));
    assert!((ratio(&ws)[0] - 0.5).abs() < 1e-3);
    // Vertical resize from d moves the c/d divider, not the root.
    ws.focus_pane(d);
    assert!(ws.resize_focused(Direction::Up, 30.0, B, GAP, MIN));
    let r = ratio(&ws);
    assert!((r[0] - 0.5).abs() < 1e-3 && r[1] < 0.5, "{r:?}");
    // A pane never shrinks below the minimum.
    for _ in 0..100 {
        ws.resize_focused(Direction::Up, 30.0, B, GAP, MIN);
    }
    let rects = ws.active().layout(B, GAP);
    assert!(rects.iter().all(|(_, r)| r.h >= MIN.h - 1.0 && r.w >= MIN.w - 1.0), "{rects:?}");
    ok(&ws);
    // No vertical split above a: up does nothing.
    ws.focus_pane(a);
    assert!(!ws.resize_focused(Direction::Up, 30.0, B, GAP, MIN));
}

#[test]
fn dragging_a_divider_clamps_to_nested_minimums() {
    let (mut ws, _a, _b, _d) = three();
    let divs = ws.active().dividers(B, GAP);
    assert_eq!(divs.len(), 2);
    assert_eq!(divs[0].path, Vec::<bool>::new());
    assert_eq!(divs[0].axis, Axis::Horizontal);
    assert_eq!(divs[0].rect, Rect::new(498.0, 0.0, 4.0, 600.0));
    assert_eq!(divs[1].path, vec![true]);
    // Drag the root divider to x = 300.
    assert!(ws.drag_divider(&[], 300.0, B, GAP, MIN));
    let a_rect = ws.active().layout(B, GAP)[0].1;
    assert!((a_rect.w - 298.0).abs() <= 1.0, "{a_rect:?}");
    // Past the left edge: clamped to the minimum width.
    ws.drag_divider(&[], -500.0, B, GAP, MIN);
    assert!((ws.active().layout(B, GAP)[0].1.w - MIN.w).abs() <= 1.0);
    // Past the right edge: the right column keeps its minimum.
    ws.drag_divider(&[], 5000.0, B, GAP, MIN);
    let right = ws.active().layout(B, GAP)[1].1;
    assert!(right.w >= MIN.w - 1.0, "{right:?}");
    ok(&ws);
    // Min extents add up along the axis and take the max across it.
    let root = &ws.active().root;
    assert_eq!(root.min_extent(Axis::Horizontal, MIN, GAP), 60.0 + 4.0 + 60.0);
    assert_eq!(root.min_extent(Axis::Vertical, MIN, GAP), 40.0 + 4.0 + 40.0);
}

#[test]
fn zoom_shows_only_the_focused_pane() {
    let (mut ws, _a, _b, d) = three();
    assert!(ws.toggle_zoom());
    assert_eq!(ws.active().layout(B, GAP), vec![(d, B)]);
    assert!(ws.active().dividers(B, GAP).is_empty());
    // Splitting or moving focus leaves focus mode.
    ws.focus_direction(Direction::Left, B, GAP);
    assert!(!ws.active().zoomed);
    ws.toggle_zoom();
    ws.split(Axis::Horizontal);
    assert!(!ws.active().zoomed);
    // A single pane never zooms.
    let (mut one, _) = Workspace::new();
    assert!(!one.toggle_zoom());
    ok(&ws);
}

#[test]
fn closing_down_to_one_pane_leaves_focus_mode() {
    let (mut ws, a) = Workspace::new();
    let b = ws.split(Axis::Horizontal).unwrap();
    ws.toggle_zoom();
    ws.close_pane(a);
    assert!(!ws.active().zoomed);
    assert_eq!(ws.focused(), Some(b));
}

#[test]
fn tabs_move_rename_and_cycle() {
    let (mut ws, a) = Workspace::new();
    let b = ws.new_tab();
    let c = ws.new_tab();
    assert_eq!(ws.focused(), Some(c));
    ws.next_tab();
    assert_eq!(ws.focused(), Some(a), "wraps");
    ws.prev_tab();
    assert_eq!(ws.focused(), Some(c));
    // Move the active tab (c) to the front; it stays active.
    assert!(ws.move_tab(2, 0));
    assert_eq!(ws.active_index(), 0);
    assert_eq!(ws.focused(), Some(c));
    let order: Vec<PaneId> = ws.tabs().iter().map(|t| t.focused).collect();
    assert_eq!(order, vec![c, a, b]);
    assert!(!ws.move_tab(0, 9));
    assert!(ws.rename_tab(1, Some("  logs\u{7}\n ")));
    assert_eq!(ws.tabs()[1].name.as_deref(), Some("logs"));
    assert!(ws.rename_tab(1, Some("   ")));
    assert_eq!(ws.tabs()[1].name, None, "blank clears");
    let long = "x".repeat(1000);
    ws.rename_tab(0, Some(&long));
    assert_eq!(ws.tabs()[0].name.as_ref().unwrap().len(), 256);
    ok(&ws);
}

#[test]
fn new_tab_goes_after_the_active_one() {
    let (mut ws, a) = Workspace::new();
    let b = ws.new_tab();
    ws.select_tab(0);
    let c = ws.new_tab();
    let order: Vec<PaneId> = ws.tabs().iter().map(|t| t.focused).collect();
    assert_eq!(order, vec![a, c, b]);
}

/// Random operations keep every invariant.
#[test]
fn random_operations_keep_invariants() {
    let mut seed: u64 = 0x2545_f491_4f6c_dd1d;
    let mut rnd = move |n: u64| {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        seed % n
    };
    let (mut ws, _) = Workspace::new();
    for step in 0..5000 {
        if ws.is_empty() {
            ws = Workspace::new().0;
        }
        let dirs = [Direction::Left, Direction::Right, Direction::Up, Direction::Down];
        match rnd(10) {
            0 | 1 => {
                ws.split(if rnd(2) == 0 { Axis::Horizontal } else { Axis::Vertical });
            }
            2 => {
                let panes = ws.all_panes();
                let p = panes[usize::try_from(rnd(panes.len() as u64)).unwrap()];
                ws.close_pane(p);
            }
            3 => {
                ws.focus_direction(dirs[usize::try_from(rnd(4)).unwrap()], B, GAP);
            }
            4 => {
                ws.resize_focused(dirs[usize::try_from(rnd(4)).unwrap()], 37.0, B, GAP, MIN);
            }
            5 => {
                ws.toggle_zoom();
            }
            6 => {
                ws.new_tab();
            }
            7 => {
                let n = ws.tabs().len() as u64;
                ws.move_tab(usize::try_from(rnd(n)).unwrap(), usize::try_from(rnd(n)).unwrap());
            }
            8 => ws.next_tab(),
            _ => {
                let panes = ws.all_panes();
                ws.focus_pane(panes[usize::try_from(rnd(panes.len() as u64)).unwrap()]);
            }
        }
        if !ws.is_empty() {
            ws.check().unwrap_or_else(|e| panic!("step {step}: {e}"));
            // Visible panes never overlap and stay inside the bounds.
            let rects = ws.active().layout(B, GAP);
            for (i, (_, r)) in rects.iter().enumerate() {
                assert!(r.x >= 0.0 && r.y >= 0.0 && r.right() <= B.w && r.bottom() <= B.h);
                for (_, q) in &rects[i + 1..] {
                    let ox = r.right().min(q.right()) - r.x.max(q.x);
                    let oy = r.bottom().min(q.bottom()) - r.y.max(q.y);
                    assert!(ox <= 0.0 || oy <= 0.0, "step {step}: overlap {r:?} {q:?}");
                }
            }
        }
    }
}

#[test]
fn node_paths_round_trip() {
    let (ws, a, b, d) = three();
    let root = &ws.active().root;
    for id in [a, b, d] {
        let p = root.path_to(id).unwrap();
        assert_eq!(root.at(&p), Some(&Node::Pane(id)));
    }
    assert_eq!(root.path_to(PaneId(99)), None);
    assert_eq!(root.depth(), 2);
    assert_eq!(root.len(), 3);
}
