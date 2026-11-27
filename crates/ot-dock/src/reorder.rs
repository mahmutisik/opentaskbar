//! Live reordering math (`DockItemOrdering` in OpenTaskbar, issue #130): where a dragged item would land and how
//! far each neighbour slides aside to open that slot. Positions are along the bar, in any one unit.

/// Live drag timings and the dragged item's look (`MainWindow.LiveDrag` / `MainWindow.DragDrop`).
pub const SLIDE_MS: u32 = 160;
pub const LIFT_MS: u32 = 130;
pub const DRAG_SCALE: f32 = 1.10;
pub const DRAG_LIFT_DIPS: f64 = 4.0;
pub const DRAG_OPACITY: f32 = 0.55;
pub const SETTLE_MS: u32 = 200;

/// Insertion index for a pointer: one past every slot whose middle the pointer passed.
pub fn insertion_index(slots: &[(f32, f32)], pointer: f32) -> usize {
    slots
        .iter()
        .take_while(|(start, size)| pointer > start + size / 2.0)
        .count()
}

/// How far item `item` slides while `dragged` hovers over insertion index `insertion`.
pub fn slide_for(item: usize, dragged: usize, insertion: usize, dragged_size: f32) -> f32 {
    if item == dragged {
        return 0.0;
    }
    let destination = if insertion > dragged {
        insertion - 1
    } else {
        insertion
    };
    if item > dragged && item <= destination {
        -dragged_size
    } else if item < dragged && item >= destination {
        dragged_size
    } else {
        0.0
    }
}

/// Final position of the dragged item in the list once it is removed and inserted again.
pub fn destination(dragged: usize, insertion: usize) -> usize {
    if insertion > dragged {
        insertion - 1
    } else {
        insertion
    }
}

/// What a drop among the dock's apps means: `pinned` apps lead, running-only apps follow.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DropAction {
    /// Nothing changes (dropped where it was, or a running app dropped among running apps).
    None,
    /// A pinned app moves to this index among the pins.
    MovePin(usize),
    /// A running app is pinned at this index among the pins (`PinRunningApp`).
    PinAt(usize),
}

/// `dragged` and `destination` are indexes in the app list (pins first); `pinned` is how many apps are pinned.
pub fn drop_action(dragged: usize, destination: usize, pinned: usize) -> DropAction {
    if dragged < pinned {
        let to = destination.min(pinned - 1);
        if to == dragged {
            DropAction::None
        } else {
            DropAction::MovePin(to)
        }
    } else if destination < pinned || (destination == pinned && dragged != destination) {
        // Dropping the first running app where it already stands changes nothing.
        DropAction::PinAt(destination)
    } else {
        DropAction::None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn slots() -> Vec<(f32, f32)> {
        (0..5).map(|i| (i as f32 * 38.0, 38.0)).collect()
    }

    #[test]
    fn insertion_follows_slot_middles() {
        let s = slots();
        assert_eq!(insertion_index(&s, 0.0), 0);
        assert_eq!(insertion_index(&s, 19.5), 1);
        assert_eq!(insertion_index(&s, 56.0), 1);
        assert_eq!(insertion_index(&s, 1000.0), 5);
    }

    #[test]
    fn neighbours_open_the_landing_slot() {
        // Item 1 dragged to the end: items 2..=4 move left by one slot.
        let slides: Vec<f32> = (0..5).map(|i| slide_for(i, 1, 5, 38.0)).collect();
        assert_eq!(slides, vec![0.0, 0.0, -38.0, -38.0, -38.0]);
        // Item 3 dragged to the front: items 0..=2 move right.
        let slides: Vec<f32> = (0..5).map(|i| slide_for(i, 3, 0, 38.0)).collect();
        assert_eq!(slides, vec![38.0, 38.0, 38.0, 0.0, 0.0]);
        // Hovering over its own slot moves nothing.
        assert!((0..5).all(|i| slide_for(i, 2, 2, 38.0) == 0.0 && slide_for(i, 2, 3, 38.0) == 0.0));
    }

    #[test]
    fn drops_reorder_pins_or_pin_running_apps() {
        // 3 pins (0..3), 2 running apps (3, 4).
        assert_eq!(drop_action(0, 2, 3), DropAction::MovePin(2));
        assert_eq!(drop_action(1, 1, 3), DropAction::None);
        assert_eq!(drop_action(0, 4, 3), DropAction::MovePin(2));
        assert_eq!(drop_action(4, 1, 3), DropAction::PinAt(1));
        assert_eq!(drop_action(3, 3, 3), DropAction::None);
        assert_eq!(drop_action(4, 3, 3), DropAction::PinAt(3));
        assert_eq!(drop_action(3, 4, 3), DropAction::None);
    }
}
