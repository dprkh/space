use gpui::{prelude::*, *};

#[derive(Clone, Copy)]
pub enum Icon {
    Close,
    Check,
    ChevronRight,
    ChevronDown,
    Tool,
    Plus,
    Minus,
    Bullet,
    Checkbox(bool),
}

impl Icon {
    pub fn render(self, size: f32, color: u32) -> impl IntoElement {
        canvas(
            |_, _, _| (),
            move |bounds, _, window, _| {
                let position = |x, y| {
                    bounds.origin
                        + point(
                            bounds.size.width * (x / 16.),
                            bounds.size.height * (y / 16.),
                        )
                };
                let mut path = if matches!(self, Self::Bullet) {
                    PathBuilder::fill()
                } else {
                    PathBuilder::stroke(px(1.5))
                };
                let lines: &[&[(f32, f32)]] = match self {
                    Self::Close => &[&[(4., 4.), (12., 12.)], &[(12., 4.), (4., 12.)]],
                    Self::Check => &[&[(3., 8.), (6.5, 11.5), (13., 4.5)]],
                    Self::ChevronRight => &[&[(6., 4.), (10., 8.), (6., 12.)]],
                    Self::ChevronDown => &[&[(4., 6.), (8., 10.), (12., 6.)]],
                    Self::Tool => &[
                        &[(4., 3.), (4., 10.), (12., 10.)],
                        &[(9., 7.), (12., 10.), (9., 13.)],
                    ],
                    Self::Plus => &[&[(8., 4.), (8., 12.)], &[(4., 8.), (12., 8.)]],
                    Self::Minus => &[&[(4., 8.), (12., 8.)]],
                    Self::Checkbox(false) => {
                        &[&[(3., 3.), (13., 3.), (13., 13.), (3., 13.), (3., 3.)]]
                    }
                    Self::Checkbox(true) => &[
                        &[(3., 3.), (13., 3.), (13., 13.), (3., 13.), (3., 3.)],
                        &[(5., 8.), (7., 10.), (11., 6.)],
                    ],
                    Self::Bullet => &[],
                };
                for line in lines {
                    path.move_to(position(line[0].0, line[0].1));
                    for &(x, y) in &line[1..] {
                        path.line_to(position(x, y));
                    }
                }
                if matches!(self, Self::Bullet) {
                    let radius = point(bounds.size.width / 8., bounds.size.height / 8.);
                    path.move_to(position(8., 6.));
                    path.arc_to(radius, px(0.), false, true, position(8., 10.));
                    path.arc_to(radius, px(0.), false, true, position(8., 6.));
                    path.close();
                }
                if let Ok(path) = path.build() {
                    window.paint_path(path, rgb(color));
                }
            },
        )
        .size(px(size))
        .flex_shrink_0()
    }
}
