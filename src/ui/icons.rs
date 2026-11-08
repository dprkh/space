use gpui::{prelude::*, *};

#[derive(Clone, Copy)]
pub enum Icon {
    Close,
    Check,
    ChevronRight,
    ChevronLeft,
    ChevronDown,
    Tool,
    Plus,
    Minus,
    Bullet,
    Checkbox(bool),
    SplitVertical,
    SplitHorizontal,
    Sidebar(bool),
    Paperclip,
    Pencil,
    Copy,
    Trash,
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
                    Self::ChevronLeft => &[&[(10., 4.), (6., 8.), (10., 12.)]],
                    Self::ChevronDown => &[&[(4., 6.), (8., 10.), (12., 6.)]],
                    Self::Tool => &[
                        &[(4., 3.), (4., 10.), (12., 10.)],
                        &[(9., 7.), (12., 10.), (9., 13.)],
                    ],
                    Self::Plus => &[&[(8., 4.), (8., 12.)], &[(4., 8.), (12., 8.)]],
                    Self::Minus => &[&[(4., 8.), (12., 8.)]],
                    Self::Copy => &[
                        &[(5., 5.), (13., 5.), (13., 14.), (5., 14.), (5., 5.)],
                        &[(10., 5.), (10., 2.), (2., 2.), (2., 11.), (5., 11.)],
                    ],
                    Self::Trash => &[
                        &[(2., 4.), (14., 4.)],
                        &[(6., 4.), (6., 2.), (10., 2.), (10., 4.)],
                        &[(4., 4.), (5., 14.), (11., 14.), (12., 4.)],
                        &[(7., 6.), (7., 12.)],
                        &[(9., 6.), (9., 12.)],
                    ],
                    Self::Pencil => &[
                        &[
                            (10., 2.),
                            (14., 6.),
                            (6., 14.),
                            (2., 14.),
                            (2., 10.),
                            (10., 2.),
                        ],
                        &[(8., 4.), (12., 8.)],
                    ],
                    Self::SplitVertical => &[
                        &[(2., 2.), (14., 2.), (14., 14.), (2., 14.), (2., 2.)],
                        &[(8., 2.), (8., 14.)],
                    ],
                    Self::SplitHorizontal => &[
                        &[(2., 2.), (14., 2.), (14., 14.), (2., 14.), (2., 2.)],
                        &[(2., 8.), (14., 8.)],
                    ],
                    Self::Sidebar(collapsed) => &[
                        &[(2., 2.), (14., 2.), (14., 14.), (2., 14.), (2., 2.)],
                        &[(6., 2.), (6., 14.)],
                        if collapsed {
                            &[(9., 6.), (11., 8.), (9., 10.)]
                        } else {
                            &[(11., 6.), (9., 8.), (11., 10.)]
                        },
                    ],
                    Self::Checkbox(false) => {
                        &[&[(3., 3.), (13., 3.), (13., 13.), (3., 13.), (3., 3.)]]
                    }
                    Self::Checkbox(true) => &[
                        &[(3., 3.), (13., 3.), (13., 13.), (3., 13.), (3., 3.)],
                        &[(5., 8.), (7., 10.), (11., 6.)],
                    ],
                    Self::Bullet | Self::Paperclip => &[],
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
                if matches!(self, Self::Paperclip) {
                    let radius = |size: f32| {
                        point(
                            bounds.size.width * (size / 16.),
                            bounds.size.height * (size / 16.),
                        )
                    };
                    path.move_to(position(14., 7.));
                    path.line_to(position(8., 13.));
                    path.arc_to(radius(4.), px(0.), false, true, position(2.5, 7.5));
                    path.line_to(position(9., 1.));
                    path.arc_to(radius(2.7), px(0.), false, true, position(13., 5.));
                    path.line_to(position(6.5, 11.5));
                    path.arc_to(radius(1.4), px(0.), false, true, position(4.5, 9.5));
                    path.line_to(position(10.5, 3.5));
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
