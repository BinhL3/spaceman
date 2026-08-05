use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Axis {
    #[default]
    Horizontal,
    Vertical,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct Rect {
    pub x: i32,
    pub y: i32,
    pub width: i32,
    pub height: i32,
}

impl Rect {
    #[must_use]
    pub fn approximately_equals(self, other: Self, tolerance: i32) -> bool {
        (self.x - other.x).abs() <= tolerance
            && (self.y - other.y).abs() <= tolerance
            && (self.width - other.width).abs() <= tolerance
            && (self.height - other.height).abs() <= tolerance
    }

    #[must_use]
    pub const fn contains(self, x: i32, y: i32) -> bool {
        x >= self.x && x < self.x + self.width && y >= self.y && y < self.y + self.height
    }

    #[must_use]
    pub const fn contains_center(self, other: Self) -> bool {
        self.contains(other.x + other.width / 2, other.y + other.height / 2)
    }
}

#[must_use]
pub fn parked_frame(output: Rect, displays: &[Rect], window: Rect) -> Rect {
    let x_offset = output.width / 10;
    let y_offset = output.height / 10;
    let left_score = corner_score(
        displays,
        [
            (output.x - 2, output.y + output.height - y_offset),
            (output.x + x_offset, output.y + output.height + 2),
            (output.x - 2, output.y + output.height + 2),
        ],
    );
    let right_score = corner_score(
        displays,
        [
            (
                output.x + output.width + 2,
                output.y + output.height - y_offset,
            ),
            (
                output.x + output.width - x_offset,
                output.y + output.height + 2,
            ),
            (output.x + output.width + 2, output.y + output.height + 2),
        ],
    );
    Rect {
        x: if left_score < right_score {
            output.x - window.width + 1
        } else {
            output.x + output.width - 1
        },
        y: output.y + (output.height - window.height).max(0),
        ..window
    }
}

fn corner_score(displays: &[Rect], points: [(i32, i32); 3]) -> usize {
    displays
        .iter()
        .map(|display| {
            usize::from(display.contains(points[0].0, points[0].1))
                + usize::from(display.contains(points[1].0, points[1].1))
                + 10 * usize::from(display.contains(points[2].0, points[2].1))
        })
        .sum()
}

#[must_use]
pub fn partition(
    output: Rect,
    axis: Axis,
    slots: &[crate::model::Slot],
) -> Vec<crate::reconcile::Placement> {
    if slots.is_empty() {
        return Vec::new();
    }

    let total_weight: u64 = slots.iter().map(|slot| u64::from(slot.weight)).sum();
    let extent = match axis {
        Axis::Horizontal => output.width,
        Axis::Vertical => output.height,
    };
    let mut cursor = 0_i32;

    slots
        .iter()
        .enumerate()
        .map(|(index, slot)| {
            let end = if index + 1 == slots.len() {
                extent
            } else {
                let cumulative_weight: u64 = slots[..=index]
                    .iter()
                    .map(|slot| u64::from(slot.weight))
                    .sum();
                (i64::from(extent) * cumulative_weight as i64 / total_weight as i64) as i32
            };
            let length = end - cursor;
            let frame = match axis {
                Axis::Horizontal => Rect {
                    x: output.x + cursor,
                    width: length,
                    ..output
                },
                Axis::Vertical => Rect {
                    y: output.y + cursor,
                    height: length,
                    ..output
                },
            };
            cursor = end;
            crate::reconcile::Placement {
                slot: slot.id,
                window: slot.bound_window().cloned(),
                frame,
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{AppInstanceId, BindingState, PhysicalWindowId, Slot, SlotId};

    fn slot(id: u64, weight: u32) -> Slot {
        Slot {
            id: SlotId(id),
            weight,
            binding: BindingState::Bound(PhysicalWindowId {
                app: AppInstanceId {
                    pid: 1,
                    launched_at: 1,
                },
                window_id: id as u32,
            }),
        }
    }

    #[test]
    fn partitions_without_gaps_or_rounding_loss() {
        let placements = partition(
            Rect {
                x: 10,
                y: 20,
                width: 101,
                height: 50,
            },
            Axis::Horizontal,
            &[slot(1, 1), slot(2, 1), slot(3, 1)],
        );

        assert_eq!(placements[0].frame.width, 33);
        assert_eq!(placements[1].frame.x, 43);
        assert_eq!(placements[1].frame.width, 34);
        assert_eq!(placements[2].frame.x, 77);
        assert_eq!(placements[2].frame.width, 34);
        assert_eq!(placements[2].frame.x + placements[2].frame.width, 111);
    }

    #[test]
    fn unresolved_slots_reserve_their_layout_position() {
        let mut unresolved = slot(2, 1);
        unresolved.binding = BindingState::Unresolved {
            previous: unresolved.bound_window().unwrap().clone(),
            candidates: Vec::new(),
            since: 4,
        };

        let placements = partition(
            Rect {
                width: 100,
                height: 50,
                ..Rect::default()
            },
            Axis::Horizontal,
            &[slot(1, 1), unresolved],
        );
        assert_eq!(placements.len(), 2);
        assert_eq!(placements[0].frame.width, 50);
        assert_eq!(placements[1].frame.width, 50);
        assert_eq!(placements[1].window, None);
    }

    #[test]
    fn parks_beyond_outer_edges_without_requesting_a_clamped_position() {
        let left = Rect {
            width: 1000,
            height: 800,
            ..Rect::default()
        };
        let right = Rect {
            x: 1000,
            width: 1000,
            height: 800,
            ..Rect::default()
        };
        let window = Rect {
            width: 400,
            height: 300,
            ..Rect::default()
        };

        assert_eq!(parked_frame(left, &[left, right], window).x, -399);
        assert_eq!(parked_frame(right, &[left, right], window).x, 1999);
        assert_eq!(parked_frame(left, &[left, right], window).y, 500);
    }
}
