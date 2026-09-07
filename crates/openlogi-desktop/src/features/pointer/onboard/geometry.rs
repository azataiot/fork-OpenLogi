use openlogi_assets::metadata::Point;

pub(super) struct PlacedMarker {
    pub index: u8,
    pub marker: Point,
    pub label: Point,
    pub anchor: Point,
}

pub(super) struct CanvasLayout {
    pub image_width: f32,
    pub image_height: f32,
    pub image_left: f32,
    pub card_width: f32,
    pub markers: Vec<PlacedMarker>,
}

impl CanvasLayout {
    pub fn new(
        width: f32,
        height: f32,
        png: (u32, u32),
        mut markers: Vec<(u8, Point)>,
    ) -> Option<Self> {
        let png_width = f32::from(u16::try_from(png.0).ok()?);
        let png_height = f32::from(u16::try_from(png.1).ok()?);
        if !width.is_finite()
            || !height.is_finite()
            || width < 300.
            || height < 300.
            || png_width == 0.
            || png_height == 0.
            || markers.is_empty()
            || markers.len() > 8
            || markers.iter().any(|(_, marker)| {
                !(0.0..=1.0).contains(&marker.x) || !(0.0..=1.0).contains(&marker.y)
            })
        {
            return None;
        }
        markers.sort_by(|a, b| a.1.y.total_cmp(&b.1.y));
        let image_height = height.min((width - 210.) * png_height / png_width);
        let image_width = image_height * png_width / png_height;
        let image_left = (width - image_width) / 2.;
        let card_width = ((width - image_width) / 2. - 16.).clamp(85., 150.);
        let label_start = |left: bool| {
            let ys: Vec<_> = markers
                .iter()
                .filter(|(_, point)| (point.x < 0.5) == left)
                .map(|(_, point)| point.y * image_height)
                .collect();
            let count = f32::from(u8::try_from(ys.len()).unwrap_or(0));
            (ys.iter().sum::<f32>() / count.max(1.) - (count - 1.) * 32. - 23.)
                .clamp(16., (height - count * 64.).max(16.))
        };
        let starts = [label_start(true), label_start(false)];
        let mut rows = [0_u8; 2];
        let markers = markers
            .into_iter()
            .map(|(index, marker)| {
                let left = marker.x < 0.5;
                let side = usize::from(!left);
                let y = starts[side] + f32::from(rows[side]) * 64.;
                rows[side] += 1;
                let x = if left { 0. } else { width - card_width };
                PlacedMarker {
                    index,
                    marker: Point {
                        x: image_left + marker.x * image_width,
                        y: marker.y * image_height,
                    },
                    label: Point { x, y },
                    anchor: Point {
                        x: if left { card_width } else { x },
                        y: y + 23.,
                    },
                }
            })
            .collect();
        Some(Self {
            image_width,
            image_height,
            image_left,
            card_width,
            markers,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn markers_track_image_scale_and_labels_keep_physical_order() {
        let markers = vec![
            (0, Point { x: 0.2, y: 0.5 }),
            (1, Point { x: 0.2, y: 0.3 }),
            (2, Point { x: 0.8, y: 0.4 }),
        ];
        for width in [320., 480., 640.] {
            let layout = CanvasLayout::new(width, 400., (800, 1600), markers.clone()).unwrap();
            assert_eq!(
                layout
                    .markers
                    .iter()
                    .map(|marker| marker.index)
                    .collect::<Vec<_>>(),
                [1, 2, 0]
            );
            for marker in &layout.markers {
                let source = markers
                    .iter()
                    .find(|(index, _)| *index == marker.index)
                    .unwrap()
                    .1;
                assert!(
                    (marker.marker.x - layout.image_left - source.x * layout.image_width).abs()
                        < 0.001
                );
                assert!((marker.marker.y - source.y * layout.image_height).abs() < 0.001);
                assert!(marker.label.x >= 0. && marker.label.x + layout.card_width <= width);
                assert!(marker.label.y >= 0. && marker.label.y + 46. <= 400.);
            }
            assert!(layout.markers[2].label.y - layout.markers[0].label.y >= 64.);
        }
    }

    #[test]
    fn invalid_canvas_geometry_is_rejected() {
        let markers = vec![(0, Point { x: 0.5, y: 0.5 })];
        for (width, height, png) in [
            (f32::NAN, 400., (800, 1600)),
            (320., 0., (800, 1600)),
            (320., 400., (0, 1600)),
            (320., 400., (800, 0)),
            (320., 400., (100_000, 1600)),
        ] {
            assert!(CanvasLayout::new(width, height, png, markers.clone()).is_none());
        }
        assert!(
            CanvasLayout::new(
                320.,
                400.,
                (800, 1600),
                vec![(
                    0,
                    Point {
                        x: f32::NAN,
                        y: 0.5
                    }
                )]
            )
            .is_none()
        );
    }
}
