//! A sparkline: an interface's recent rates, as a small line.

use std::collections::VecDeque;

use iced::mouse::Cursor;
use iced::widget::canvas::{self, Frame, Geometry, Path, Stroke};
use iced::{Element, Length, Point, Rectangle, Renderer, Theme};

use crate::message::Message;

pub struct Spark<'a> {
    pub rx: &'a VecDeque<u64>,
    pub tx: &'a VecDeque<u64>,
}

impl canvas::Program<Message> for Spark<'_> {
    type State = ();

    fn draw(
        &self,
        _state: &(),
        renderer: &Renderer,
        theme: &Theme,
        bounds: Rectangle,
        _cursor: Cursor,
    ) -> Vec<Geometry> {
        let mut f = Frame::new(renderer, bounds.size());
        let p = theme.extended_palette();
        let top = self
            .rx
            .iter()
            .chain(self.tx.iter())
            .copied()
            .max()
            .unwrap_or(0)
            .max(1) as f32;
        let n = crate::SPARK_POINTS;
        for (series, color) in [
            (self.rx, p.primary.base.color),
            (self.tx, p.success.base.color),
        ] {
            let skip = series.len().saturating_sub(n);
            let points: Vec<Point> = series
                .iter()
                .skip(skip)
                .enumerate()
                .map(|(i, v)| {
                    Point::new(
                        bounds.width * i as f32 / (n - 1) as f32,
                        bounds.height - 1.0 - (bounds.height - 2.0) * (*v as f32 / top),
                    )
                })
                .collect();
            if points.len() < 2 {
                continue;
            }
            let path = Path::new(|b| {
                b.move_to(points[0]);
                for p in &points[1..] {
                    b.line_to(*p);
                }
            });
            f.stroke(&path, Stroke::default().with_color(color).with_width(1.2));
        }
        vec![f.into_geometry()]
    }
}

pub fn spark<'a>(rx: &'a VecDeque<u64>, tx: &'a VecDeque<u64>) -> Element<'a, Message> {
    canvas::Canvas::new(Spark { rx, tx })
        .width(Length::Fixed(110.0))
        .height(Length::Fixed(22.0))
        .into()
}
