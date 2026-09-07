//! Parses the per-depot hotspot metadata shipped by the Logi Options+
//! installer (and re-hosted by assets.openlogi.org) — `core_metadata.json`
//! on newer depots, `metadata.json` on older ones. The caller picks the
//! filename and hands the path to [`Metadata::load_from`].
//!
//! The two generations *mostly* share a schema, but older `metadata.json`
//! files (e.g. the G513 keyboard depot) identify assignments by `slotId`
//! only — there is no `slotName` — so every observed-optional field must
//! stay soft: one missing field would otherwise fail the whole file and
//! drop the `origin` dimensions the renderer needs.
//!
//! Only the fields OpenLogi actually consumes are deserialized — every
//! other field is silently ignored. The schema below is observed-from-the-
//! wild, not derived from any Logitech specification.
//!
//! ```json
//! {
//!   "images": [
//!     {
//!       "key": "device_image",
//!       "origin": { "width": 687, "height": 1024 }
//!     },
//!     {
//!       "key": "device_buttons_image",
//!       "origin": { "width": 687, "height": 1024 },
//!       "assignments": [
//!         { "slotId": "...", "slotName": "SLOT_NAME_MIDDLE_BUTTON",
//!           "marker": { "x": 73, "y": 18 },
//!           "label":  { "x": 1,  "y": 0  } }
//!       ]
//!     }
//!   ]
//! }
//! ```
//!
//! `core_metadata.json` markers use percentages of the origin dimensions.
//! `metadata.json` markers use pixels. `label.{x,y}` is a direction code (-1 = left, 0 = centre,
//! +1 = right; same for y) hinting where the annotation card should sit
//! relative to the marker.

use std::path::Path;

use serde::Deserialize;

use crate::error::AssetError;
use crate::http;

#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
pub struct Metadata {
    pub images: Vec<ImageEntry>,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct ImageEntry {
    pub key: String,
    #[serde(skip)]
    pub coordinates: MarkerCoordinates,
    pub origin: Origin,
    #[serde(default)]
    pub assignments: Vec<Assignment>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum MarkerCoordinates {
    Pixels,
    #[default]
    Percentage,
}

impl ImageEntry {
    #[expect(
        clippy::cast_precision_loss,
        reason = "PNG and metadata dimensions are bounded below the f32 integer precision limit"
    )]
    #[must_use]
    pub fn marker_fraction(&self, assignment: &Assignment, png: Origin) -> Option<Point> {
        if self.origin.width == 0
            || self.origin.height == 0
            || png.width < self.origin.width
            || png.height < self.origin.height
            || png.width > 16384
            || png.height > 16384
        {
            return None;
        }
        let origin_w = self.origin.width as f32;
        let origin_h = self.origin.height as f32;
        let marker = assignment.marker?;
        let (x, y) = match self.coordinates {
            MarkerCoordinates::Pixels => (marker.x / origin_w, marker.y / origin_h),
            MarkerCoordinates::Percentage => (marker.x / 100.0, marker.y / 100.0),
        };
        if !(0.0..=1.0).contains(&x) || !(0.0..=1.0).contains(&y) {
            return None;
        }
        let png_w = png.width as f32;
        let png_h = png.height as f32;
        Some(Point {
            x: ((png_w - origin_w) / 2.0 + x * origin_w) / png_w,
            y: ((png_h - origin_h) / 2.0 + y * origin_h) / png_h,
        })
    }
}

#[derive(Debug, Deserialize, Clone, Copy, PartialEq, Eq)]
pub struct Origin {
    pub width: u32,
    pub height: u32,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct Assignment {
    #[serde(rename = "slotId", default)]
    pub slot_id: String,
    /// Empty on older keyboard depots whose assignments carry only `slotId`;
    /// `map_slot_name`-style consumers treat unknown names as "no hotspot".
    #[serde(rename = "slotName", default)]
    pub slot_name: String,
    #[serde(default)]
    pub marker: Option<Point>,
    #[serde(default)]
    pub label: Direction,
}

#[derive(Debug, Deserialize, Clone, Copy, Default, PartialEq)]
pub struct Point {
    pub x: f32,
    pub y: f32,
}

#[derive(Debug, Deserialize, Clone, Copy, Default, PartialEq, Eq)]
pub struct Direction {
    pub x: i32,
    pub y: i32,
}

impl Metadata {
    /// Load and parse a metadata JSON file from disk.
    pub fn load_from(path: &Path) -> Result<Self, AssetError> {
        let mut metadata: Self = http::load_json(path)?;
        let coordinates = if path.file_name().is_some_and(|name| name == "metadata.json") {
            MarkerCoordinates::Pixels
        } else {
            MarkerCoordinates::Percentage
        };
        for image in &mut metadata.images {
            image.coordinates = coordinates;
        }
        Ok(metadata)
    }

    /// Image dimensions (use the `device_image` entry — both entries
    /// always share the same origin in practice).
    #[must_use]
    pub fn origin(&self) -> Option<Origin> {
        self.images.first().map(|i| i.origin)
    }

    /// Raw assignment iterator over the `device_buttons_image` entry.
    /// Slot-name → application-button mapping is intentionally left to
    /// the consumer (the GUI owns the ButtonId enum).
    pub fn assignments(&self) -> impl Iterator<Item = &Assignment> + '_ {
        self.images
            .iter()
            .find(|i| i.key == "device_buttons_image")
            .into_iter()
            .flat_map(|img| img.assignments.iter())
    }
}

#[cfg(test)]
mod tests {
    use super::Metadata;

    #[test]
    fn absent_markers_are_not_invented_at_the_image_origin() {
        let metadata: Metadata = serde_json::from_str(r#"{"images":[{"key":"device_image","origin":{"width":800,"height":2000},"assignments":[{"slotId":"first"}]}]}"#).unwrap();
        let image = &metadata.images[0];
        assert!(
            image
                .marker_fraction(&image.assignments[0], image.origin)
                .is_none()
        );
    }

    #[test]
    fn source_format_preserves_pixel_slots_and_scales_against_the_selected_image() {
        let dir = tempfile::tempdir().unwrap();
        let json = r#"{"images":[{"key":"device_side","origin":{"width":800,"height":2000},"assignments":[{"slotId":"example_g4_m1","marker":{"x":200,"y":1000}}]}]}"#;
        let path = dir.path().join("metadata.json");
        std::fs::write(&path, json).unwrap();
        let metadata = Metadata::load_from(&path).unwrap();
        let image = &metadata.images[0];
        assert_eq!(image.assignments[0].slot_id, "example_g4_m1");
        assert_eq!(image.coordinates, super::MarkerCoordinates::Pixels);
        assert_eq!(
            image.marker_fraction(
                &image.assignments[0],
                super::Origin {
                    width: 800,
                    height: 2000
                }
            ),
            Some(super::Point { x: 0.25, y: 0.5 })
        );
    }

    #[test]
    fn percentage_markers_keep_padding_and_reject_invalid_geometry() {
        let dir = tempfile::tempdir().unwrap();
        let json = r#"{"images":[{"key":"device_buttons_image","origin":{"width":800,"height":2000},"assignments":[{"slotName":"MIDDLE","marker":{"x":25,"y":50}}]}]}"#;
        let path = dir.path().join("core_metadata.json");
        std::fs::write(&path, json).unwrap();
        let mut metadata = Metadata::load_from(&path).unwrap();
        let image = &mut metadata.images[0];
        let png = super::Origin {
            width: 1000,
            height: 2000,
        };
        assert_eq!(image.coordinates, super::MarkerCoordinates::Percentage);
        assert_eq!(
            image.marker_fraction(&image.assignments[0], png),
            Some(super::Point { x: 0.3, y: 0.5 })
        );
        image.assignments[0].marker.as_mut().unwrap().x = 101.0;
        assert!(image.marker_fraction(&image.assignments[0], png).is_none());
        image.assignments[0].marker.as_mut().unwrap().x = f32::NAN;
        assert!(image.marker_fraction(&image.assignments[0], png).is_none());
        image.assignments[0].marker.as_mut().unwrap().x = 20.0;
        image.origin.width = 0;
        assert!(image.marker_fraction(&image.assignments[0], png).is_none());
    }

    /// Older keyboard depots (G513) identify assignments by `slotId` only —
    /// no `slotName` — and add fields like `assignmentOffset`. Parsing must
    /// not fail wholesale: the renderer still needs `origin`, and unknown
    /// slot names already degrade to "no hotspot" in the consumer.
    #[test]
    fn old_slot_id_only_metadata_parses() {
        let json = r#"{
          "images": [
            {
              "key": "device_image",
              "origin": { "width": 3598, "height": 1315 },
              "assignmentOffset": { "x": 800, "y": 0 },
              "assignments": [
                { "slotId": "g513_g1_m1",
                  "marker": { "x": 370, "y": 300 },
                  "label":  { "x": -1200, "y": 300 } }
              ]
            }
          ]
        }"#;
        let meta: Metadata = serde_json::from_str(json).expect("old schema must parse");
        let origin = meta.origin().expect("origin survives");
        assert_eq!((origin.width, origin.height), (3598, 1315));
        assert_eq!(meta.images[0].assignments[0].slot_name, "");
    }

    /// Camera depots (StreamCam) list settings-slot assignments with no
    /// `marker` under their `device_camera_image` entry. Parsing must not
    /// fail wholesale, and `assignments()` must not surface them (it reads
    /// only the `device_buttons_image` entry).
    #[test]
    fn camera_metadata_without_markers_parses() {
        let json = r#"{
          "images": [
            { "key": "device_image", "origin": { "width": 1280, "height": 800 } },
            {
              "key": "device_camera_image",
              "origin": { "width": 396, "height": 396 },
              "assignments": [
                { "slotId": "streamcam-0893_webcam_camera_settings",
                  "slotName": "SLOT_NAME_WEBCAM_CAMERA_SETTINGS",
                  "disableAssignmentClick": true }
              ]
            }
          ]
        }"#;
        let meta: Metadata = serde_json::from_str(json).expect("camera schema must parse");
        let origin = meta.origin().expect("origin survives");
        assert_eq!((origin.width, origin.height), (1280, 800));
        assert_eq!(meta.assignments().count(), 0);
    }
}
