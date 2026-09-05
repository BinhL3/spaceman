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
        self.position_approximately_equals(other, tolerance)
            && (self.width - other.width).abs() <= tolerance
            && (self.height - other.height).abs() <= tolerance
    }

    /// Position-only tolerance comparison; sizes may legitimately differ when
    /// an application constrains its own dimensions.
    #[must_use]
    pub fn position_approximately_equals(self, other: Self, tolerance: i32) -> bool {
        (self.x - other.x).abs() <= tolerance && (self.y - other.y).abs() <= tolerance
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
    // Keep a one-pixel strip at an actual display edge so macOS does not
    // clamp the window back on-screen. Score whole rectangles, not corners:
    // the nearest output may have a neighbor on both sides.
    std::iter::once(&output)
        .chain(displays)
        .flat_map(|display| {
            [true, false].map(|right| {
                let strip = window.width.clamp(0, 2) / 2;
                let frame = Rect {
                    x: if right {
                        display.x + display.width - strip
                    } else {
                        display.x - window.width + strip
                    },
                    y: display.y + (display.height - window.height).max(0),
                    ..window
                };
                let distance = (i64::from(display.x) - i64::from(output.x)).abs()
                    + (i64::from(display.y) - i64::from(output.y)).abs();
                (frame, distance)
            })
        })
        .filter(|(frame, _)| {
            displays.iter().all(|display| {
                let width = (i64::from(frame.x) + i64::from(frame.width))
                    .min(i64::from(display.x) + i64::from(display.width))
                    - i64::from(frame.x.max(display.x));
                let height = (i64::from(frame.y) + i64::from(frame.height))
                    .min(i64::from(display.y) + i64::from(display.height))
                    - i64::from(frame.y.max(display.y));
                (width <= 1 || height <= 0) && !display.contains_center(*frame)
            })
        })
        .min_by_key(|(_, distance)| *distance)
        .map(|(frame, _)| frame)
        .expect("an outer display edge always provides a parking position")
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

    #[test]
    fn parking_avoids_neighbors_in_middle_and_staggered_display_topologies() {
        for offsets in [[0, 0, 0], [-400, 0, 300], [0, 900, 0]] {
            let displays = offsets
                .into_iter()
                .enumerate()
                .map(|(index, y)| Rect {
                    x: (index as i32 - 1) * 1000,
                    y,
                    width: 1000,
                    height: 800,
                })
                .collect::<Vec<_>>();
            for output in &displays {
                for width in [100, 400, 1200, 3000] {
                    for height in [100, 800, 1500] {
                        let window = Rect {
                            width,
                            height,
                            ..Rect::default()
                        };
                        let parked = parked_frame(*output, &displays, window);
                        for display in &displays {
                            let overlap_width = (parked.x + parked.width)
                                .min(display.x + display.width)
                                - parked.x.max(display.x);
                            let overlap_height = (parked.y + parked.height)
                                .min(display.y + display.height)
                                - parked.y.max(display.y);
                            assert!(
                                overlap_width <= 1 || overlap_height <= 0,
                                "parked {parked:?} overlaps {display:?}"
                            );
                            assert!(!display.contains_center(parked));
                        }
                    }
                }
            }
        }
    }
}
