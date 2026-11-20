//! Finding UI elements in a picture: nested blocks a person would point at (a sidebar, a card, a
//! tweet, a paragraph, a button), from the pixels alone, so it works in every app.
//!
//! Interfaces are blocks on a background, separated by gaps of it. A recursive XY cut finds them:
//! take a block's background (the colour most of its border is), trim its margins of that colour,
//! split it where whole rows (or columns) are that colour, and do the same inside each piece. A
//! card filled with its own colour is one piece on the page; inside it, its fill is the background
//! its lines sit on.

use crate::screenshot::{Image, Rect};

/// One block and the blocks inside it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Region {
    /// In the image's pixels.
    pub rect: Rect,
    pub children: Vec<Region>,
}

/// How different two pixels may be (per channel) and still count as the same plain colour.
const TOLERANCE: i16 = 10;
/// The narrowest gap that separates two blocks, in pixels.
const MIN_GAP: i32 = 3;
/// Blocks smaller than this on either side aren't split further.
const MIN_SIDE: i32 = 6;
/// How deep the cut goes: window, pane, section, card, line, word.
const MAX_DEPTH: usize = 8;

impl Region {
    /// The blocks inside `rect` of `image`, as a tree whose root is `rect` itself.
    pub fn find(image: &Image, rect: Rect) -> Region {
        let bounds = Rect { x: 0, y: 0, width: image.width as i32, height: image.height as i32 };
        match rect.clamp_to(bounds) {
            Some(rect) => View { image }.split(rect, 0),
            None => Region { rect, children: Vec::new() },
        }
    }

    /// The chain of blocks containing a point, outermost first.
    pub fn path_at(&self, x: i32, y: i32) -> Vec<Rect> {
        let mut path = Vec::new();
        let mut node = self;
        if !node.rect.contains(x, y) {
            return path;
        }
        loop {
            path.push(node.rect);
            match node.children.iter().find(|c| c.rect.contains(x, y)) {
                Some(child) => node = child,
                None => return path,
            }
        }
    }
}

struct View<'a> {
    image: &'a Image,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Axis {
    Rows,
    Columns,
}

type Rgb = [i16; 3];

fn near(a: Rgb, b: Rgb) -> bool {
    (0..3).all(|c| (a[c] - b[c]).abs() <= TOLERANCE)
}

impl View<'_> {
    fn px(&self, x: i32, y: i32) -> Rgb {
        let i = (y as usize * self.image.width as usize + x as usize) * 4;
        match self.image.rgba.get(i..i + 3) {
            Some(p) => [i16::from(p[0]), i16::from(p[1]), i16::from(p[2])],
            None => [0; 3],
        }
    }

    /// The colour most of `r`'s border is: the background its contents sit on.
    fn background(&self, r: Rect) -> Rgb {
        let mut counts: Vec<(Rgb, usize)> = Vec::new();
        let (x1, y1) = (r.x + r.width - 1, r.y + r.height - 1);
        let border =
            (r.x..=x1).flat_map(|x| [(x, r.y), (x, y1)]).chain((r.y..=y1).flat_map(|y| [(r.x, y), (x1, y)])).step_by(3);
        for (x, y) in border {
            let p = self.px(x, y);
            match counts.iter_mut().find(|(c, _)| near(*c, p)) {
                Some((_, n)) => *n += 1,
                None => counts.push((p, 1)),
            }
        }
        counts.into_iter().max_by_key(|(_, n)| *n).map_or([0; 3], |(c, _)| c)
    }

    /// Whether row `i` (or column) of `r` is all `bg`.
    fn gap(&self, r: Rect, axis: Axis, i: i32, bg: Rgb) -> bool {
        match axis {
            Axis::Rows => (r.x..r.x + r.width).all(|x| near(self.px(x, i), bg)),
            Axis::Columns => (r.y..r.y + r.height).all(|y| near(self.px(i, y), bg)),
        }
    }

    /// `r` without its `bg` margins; `None` when it's all `bg`.
    fn trim(&self, r: Rect, bg: Rgb) -> Option<Rect> {
        let rows: Vec<bool> = (r.y..r.y + r.height).map(|y| self.gap(r, Axis::Rows, y, bg)).collect();
        let top = rows.iter().position(|g| !g)? as i32;
        let bottom = rows.iter().rposition(|g| !g)? as i32;
        let r = Rect { x: r.x, y: r.y + top, width: r.width, height: bottom - top + 1 };
        let cols: Vec<bool> = (r.x..r.x + r.width).map(|x| self.gap(r, Axis::Columns, x, bg)).collect();
        let left = cols.iter().position(|g| !g)? as i32;
        let right = cols.iter().rposition(|g| !g)? as i32;
        Some(Rect { x: r.x + left, y: r.y, width: right - left + 1, height: r.height })
    }

    /// The pieces of `r` between runs of at least [`MIN_GAP`] `bg` rows (or columns), each
    /// trimmed. Narrower gaps (between letters, or a line's spacing) don't split.
    fn cut(&self, r: Rect, axis: Axis, bg: Rgb) -> Vec<Rect> {
        let (start, len) = match axis {
            Axis::Rows => (r.y, r.height),
            Axis::Columns => (r.x, r.width),
        };
        let gaps: Vec<bool> = (start..start + len).map(|i| self.gap(r, axis, i, bg)).collect();
        let mut pieces = Vec::new();
        let mut piece_start: Option<usize> = None;
        let mut i = 0;
        while i <= gaps.len() {
            let run = gaps[i.min(gaps.len())..].iter().take_while(|g| **g).count();
            let at_end = i == gaps.len();
            if at_end || (run >= MIN_GAP as usize && piece_start.is_some()) {
                if let Some(s) = piece_start.take() {
                    let (s, e) = (start + s as i32, start + i as i32);
                    let piece = match axis {
                        Axis::Rows => Rect { x: r.x, y: s, width: r.width, height: e - s },
                        Axis::Columns => Rect { x: s, y: r.y, width: e - s, height: r.height },
                    };
                    pieces.extend(self.trim(piece, bg).filter(|p| p.width >= 3 && p.height >= 3));
                }
                if at_end {
                    break;
                }
                i += run;
            } else if run > 0 {
                i += run;
            } else {
                piece_start.get_or_insert(i);
                i += 1;
            }
        }
        pieces
    }

    /// The block `r` and what's inside it.
    fn split(&self, r: Rect, depth: usize) -> Region {
        let mut region = Region { rect: r, children: Vec::new() };
        if depth >= MAX_DEPTH || r.width < MIN_SIDE || r.height < MIN_SIDE {
            return region;
        }
        let bg = self.background(r);
        let Some(inner) = self.trim(r, bg) else { return region };
        // Prefer the cut into more pieces: rows for stacked content, columns for side by side.
        let rows = self.cut(inner, Axis::Rows, bg);
        let cols = self.cut(inner, Axis::Columns, bg);
        let pieces = if rows.len() >= cols.len() { rows } else { cols };
        region.children = match pieces.as_slice() {
            [] => Vec::new(),
            // The whole block again: nothing new inside.
            [only] if *only == r => Vec::new(),
            many => many.iter().map(|p| self.split(*p, depth + 1)).collect(),
        };
        region
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const BG: [u8; 3] = [30, 30, 30];

    fn canvas(w: u32, h: u32) -> Image {
        Image { width: w, height: h, rgba: (0..w * h).flat_map(|_| [BG[0], BG[1], BG[2], 255]).collect() }
    }

    fn paint(image: &mut Image, r: Rect, rgb: [u8; 3]) {
        for y in r.y..r.y + r.height {
            for x in r.x..r.x + r.width {
                let i = (y as usize * image.width as usize + x as usize) * 4;
                image.rgba[i..i + 3].copy_from_slice(&rgb);
            }
        }
    }

    /// Text-like content: alternating columns, so no row or column inside is plain... except
    /// between "words".
    fn text(image: &mut Image, r: Rect) {
        for x in (r.x..r.x + r.width).step_by(2) {
            paint(image, Rect { x, y: r.y, width: 1, height: r.height }, [220, 220, 220]);
        }
    }

    #[test]
    fn stacked_cards_and_their_lines_nest() {
        let mut img = canvas(200, 120);
        let card_a = Rect { x: 10, y: 10, width: 180, height: 40 };
        let card_b = Rect { x: 10, y: 70, width: 180, height: 40 };
        paint(&mut img, card_a, [60, 60, 70]);
        paint(&mut img, card_b, [60, 60, 70]);
        let line = Rect { x: 20, y: 20, width: 100, height: 8 };
        text(&mut img, line);
        let root = Region::find(&img, Rect { x: 0, y: 0, width: 200, height: 120 });

        let path = root.path_at(30, 22);
        assert_eq!(path.first(), Some(&Rect { x: 0, y: 0, width: 200, height: 120 }));
        assert!(path.contains(&card_a), "{path:?}");
        assert!(path.iter().any(|r| r.y == 20 && r.height == 8), "the text line is a block: {path:?}");
        assert!(root.path_at(30, 80).contains(&card_b));
        // The background between the cards is nobody's block but the window's.
        assert_eq!(root.path_at(30, 60), vec![Rect { x: 0, y: 0, width: 200, height: 120 }]);
    }

    #[test]
    fn side_by_side_panes_split_by_columns() {
        let mut img = canvas(200, 100);
        let left = Rect { x: 0, y: 0, width: 60, height: 100 };
        paint(&mut img, left, [50, 50, 50]);
        text(&mut img, Rect { x: 5, y: 5, width: 40, height: 6 });
        text(&mut img, Rect { x: 80, y: 30, width: 100, height: 6 });
        let root = Region::find(&img, Rect { x: 0, y: 0, width: 200, height: 100 });
        assert!(root.path_at(10, 50).contains(&left), "{:?}", root.path_at(10, 50));
        // Text columns run from x 80 to 178 (every other pixel), so the block is 99 wide.
        assert!(root.path_at(100, 32).iter().any(|r| r.x == 80 && r.width == 99), "{:?}", root.path_at(100, 32));
    }

    #[test]
    fn a_framed_box_has_its_content_inside() {
        let mut img = canvas(100, 60);
        let frame = Rect { x: 10, y: 10, width: 80, height: 40 };
        paint(&mut img, frame, [120, 120, 120]);
        paint(&mut img, Rect { x: 11, y: 11, width: 78, height: 38 }, [40, 40, 50]);
        text(&mut img, Rect { x: 20, y: 25, width: 40, height: 6 });
        let root = Region::find(&img, Rect { x: 0, y: 0, width: 100, height: 60 });
        let path = root.path_at(22, 27);
        assert!(path.contains(&frame), "{path:?}");
        assert!(path.iter().any(|r| r.width == 39 && r.height == 6), "{path:?}");
    }

    #[test]
    fn a_plain_window_has_no_blocks_and_outside_points_have_no_path() {
        let img = canvas(50, 50);
        let root = Region::find(&img, Rect { x: 0, y: 0, width: 50, height: 50 });
        assert!(root.children.is_empty());
        assert!(root.path_at(99, 99).is_empty());
    }
}
