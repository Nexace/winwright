//! Coordinate types. Everything here is **physical virtual-desktop pixels** (signed: monitors
//! left of or above the primary have negative origins). UIA bounding rectangles are already in
//! this space for a Per-Monitor-V2-aware client, so never scale them again (spec §43).

use std::borrow::Cow;

use schemars::{JsonSchema, Schema, SchemaGenerator};
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
pub struct PhysicalPoint {
    pub x: i32,
    pub y: i32,
}

/// Serialized compactly as `[left, top, right, bottom]`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(from = "[i32; 4]", into = "[i32; 4]")]
pub struct PhysicalRect {
    pub left: i32,
    pub top: i32,
    pub right: i32,
    pub bottom: i32,
}

impl PhysicalRect {
    pub const fn new(left: i32, top: i32, right: i32, bottom: i32) -> Self {
        Self {
            left,
            top,
            right,
            bottom,
        }
    }

    pub const fn width(&self) -> i32 {
        self.right.saturating_sub(self.left)
    }

    pub const fn height(&self) -> i32 {
        self.bottom.saturating_sub(self.top)
    }

    pub const fn is_empty(&self) -> bool {
        self.width() <= 0 || self.height() <= 0
    }

    pub const fn center(&self) -> PhysicalPoint {
        PhysicalPoint {
            x: self.left + self.width() / 2,
            y: self.top + self.height() / 2,
        }
    }

    pub const fn contains(&self, p: PhysicalPoint) -> bool {
        p.x >= self.left && p.x < self.right && p.y >= self.top && p.y < self.bottom
    }

    pub const fn intersects(&self, other: &Self) -> bool {
        self.left < other.right
            && other.left < self.right
            && self.top < other.bottom
            && other.top < self.bottom
    }
}

impl From<[i32; 4]> for PhysicalRect {
    fn from([left, top, right, bottom]: [i32; 4]) -> Self {
        Self::new(left, top, right, bottom)
    }
}

impl From<PhysicalRect> for [i32; 4] {
    fn from(r: PhysicalRect) -> Self {
        [r.left, r.top, r.right, r.bottom]
    }
}

impl JsonSchema for PhysicalRect {
    fn schema_name() -> Cow<'static, str> {
        "PhysicalRect".into()
    }

    fn json_schema(generator: &mut SchemaGenerator) -> Schema {
        <[i32; 4]>::json_schema(generator)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rect_serializes_as_array() {
        let r = PhysicalRect::new(1240, 820, 1324, 858);
        assert_eq!(serde_json::to_string(&r).unwrap(), "[1240,820,1324,858]");
        let back: PhysicalRect = serde_json::from_str("[1240,820,1324,858]").unwrap();
        assert_eq!(back, r);
    }

    #[test]
    fn negative_origin_geometry() {
        let r = PhysicalRect::new(-1920, -200, -1000, 400);
        assert_eq!((r.width(), r.height()), (920, 600));
        assert_eq!(r.center(), PhysicalPoint { x: -1460, y: 100 });
        assert!(r.contains(PhysicalPoint { x: -1920, y: -200 }));
        assert!(!r.contains(PhysicalPoint { x: -1000, y: 0 }));
        assert!(PhysicalRect::new(0, 0, 0, 10).is_empty());
    }
}
