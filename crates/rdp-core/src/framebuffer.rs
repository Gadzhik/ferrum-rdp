//! A CPU copy of the remote desktop with accumulated damage.
//!
//! The session thread blits dirty rectangles in; the UI takes the damage and
//! uploads only what changed.

/// Pixel layout: 32-bit BGRX (little-endian XRGB8888), i.e. `B8G8R8X8` in GDK.
pub const BYTES_PER_PIXEL: usize = 4;

/// Above this many rectangles the damage collapses into its bounding box.
const MAX_DAMAGE_RECTS: usize = 64;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Rect {
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
}

impl Rect {
    pub const fn new(x: i32, y: i32, w: i32, h: i32) -> Self {
        Self { x, y, w, h }
    }

    pub fn is_empty(&self) -> bool {
        self.w <= 0 || self.h <= 0
    }

    pub fn right(&self) -> i32 {
        self.x + self.w
    }

    pub fn bottom(&self) -> i32 {
        self.y + self.h
    }

    pub fn intersect(&self, other: &Rect) -> Rect {
        let x = self.x.max(other.x);
        let y = self.y.max(other.y);
        let r = self.right().min(other.right());
        let b = self.bottom().min(other.bottom());
        Rect::new(x, y, (r - x).max(0), (b - y).max(0))
    }

    pub fn union(&self, other: &Rect) -> Rect {
        if self.is_empty() {
            return *other;
        }
        if other.is_empty() {
            return *self;
        }
        let x = self.x.min(other.x);
        let y = self.y.min(other.y);
        let r = self.right().max(other.right());
        let b = self.bottom().max(other.bottom());
        Rect::new(x, y, r - x, b - y)
    }

    pub fn contains(&self, other: &Rect) -> bool {
        other.x >= self.x
            && other.y >= self.y
            && other.right() <= self.right()
            && other.bottom() <= self.bottom()
    }
}

#[derive(Default)]
pub struct FrameBuffer {
    pub width: u32,
    pub height: u32,
    pub data: Vec<u8>,
    damage: Vec<Rect>,
    /// Incremented on every resize so consumers can drop stale textures.
    pub generation: u64,
}

impl FrameBuffer {
    pub fn stride(&self) -> usize {
        self.width as usize * BYTES_PER_PIXEL
    }

    pub fn bounds(&self) -> Rect {
        Rect::new(0, 0, self.width as i32, self.height as i32)
    }

    pub fn resize(&mut self, width: u32, height: u32) {
        if width == self.width && height == self.height && !self.data.is_empty() {
            return;
        }
        self.width = width;
        self.height = height;
        self.data = vec![0; width as usize * height as usize * BYTES_PER_PIXEL];
        self.generation += 1;
        self.damage.clear();
        self.damage.push(self.bounds());
    }

    /// Copy `rects` from a source surface (same pixel format).
    pub fn blit(&mut self, src: &[u8], src_stride: usize, width: u32, height: u32, rects: &[Rect]) {
        if width != self.width || height != self.height {
            self.resize(width, height);
        }
        let bounds = self.bounds();
        let dst_stride = self.stride();
        for rect in rects {
            let r = rect.intersect(&bounds);
            if r.is_empty() {
                continue;
            }
            let row_bytes = r.w as usize * BYTES_PER_PIXEL;
            for row in r.y..r.bottom() {
                let s = row as usize * src_stride + r.x as usize * BYTES_PER_PIXEL;
                let d = row as usize * dst_stride + r.x as usize * BYTES_PER_PIXEL;
                if s + row_bytes > src.len() {
                    break;
                }
                self.data[d..d + row_bytes].copy_from_slice(&src[s..s + row_bytes]);
            }
            self.add_damage(r);
        }
    }

    pub fn add_damage(&mut self, r: Rect) {
        if r.is_empty() {
            return;
        }
        if self.damage.iter().any(|d| d.contains(&r)) {
            return;
        }
        self.damage.retain(|d| !r.contains(d));
        self.damage.push(r);
        if self.damage.len() > MAX_DAMAGE_RECTS {
            let all = self
                .damage
                .iter()
                .fold(Rect::default(), |acc, d| acc.union(d));
            self.damage.clear();
            self.damage.push(all);
        }
    }

    pub fn has_damage(&self) -> bool {
        !self.damage.is_empty()
    }

    pub fn take_damage(&mut self) -> Vec<Rect> {
        std::mem::take(&mut self.damage)
    }

    /// Pixel at (x, y) as (r, g, b).
    pub fn pixel(&self, x: u32, y: u32) -> Option<(u8, u8, u8)> {
        if x >= self.width || y >= self.height {
            return None;
        }
        let i = y as usize * self.stride() + x as usize * BYTES_PER_PIXEL;
        Some((self.data[i + 2], self.data[i + 1], self.data[i]))
    }

    /// Convert to tightly packed RGBA (for PNG export / screenshots).
    pub fn to_rgba(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(self.data.len());
        for px in self.data.chunks_exact(4) {
            out.extend_from_slice(&[px[2], px[1], px[0], 0xFF]);
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blit_copies_and_tracks_damage() {
        let mut fb = FrameBuffer::default();
        fb.resize(4, 4);
        fb.take_damage();

        let mut src = vec![0u8; 4 * 4 * 4];
        // pixel (1,1) = pure red in BGRX
        let i = 4 * 4 + 4;
        src[i..i + 4].copy_from_slice(&[0, 0, 255, 0]);
        fb.blit(&src, 16, 4, 4, &[Rect::new(1, 1, 2, 2)]);

        assert_eq!(fb.pixel(1, 1), Some((255, 0, 0)));
        assert_eq!(fb.take_damage(), vec![Rect::new(1, 1, 2, 2)]);
        assert!(!fb.has_damage());
    }

    #[test]
    fn damage_is_clipped_and_merged() {
        let mut fb = FrameBuffer::default();
        fb.resize(10, 10);
        fb.take_damage();
        fb.add_damage(Rect::new(0, 0, 2, 2));
        fb.add_damage(Rect::new(0, 0, 5, 5)); // swallows the first
        fb.add_damage(Rect::new(1, 1, 1, 1)); // contained
        assert_eq!(fb.take_damage(), vec![Rect::new(0, 0, 5, 5)]);

        let src = vec![0u8; 400];
        fb.blit(&src, 40, 10, 10, &[Rect::new(8, 8, 10, 10)]);
        assert_eq!(fb.take_damage(), vec![Rect::new(8, 8, 2, 2)]);
    }

    #[test]
    fn many_rects_collapse() {
        let mut fb = FrameBuffer::default();
        fb.resize(1000, 10);
        fb.take_damage();
        for i in 0..100 {
            fb.add_damage(Rect::new(i * 10, 0, 1, 1));
        }
        let d = fb.take_damage();
        assert!(d.len() <= MAX_DAMAGE_RECTS);
        let all = d.iter().fold(Rect::default(), |a, r| a.union(r));
        assert_eq!(all, Rect::new(0, 0, 991, 1));
    }

    #[test]
    fn resize_bumps_generation() {
        let mut fb = FrameBuffer::default();
        fb.resize(2, 2);
        let g = fb.generation;
        fb.resize(2, 2);
        assert_eq!(fb.generation, g);
        fb.resize(3, 2);
        assert_eq!(fb.generation, g + 1);
        assert_eq!(fb.data.len(), 3 * 2 * 4);
    }
}
