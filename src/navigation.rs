use std::{fmt, str::FromStr};

use crate::{PhysicalWindowId, Rect, WorkspaceId};

#[derive(Clone, Copy, Debug, Eq, PartialEq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Direction {
    Left,
    Down,
    Up,
    Right,
}

impl fmt::Display for Direction {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Left => "left",
            Self::Down => "down",
            Self::Up => "up",
            Self::Right => "right",
        })
    }
}

impl FromStr for Direction {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "left" => Ok(Self::Left),
            "down" => Ok(Self::Down),
            "up" => Ok(Self::Up),
            "right" => Ok(Self::Right),
            _ => Err(format!("invalid direction {value:?}")),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize)]
pub struct NavigationTile {
    pub workspace: WorkspaceId,
    pub window: PhysicalWindowId,
    pub frame: Rect,
}

#[must_use]
pub fn target<'a>(
    tiles: &'a [NavigationTile],
    focused: &PhysicalWindowId,
    direction: Direction,
) -> Option<&'a NavigationTile> {
    let current = tiles.iter().find(|tile| &tile.window == focused)?.frame;
    let current_x = i64::from(current.x) * 2 + i64::from(current.width);
    let current_y = i64::from(current.y) * 2 + i64::from(current.height);
    tiles
        .iter()
        .filter(|tile| &tile.window != focused)
        .filter_map(|tile| {
            let x = i64::from(tile.frame.x) * 2 + i64::from(tile.frame.width);
            let y = i64::from(tile.frame.y) * 2 + i64::from(tile.frame.height);
            let (primary, perpendicular) = match direction {
                Direction::Left => (current_x - x, (current_y - y).abs()),
                Direction::Down => (y - current_y, (current_x - x).abs()),
                Direction::Up => (current_y - y, (current_x - x).abs()),
                Direction::Right => (x - current_x, (current_y - y).abs()),
            };
            (primary > 0 && primary >= perpendicular).then_some(((primary, perpendicular), tile))
        })
        .min_by_key(|(score, _)| *score)
        .map(|(_, tile)| tile)
}

#[cfg(test)]
mod tests {
    use crate::AppInstanceId;

    use super::*;

    fn tile(id: u32, workspace: u8, x: i32, y: i32) -> NavigationTile {
        NavigationTile {
            workspace: WorkspaceId::new(workspace).unwrap(),
            window: PhysicalWindowId {
                app: AppInstanceId {
                    pid: i32::try_from(id).unwrap(),
                    launched_at: 1,
                },
                window_id: id,
            },
            frame: Rect {
                x,
                y,
                width: 500,
                height: 500,
            },
        }
    }

    #[test]
    fn chooses_geometric_neighbors_in_all_directions() {
        let center = tile(1, 1, 500, 500);
        let tiles = vec![
            center.clone(),
            tile(2, 1, 0, 500),
            tile(3, 1, 500, 1000),
            tile(4, 1, 500, 0),
            tile(5, 1, 1000, 500),
        ];

        assert_eq!(
            target(&tiles, &center.window, Direction::Left)
                .unwrap()
                .window
                .window_id,
            2
        );
        assert_eq!(
            target(&tiles, &center.window, Direction::Down)
                .unwrap()
                .window
                .window_id,
            3
        );
        assert_eq!(
            target(&tiles, &center.window, Direction::Up)
                .unwrap()
                .window
                .window_id,
            4
        );
        assert_eq!(
            target(&tiles, &center.window, Direction::Right)
                .unwrap()
                .window
                .window_id,
            5
        );
    }

    #[test]
    fn ignores_small_vertical_offset_between_side_by_side_displays() {
        let left = tile(1, 1, 0, 30);
        let right = tile(2, 6, 2560, 0);
        let tiles = vec![left.clone(), right.clone()];

        assert_eq!(target(&tiles, &left.window, Direction::Right), Some(&right));
        assert_eq!(target(&tiles, &left.window, Direction::Up), None);
        assert_eq!(target(&tiles, &right.window, Direction::Left), Some(&left));
        assert_eq!(target(&tiles, &right.window, Direction::Down), None);
    }

    #[test]
    fn direction_strings_round_trip() {
        for direction in [
            Direction::Left,
            Direction::Down,
            Direction::Up,
            Direction::Right,
        ] {
            assert_eq!(direction.to_string().parse(), Ok(direction));
        }
        assert_eq!(
            "diagonal".parse::<Direction>(),
            Err("invalid direction \"diagonal\"".into())
        );
    }
}
