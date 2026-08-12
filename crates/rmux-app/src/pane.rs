//! The splittable pane tree — a close port of Zed's `workspace::pane_group`
//! geometry, stripped of its domain coupling (project/collab/settings/theme
//! persistence) per ADR-0002. A leaf is a `TerminalView`; the tree arranges
//! leaves along horizontal/vertical axes and supports drag-to-resize.
//!
//! What was dropped vs. the upstream `pane_group.rs`: the `Project`-aware
//! `Pane`/`Item` model, the collaboration `PaneLeaderDecorator`/follower path,
//! `WorkspaceSettings` reads, `serialize_workspace` persistence, and theme
//! colors (hardcoded here to match the terminal skin). The split-tree algebra
//! and the flex-based resize element are carried over near-verbatim.

use gpui::{App, Axis, Bounds, Entity, IntoElement, Pixels, Window, div, prelude::*, rgb};
use parking_lot::Mutex;
use std::sync::Arc;

use crate::terminal::TerminalView;

/// A leaf in the pane tree. For now every pane is a terminal; the
/// remote-session tab trait (ADR-0002) replaces this when non-terminal tabs
/// arrive.
pub type Pane = TerminalView;

const DIVIDER_COLOR: u32 = 0x2a2621;
const ACTIVE_BORDER: u32 = 0x5c5346;

#[derive(Clone, Copy, PartialEq)]
pub enum SplitDirection {
    Up,
    Down,
    Left,
    Right,
}

impl SplitDirection {
    fn axis(&self) -> Axis {
        match self {
            Self::Up | Self::Down => Axis::Vertical,
            Self::Left | Self::Right => Axis::Horizontal,
        }
    }

    fn increasing(&self) -> bool {
        matches!(self, Self::Down | Self::Right)
    }
}

/// A tree of panes arranged by splits. A single-pane tree is just one pane.
pub struct PaneGroup {
    root: Member,
}

impl PaneGroup {
    pub fn new(pane: Entity<Pane>) -> Self {
        Self {
            root: Member::Pane(pane),
        }
    }

    /// Split `old_pane` in `direction`, inserting `new_pane` beside it.
    pub fn split(
        &mut self,
        old_pane: &Entity<Pane>,
        new_pane: &Entity<Pane>,
        direction: SplitDirection,
    ) {
        match &mut self.root {
            Member::Pane(pane) if pane == old_pane => {
                self.root = Member::new_axis(old_pane.clone(), new_pane.clone(), direction);
            }
            Member::Pane(_) => {}
            Member::Axis(axis) => {
                axis.split(old_pane, new_pane, direction);
            }
        }
    }

    /// Remove `pane`. Returns true if it was found and removed. When an axis
    /// collapses to a single member, it is replaced by that member.
    pub fn remove(&mut self, pane: &Entity<Pane>) -> bool {
        match &mut self.root {
            Member::Pane(_) => false,
            Member::Axis(axis) => match axis.remove(pane) {
                Removed::NotFound => false,
                Removed::Removed => true,
                Removed::Collapse(member) => {
                    self.root = member;
                    true
                }
            },
        }
    }

    pub fn panes(&self) -> Vec<&Entity<Pane>> {
        let mut panes = Vec::new();
        self.root.collect_panes(&mut panes);
        panes
    }

    pub fn first_pane(&self) -> Entity<Pane> {
        self.root.first_pane()
    }

    pub fn render(
        &self,
        active: &Entity<Pane>,
        window: &mut Window,
        cx: &mut App,
    ) -> impl IntoElement {
        self.root.render(0, active, window, cx)
    }
}

#[derive(Clone)]
enum Member {
    Axis(PaneAxis),
    Pane(Entity<Pane>),
}

/// Outcome of removing a pane from an axis.
enum Removed {
    NotFound,
    Removed,
    /// The axis now holds a single member; the caller should replace the axis
    /// with this member.
    Collapse(Member),
}

impl Member {
    fn new_axis(
        old_pane: Entity<Pane>,
        new_pane: Entity<Pane>,
        direction: SplitDirection,
    ) -> Self {
        let members = if direction.increasing() {
            vec![Member::Pane(old_pane), Member::Pane(new_pane)]
        } else {
            vec![Member::Pane(new_pane), Member::Pane(old_pane)]
        };
        Member::Axis(PaneAxis::new(direction.axis(), members))
    }

    fn first_pane(&self) -> Entity<Pane> {
        match self {
            Member::Axis(axis) => axis.members[0].first_pane(),
            Member::Pane(pane) => pane.clone(),
        }
    }

    fn collect_panes<'a>(&'a self, panes: &mut Vec<&'a Entity<Pane>>) {
        match self {
            Member::Axis(axis) => {
                for member in &axis.members {
                    member.collect_panes(panes);
                }
            }
            Member::Pane(pane) => panes.push(pane),
        }
    }

    fn render(
        &self,
        basis: usize,
        active: &Entity<Pane>,
        window: &mut Window,
        cx: &mut App,
    ) -> gpui::AnyElement {
        match self {
            Member::Pane(pane) => {
                let is_active = pane == active;
                div()
                    .relative()
                    .flex_1()
                    .size_full()
                    .child(pane.clone())
                    .when(is_active, |this| {
                        this.child(
                            div()
                                .absolute()
                                .size_full()
                                .left_0()
                                .top_0()
                                .border_1()
                                .border_color(rgb(ACTIVE_BORDER)),
                        )
                    })
                    .into_any_element()
            }
            Member::Axis(axis) => axis.render(basis, active, window, cx),
        }
    }
}

#[derive(Clone)]
struct PaneAxis {
    axis: Axis,
    members: Vec<Member>,
    /// Relative sizes of the members, in flex units (mean 1.0). Shared with the
    /// layout element, which mutates it during a drag-resize.
    flexes: Arc<Mutex<Vec<f32>>>,
    /// Last laid-out bounds per member, written by the element during prepaint.
    bounding_boxes: Arc<Mutex<Vec<Option<Bounds<Pixels>>>>>,
}

impl PaneAxis {
    fn new(axis: Axis, members: Vec<Member>) -> Self {
        let flexes = Arc::new(Mutex::new(vec![1.; members.len()]));
        let bounding_boxes = Arc::new(Mutex::new(vec![None; members.len()]));
        Self {
            axis,
            members,
            flexes,
            bounding_boxes,
        }
    }

    fn split(
        &mut self,
        old_pane: &Entity<Pane>,
        new_pane: &Entity<Pane>,
        direction: SplitDirection,
    ) -> bool {
        for (mut idx, member) in self.members.iter_mut().enumerate() {
            match member {
                Member::Axis(axis) => {
                    if axis.split(old_pane, new_pane, direction) {
                        return true;
                    }
                }
                Member::Pane(pane) => {
                    if pane == old_pane {
                        if direction.axis() == self.axis {
                            if direction.increasing() {
                                idx += 1;
                            }
                            self.insert_pane(idx, new_pane);
                        } else {
                            *member =
                                Member::new_axis(old_pane.clone(), new_pane.clone(), direction);
                        }
                        return true;
                    }
                }
            }
        }
        false
    }

    fn insert_pane(&mut self, idx: usize, new_pane: &Entity<Pane>) {
        self.members.insert(idx, Member::Pane(new_pane.clone()));
        *self.flexes.lock() = vec![1.; self.members.len()];
    }

    fn remove(&mut self, pane_to_remove: &Entity<Pane>) -> Removed {
        let mut remove_idx = None;
        for (idx, member) in self.members.iter_mut().enumerate() {
            match member {
                Member::Axis(axis) => match axis.remove(pane_to_remove) {
                    Removed::NotFound => {}
                    Removed::Removed => return self.after_removal(),
                    Removed::Collapse(collapsed) => {
                        *member = collapsed;
                        return self.after_removal();
                    }
                },
                Member::Pane(pane) => {
                    if pane == pane_to_remove {
                        remove_idx = Some(idx);
                        break;
                    }
                }
            }
        }

        let Some(idx) = remove_idx else {
            return Removed::NotFound;
        };
        self.members.remove(idx);
        self.after_removal()
    }

    /// Normalise flexes after a structural change and collapse a
    /// single-member axis into that member.
    fn after_removal(&mut self) -> Removed {
        *self.flexes.lock() = vec![1.; self.members.len()];
        if self.members.len() == 1 {
            Removed::Collapse(self.members.pop().expect("len checked"))
        } else {
            Removed::Removed
        }
    }

    fn render(
        &self,
        basis: usize,
        active: &Entity<Pane>,
        window: &mut Window,
        cx: &mut App,
    ) -> gpui::AnyElement {
        let children = self
            .members
            .iter()
            .enumerate()
            .map(|(ix, member)| member.render((basis + ix + 1) * 10, active, window, cx))
            .collect::<Vec<_>>();

        element::pane_axis(self.axis, basis, self.flexes.clone(), self.bounding_boxes.clone())
            .children(children)
            .into_any_element()
    }
}

/// The custom flex element that lays out an axis of panes and hosts the
/// drag-to-resize divider handles. Ported from `pane_group::element`.
mod element {
    use std::cell::RefCell;
    use std::rc::Rc;
    use std::sync::Arc;
    use std::{iter, mem};

    use gpui::{
        Along, AnyElement, App, Axis, Bounds, CursorStyle, Element, ElementId, GlobalElementId,
        Hitbox, HitboxBehavior, InspectorElementId, IntoElement, LayoutId, MouseDownEvent,
        MouseMoveEvent, MouseUpEvent, ParentElement, Pixels, Point, Size, Style, Window, fill, px,
        relative, size,
    };
    use parking_lot::Mutex;

    use super::DIVIDER_COLOR;

    const HANDLE_HITBOX_SIZE: f32 = 4.0;
    const DIVIDER_SIZE: f32 = 1.0;
    const HORIZONTAL_MIN_SIZE: f32 = 80.;
    const VERTICAL_MIN_SIZE: f32 = 100.;

    pub(super) fn pane_axis(
        axis: Axis,
        basis: usize,
        flexes: Arc<Mutex<Vec<f32>>>,
        bounding_boxes: Arc<Mutex<Vec<Option<Bounds<Pixels>>>>>,
    ) -> PaneAxisElement {
        PaneAxisElement {
            axis,
            basis,
            flexes,
            bounding_boxes,
            children: Vec::new(),
        }
    }

    pub struct PaneAxisElement {
        axis: Axis,
        basis: usize,
        /// Flex weights (mean 1.0), e.g. `[1.33, 1.0, 1.0]` instead of
        /// `40%, 30%, 30%`.
        flexes: Arc<Mutex<Vec<f32>>>,
        bounding_boxes: Arc<Mutex<Vec<Option<Bounds<Pixels>>>>>,
        children: Vec<AnyElement>,
    }

    pub struct PaneAxisLayout {
        dragged_handle: Rc<RefCell<Option<usize>>>,
        children: Vec<PaneAxisChildLayout>,
    }

    struct PaneAxisChildLayout {
        bounds: Bounds<Pixels>,
        element: AnyElement,
        handle: Option<PaneAxisHandleLayout>,
    }

    struct PaneAxisHandleLayout {
        hitbox: Hitbox,
        divider_bounds: Bounds<Pixels>,
    }

    impl PaneAxisElement {
        fn compute_resize(
            flexes: &Arc<Mutex<Vec<f32>>>,
            e: &MouseMoveEvent,
            ix: usize,
            axis: Axis,
            child_start: Point<Pixels>,
            container_size: Size<Pixels>,
            window: &mut Window,
        ) {
            let min_size = match axis {
                Axis::Horizontal => px(HORIZONTAL_MIN_SIZE),
                Axis::Vertical => px(VERTICAL_MIN_SIZE),
            };
            let mut flexes = flexes.lock();
            debug_assert!(flex_values_in_bounds(flexes.as_slice()));

            let size = move |ix, flexes: &[f32]| {
                container_size.along(axis) * (flexes[ix] / flexes.len() as f32)
            };

            // Don't shrink an element already at (or below) the minimum size.
            if min_size - px(1.) > size(ix, flexes.as_slice()) {
                return;
            }

            // The pixel delta this event needs to distribute.
            let mut proposed_current_pixel_change =
                (e.position - child_start).along(axis) - size(ix, flexes.as_slice());

            let flex_changes = |pixel_dx, target_ix, next: isize, flexes: &[f32]| {
                let flex_change = pixel_dx / container_size.along(axis);
                let current_target_flex = flexes[target_ix] + flex_change;
                let next_target_flex = flexes[(target_ix as isize + next) as usize] - flex_change;
                (current_target_flex, next_target_flex)
            };

            // Successor indices in the drag direction.
            let mut successors = iter::from_fn({
                let forward = proposed_current_pixel_change > px(0.);
                let mut ix_offset = 0;
                let len = flexes.len();
                move || {
                    let result = if forward {
                        (ix + 1 + ix_offset < len).then(|| ix + ix_offset)
                    } else {
                        (ix as isize - ix_offset as isize >= 0).then(|| ix - ix_offset)
                    };
                    ix_offset += 1;
                    result
                }
            });

            while proposed_current_pixel_change.abs() > px(0.) {
                let Some(current_ix) = successors.next() else {
                    break;
                };

                let next_target_size = Pixels::max(
                    size(current_ix + 1, flexes.as_slice()) - proposed_current_pixel_change,
                    min_size,
                );

                let current_target_size = Pixels::max(
                    size(current_ix, flexes.as_slice()) + size(current_ix + 1, flexes.as_slice())
                        - next_target_size,
                    min_size,
                );

                let current_pixel_change =
                    current_target_size - size(current_ix, flexes.as_slice());

                let (current_target_flex, next_target_flex) =
                    flex_changes(current_pixel_change, current_ix, 1, flexes.as_slice());

                flexes[current_ix] = current_target_flex;
                flexes[current_ix + 1] = next_target_flex;

                proposed_current_pixel_change -= current_pixel_change;
            }

            window.refresh();
        }

        fn layout_handle(
            axis: Axis,
            pane_bounds: Bounds<Pixels>,
            window: &mut Window,
        ) -> PaneAxisHandleLayout {
            let handle_bounds = Bounds {
                origin: pane_bounds.origin.apply_along(axis, |origin| {
                    origin + pane_bounds.size.along(axis) - px(HANDLE_HITBOX_SIZE / 2.)
                }),
                size: pane_bounds
                    .size
                    .apply_along(axis, |_| px(HANDLE_HITBOX_SIZE)),
            };
            let divider_bounds = Bounds {
                origin: pane_bounds
                    .origin
                    .apply_along(axis, |origin| origin + pane_bounds.size.along(axis)),
                size: pane_bounds.size.apply_along(axis, |_| px(DIVIDER_SIZE)),
            };

            PaneAxisHandleLayout {
                hitbox: window.insert_hitbox(handle_bounds, HitboxBehavior::BlockMouse),
                divider_bounds,
            }
        }
    }

    impl IntoElement for PaneAxisElement {
        type Element = Self;

        fn into_element(self) -> Self::Element {
            self
        }
    }

    impl Element for PaneAxisElement {
        type RequestLayoutState = ();
        type PrepaintState = PaneAxisLayout;

        fn id(&self) -> Option<ElementId> {
            Some(self.basis.into())
        }

        fn source_location(&self) -> Option<&'static core::panic::Location<'static>> {
            None
        }

        fn request_layout(
            &mut self,
            _global_id: Option<&GlobalElementId>,
            _inspector_id: Option<&InspectorElementId>,
            window: &mut Window,
            cx: &mut App,
        ) -> (LayoutId, Self::RequestLayoutState) {
            let style = Style {
                flex_grow: 1.,
                flex_shrink: 1.,
                flex_basis: relative(0.).into(),
                size: size(relative(1.).into(), relative(1.).into()),
                ..Style::default()
            };
            (window.request_layout(style, None, cx), ())
        }

        fn prepaint(
            &mut self,
            global_id: Option<&GlobalElementId>,
            _inspector_id: Option<&InspectorElementId>,
            bounds: Bounds<Pixels>,
            _state: &mut Self::RequestLayoutState,
            window: &mut Window,
            cx: &mut App,
        ) -> PaneAxisLayout {
            let dragged_handle = window.with_element_state::<Rc<RefCell<Option<usize>>>, _>(
                global_id.unwrap(),
                |state, _cx| {
                    let state = state.unwrap_or_else(|| Rc::new(RefCell::new(None)));
                    (state.clone(), state)
                },
            );
            let flexes = self.flexes.lock().clone();
            let len = self.children.len();
            debug_assert!(flexes.len() == len);
            debug_assert!(flex_values_in_bounds(flexes.as_slice()));

            let total_flex = len as f32;
            let mut origin = bounds.origin;
            let space_per_flex = bounds.size.along(self.axis) / total_flex;

            let mut bounding_boxes = self.bounding_boxes.lock();
            bounding_boxes.clear();

            let mut layout = PaneAxisLayout {
                dragged_handle,
                children: Vec::new(),
            };
            for (ix, mut child) in mem::take(&mut self.children).into_iter().enumerate() {
                let child_flex = flexes[ix];

                let child_size = bounds
                    .size
                    .apply_along(self.axis, |_| space_per_flex * child_flex)
                    .map(|d| d.round());

                let child_bounds = Bounds {
                    origin,
                    size: child_size,
                };

                bounding_boxes.push(Some(child_bounds));
                child.layout_as_root(child_size.into(), window, cx);
                child.prepaint_at(origin, window, cx);

                origin = origin.apply_along(self.axis, |val| val + child_size.along(self.axis));

                layout.children.push(PaneAxisChildLayout {
                    bounds: child_bounds,
                    element: child,
                    handle: None,
                })
            }

            for (ix, child_layout) in layout.children.iter_mut().enumerate() {
                if ix < len - 1 {
                    child_layout.handle =
                        Some(Self::layout_handle(self.axis, child_layout.bounds, window));
                }
            }

            layout
        }

        fn paint(
            &mut self,
            _id: Option<&GlobalElementId>,
            _inspector_id: Option<&InspectorElementId>,
            bounds: Bounds<Pixels>,
            _: &mut Self::RequestLayoutState,
            layout: &mut Self::PrepaintState,
            window: &mut Window,
            cx: &mut App,
        ) {
            for child in &mut layout.children {
                child.element.paint(window, cx);
            }

            for (ix, child) in &mut layout.children.iter_mut().enumerate() {
                let Some(handle) = child.handle.as_mut() else {
                    continue;
                };

                let cursor_style = match self.axis {
                    Axis::Vertical => CursorStyle::ResizeRow,
                    Axis::Horizontal => CursorStyle::ResizeColumn,
                };

                if layout
                    .dragged_handle
                    .borrow()
                    .is_some_and(|dragged_ix| dragged_ix == ix)
                {
                    window.set_window_cursor_style(cursor_style);
                } else {
                    window.set_cursor_style(cursor_style, &handle.hitbox);
                }

                window.paint_quad(fill(handle.divider_bounds, gpui::rgb(DIVIDER_COLOR)));

                window.on_mouse_event({
                    let dragged_handle = layout.dragged_handle.clone();
                    let flexes = self.flexes.clone();
                    let handle_hitbox = handle.hitbox.clone();
                    move |e: &MouseDownEvent, phase, window, cx| {
                        if phase.bubble() && handle_hitbox.is_hovered(window) {
                            dragged_handle.replace(Some(ix));
                            if e.click_count >= 2 {
                                let mut borrow = flexes.lock();
                                *borrow = vec![1.; borrow.len()];
                                window.refresh();
                            }
                            cx.stop_propagation();
                        }
                    }
                });
                window.on_mouse_event({
                    let dragged_handle = layout.dragged_handle.clone();
                    let flexes = self.flexes.clone();
                    let child_bounds = child.bounds;
                    let axis = self.axis;
                    move |e: &MouseMoveEvent, phase, window, _cx| {
                        let dragged_handle = dragged_handle.borrow();
                        if phase.bubble() && *dragged_handle == Some(ix) {
                            Self::compute_resize(
                                &flexes,
                                e,
                                ix,
                                axis,
                                child_bounds.origin,
                                bounds.size,
                                window,
                            )
                        }
                    }
                });
            }

            window.on_mouse_event({
                let dragged_handle = layout.dragged_handle.clone();
                move |_: &MouseUpEvent, phase, _window, _cx| {
                    if phase.bubble() {
                        dragged_handle.replace(None);
                    }
                }
            });
        }
    }

    impl ParentElement for PaneAxisElement {
        fn extend(&mut self, elements: impl IntoIterator<Item = AnyElement>) {
            self.children.extend(elements)
        }
    }

    fn flex_values_in_bounds(flexes: &[f32]) -> bool {
        (flexes.iter().copied().sum::<f32>() - flexes.len() as f32).abs() < 0.001
    }
}
