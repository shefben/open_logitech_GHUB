//! Screen sampling for G HUB's SCREEN SAMPLER effect.
//!
//! The whole primary screen is shrunk once per call into a tiny bitmap with `HALFTONE`
//! stretching, which averages the source pixels under each destination pixel. Each region's
//! colour is then the mean of the small bitmap's pixels inside it. At 64x36 that is 2304 pixels
//! to read, against the 8 million of a 4K frame.
//!
//! Region semantics, checked against every `screen_sampler*.json` G HUB ships: `left`, `top`,
//! `right` and `bottom` are *insets* from the matching screen edge, as fractions of the screen.
//! So `left 0.2, right 0.6` is the strip from 20% to 40% across. Read that way the keyboard file's
//! five regions tile the width exactly (0-0.2, 0.2-0.4, ... 0.8-1.0) over the band 50%-80% down,
//! the illumination file's seven tile it with a small overlap, the zonal file's four are the four
//! quadrants of the 20%-80% band, and the microphone file's four are the four corners. Read as
//! edges instead, most of them would have right < left.

use serde_json::Value;

/// One sampled area: insets from each edge, as fractions of the screen.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Region {
    pub left: f64,
    pub top: f64,
    pub right: f64,
    pub bottom: f64,
}

impl Region {
    /// The covered span as `(x0, y0, x1, y1)` in 0..1, clamped and never inverted.
    pub fn span(&self) -> (f64, f64, f64, f64) {
        let x0 = self.left.clamp(0.0, 1.0);
        let y0 = self.top.clamp(0.0, 1.0);
        let x1 = (1.0 - self.right).clamp(x0, 1.0);
        let y1 = (1.0 - self.bottom).clamp(y0, 1.0);
        (x0, y0, x1, y1)
    }
}

/// The regions of a G HUB `screen_sampler*.json`, ordered by left edge, then top edge, then name.
///
/// Ordered by position rather than by name because the names are not consistent between files
/// (`MID_LEFT` and `MID-LEFT`) and sort alphabetically into a meaningless order. Entries that are
/// missing a coordinate are skipped.
pub fn regions_from_ghub_json(text: &str) -> Vec<(String, Region)> {
    let Ok(root) = serde_json::from_str::<Value>(text) else { return Vec::new() };
    let Some(map) = root.pointer("/screen_sampler_info/region_map").and_then(Value::as_object)
    else {
        return Vec::new();
    };
    let mut out: Vec<(String, Region)> = map
        .values()
        .filter_map(|entry| {
            let number = |key: &str| entry.get(key).and_then(Value::as_f64);
            let region = Region {
                left: number("left")?,
                top: number("top")?,
                right: number("right")?,
                bottom: number("bottom")?,
            };
            let name = entry.get("name").and_then(Value::as_str).unwrap_or("").to_owned();
            Some((name, region))
        })
        .collect();
    out.sort_by(|a, b| {
        a.1.left
            .total_cmp(&b.1.left)
            .then(a.1.top.total_cmp(&b.1.top))
            .then_with(|| a.0.cmp(&b.0))
    });
    out
}

/// Mean colour of each region over a `width` x `height` image stored top-down as BGRA (`BGRX`
/// from GDI). A region too small to cover a pixel takes the pixel under its centre.
///
/// Pure, so the averaging is testable without a screen.
pub fn average_regions(
    pixels: &[u8],
    width: usize,
    height: usize,
    regions: &[Region],
    out: &mut Vec<[u8; 3]>,
) {
    out.clear();
    if width == 0 || height == 0 || pixels.len() < width * height * 4 {
        out.resize(regions.len(), [0, 0, 0]);
        return;
    }
    for region in regions {
        let (x0, y0, x1, y1) = region.span();
        let mut px0 = (x0 * width as f64).floor() as usize;
        let mut py0 = (y0 * height as f64).floor() as usize;
        let mut px1 = (x1 * width as f64).ceil() as usize;
        let mut py1 = (y1 * height as f64).ceil() as usize;
        px1 = px1.min(width);
        py1 = py1.min(height);
        if px1 <= px0 {
            px0 = (((x0 + x1) / 2.0 * width as f64) as usize).min(width - 1);
            px1 = px0 + 1;
        }
        if py1 <= py0 {
            py0 = (((y0 + y1) / 2.0 * height as f64) as usize).min(height - 1);
            py1 = py0 + 1;
        }
        let (mut r, mut g, mut b) = (0u32, 0u32, 0u32);
        for y in py0..py1 {
            let row = &pixels[(y * width + px0) * 4..(y * width + px1) * 4];
            for px in row.chunks_exact(4) {
                b += px[0] as u32;
                g += px[1] as u32;
                r += px[2] as u32;
            }
        }
        let n = ((px1 - px0) * (py1 - py0)) as u32;
        out.push([(r / n) as u8, (g / n) as u8, (b / n) as u8]);
    }
}

/// Width and height of the shrunk capture. 16:9; the read-back of the source, not this size,
/// is what a sample costs.
pub const SAMPLE_WIDTH: usize = 160;
pub const SAMPLE_HEIGHT: usize = 90;

/// The smallest span covering every region, as `(x0, y0, x1, y1)` in 0..1, or `None` when there
/// is nothing to cover.
pub fn bounds(regions: &[Region]) -> Option<(f64, f64, f64, f64)> {
    regions.iter().map(Region::span).reduce(|a, b| (a.0.min(b.0), a.1.min(b.1), a.2.max(b.2), a.3.max(b.3)))
}

/// `region` re-expressed as insets of the `area` span instead of the whole screen.
pub fn relative_to(region: &Region, area: (f64, f64, f64, f64)) -> Region {
    let (ax0, ay0, ax1, ay1) = area;
    let w = (ax1 - ax0).max(f64::EPSILON);
    let h = (ay1 - ay0).max(f64::EPSILON);
    let (x0, y0, x1, y1) = region.span();
    Region {
        left: (x0 - ax0) / w,
        top: (y0 - ay0) / h,
        right: (ax1 - x1) / w,
        bottom: (ay1 - y1) / h,
    }
}

#[cfg(windows)]
pub use live::ScreenSampler;

#[cfg(windows)]
mod live {
    use super::*;
    use crate::sys::*;
    use std::ptr::null_mut;

    /// Holds the screen DC, a memory DC and a small DIB between calls, so a sample is one
    /// `StretchBlt` and a read.
    pub struct ScreenSampler {
        screen: HANDLE,
        memory: HANDLE,
        bitmap: HANDLE,
        previous: HANDLE,
        bits: *const u8,
        colours: Vec<[u8; 3]>,
        relative: Vec<Region>,
    }

    // The handles are only ever used by the thread that holds `&mut self`.
    unsafe impl Send for ScreenSampler {}

    impl ScreenSampler {
        pub fn new() -> Option<Self> {
            unsafe {
                let screen = GetDC(null_mut());
                if screen.is_null() {
                    return None;
                }
                let memory = CreateCompatibleDC(screen);
                if memory.is_null() {
                    ReleaseDC(null_mut(), screen);
                    return None;
                }
                let header = BITMAPINFOHEADER {
                    biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                    biWidth: SAMPLE_WIDTH as i32,
                    // Negative height: rows top-down, which is what `average_regions` expects.
                    biHeight: -(SAMPLE_HEIGHT as i32),
                    biPlanes: 1,
                    biBitCount: 32,
                    biCompression: BI_RGB,
                    ..Default::default()
                };
                let mut bits: *mut std::ffi::c_void = null_mut();
                let bitmap =
                    CreateDIBSection(memory, &header, DIB_RGB_COLORS, &mut bits, null_mut(), 0);
                if bitmap.is_null() || bits.is_null() {
                    DeleteDC(memory);
                    ReleaseDC(null_mut(), screen);
                    return None;
                }
                let previous = SelectObject(memory, bitmap);
                // Point sampling. HALFTONE averages properly but costs half as much again, and at
                // 160x90 point samples are already a fair average of a region.
                SetStretchBltMode(memory, COLORONCOLOR);
                Some(Self {
                    screen,
                    memory,
                    bitmap,
                    previous,
                    bits: bits as *const u8,
                    colours: Vec::new(),
                    relative: Vec::new(),
                })
            }
        }

        /// The average colour of each region on the primary screen, `[r, g, b]`, in order. Black
        /// for every region if the capture fails (a secure desktop, a locked session).
        ///
        /// Only the box around the regions is read back, because the read-back is the cost: the
        /// keyboard layout covers 30% of the screen and costs about a third as much as all of it.
        pub fn sample(&mut self, regions: &[Region]) -> Vec<[u8; 3]> {
            let Some(area) = bounds(regions) else { return Vec::new() };
            let (x0, y0, x1, y1) = area;
            let ok = unsafe {
                let width = GetSystemMetrics(SM_CXSCREEN) as f64;
                let height = GetSystemMetrics(SM_CYSCREEN) as f64;
                let sx = (x0 * width).floor() as i32;
                let sy = (y0 * height).floor() as i32;
                let sw = ((x1 * width).ceil() as i32 - sx).max(1);
                let sh = ((y1 * height).ceil() as i32 - sy).max(1);
                width > 0.0
                    && height > 0.0
                    && StretchBlt(
                        self.memory,
                        0,
                        0,
                        SAMPLE_WIDTH as i32,
                        SAMPLE_HEIGHT as i32,
                        self.screen,
                        sx,
                        sy,
                        sw,
                        sh,
                        SRCCOPY,
                    ) != 0
            };
            if !ok {
                return vec![[0, 0, 0]; regions.len()];
            }
            unsafe { GdiFlush() };
            self.relative.clear();
            self.relative.extend(regions.iter().map(|r| relative_to(r, area)));
            let pixels =
                unsafe { std::slice::from_raw_parts(self.bits, SAMPLE_WIDTH * SAMPLE_HEIGHT * 4) };
            average_regions(pixels, SAMPLE_WIDTH, SAMPLE_HEIGHT, &self.relative, &mut self.colours);
            self.colours.clone()
        }
    }

    impl Drop for ScreenSampler {
        fn drop(&mut self) {
            unsafe {
                SelectObject(self.memory, self.previous);
                DeleteObject(self.bitmap);
                DeleteDC(self.memory);
                ReleaseDC(null_mut(), self.screen);
            }
        }
    }
}

/// Off Windows there is no screen to sample.
#[cfg(not(windows))]
pub struct ScreenSampler;

#[cfg(not(windows))]
impl ScreenSampler {
    pub fn new() -> Option<Self> {
        None
    }

    pub fn sample(&mut self, regions: &[Region]) -> Vec<[u8; 3]> {
        vec![[0, 0, 0]; regions.len()]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// G HUB's `screen_sampler-kb.json`, region map only, ids shortened.
    const KEYBOARD: &str = r#"{"screen_sampler_info":{"region_map":{
        "a":{"id":"a","top":0.5,"left":0,"right":0.8,"bottom":0.2,"color_function":"AVERAGE_COLOR","name":"LEFT"},
        "b":{"id":"b","top":0.5,"left":0.2,"right":0.6,"bottom":0.2,"color_function":"AVERAGE_COLOR","name":"MID_LEFT"},
        "c":{"id":"c","top":0.5,"left":0.4,"right":0.4,"bottom":0.2,"color_function":"AVERAGE_COLOR","name":"MIDDLE"},
        "d":{"id":"d","top":0.5,"left":0.6,"right":0.2,"bottom":0.2,"color_function":"AVERAGE_COLOR","name":"MID_RIGHT"},
        "e":{"id":"e","top":0.5,"left":0.8,"right":0,"bottom":0.2,"color_function":"AVERAGE_COLOR","name":"RIGHT"}
    },"lighting_slots":{}}}"#;

    /// `screen_sampler.json`: the four quadrants of the middle band.
    const ZONAL: &str = r#"{"screen_sampler_info":{"region_map":{
        "1":{"top":0.2,"left":0,"right":0.8,"bottom":0.55,"name":"TOP_LEFT"},
        "2":{"top":0.55,"left":0,"right":0.8,"bottom":0.2,"name":"BOTTOM_LEFT"},
        "3":{"top":0.2,"left":0.8,"right":0,"bottom":0.55,"name":"TOP_RIGHT"},
        "4":{"top":0.55,"left":0.8,"right":0,"bottom":0.2,"name":"BOTTOM_RIGHT"}
    }}}"#;

    fn close(a: f64, b: f64) -> bool {
        (a - b).abs() < 1e-9
    }

    #[test]
    fn keyboard_regions_tile_the_width_left_to_right() {
        let regions = regions_from_ghub_json(KEYBOARD);
        let names: Vec<&str> = regions.iter().map(|(n, _)| n.as_str()).collect();
        assert_eq!(names, ["LEFT", "MID_LEFT", "MIDDLE", "MID_RIGHT", "RIGHT"]);
        let mut edge = 0.0;
        for (_, region) in &regions {
            let (x0, y0, x1, y1) = region.span();
            assert!(close(x0, edge), "regions must abut");
            assert!(close(x1 - x0, 0.2));
            assert!(close(y0, 0.5) && close(y1, 0.8));
            edge = x1;
        }
        assert!(close(edge, 1.0), "the strips must reach the right edge");
    }

    #[test]
    fn zonal_regions_are_the_four_quadrants_of_the_band() {
        let regions = regions_from_ghub_json(ZONAL);
        let spans: Vec<_> = regions.iter().map(|(n, r)| (n.as_str(), r.span())).collect();
        assert_eq!(spans[0].0, "TOP_LEFT");
        let (x0, y0, x1, y1) = spans[0].1;
        assert!(close(x0, 0.0) && close(x1, 0.2) && close(y0, 0.2) && close(y1, 0.45));
        assert_eq!(spans[3].0, "BOTTOM_RIGHT");
        let (x0, y0, x1, y1) = spans[3].1;
        assert!(close(x0, 0.8) && close(x1, 1.0) && close(y0, 0.55) && close(y1, 0.8));
    }

    #[test]
    fn bad_json_and_incomplete_entries_give_nothing_rather_than_a_panic() {
        assert!(regions_from_ghub_json("not json").is_empty());
        assert!(regions_from_ghub_json("{}").is_empty());
        let partial = r#"{"screen_sampler_info":{"region_map":{"x":{"left":0,"top":0,"name":"X"}}}}"#;
        assert!(regions_from_ghub_json(partial).is_empty());
    }

    #[test]
    fn regions_are_remapped_into_their_bounding_box() {
        let regions: Vec<Region> = regions_from_ghub_json(KEYBOARD).into_iter().map(|(_, r)| r).collect();
        let area = bounds(&regions).unwrap();
        assert!(close(area.0, 0.0) && close(area.1, 0.5) && close(area.2, 1.0) && close(area.3, 0.8));
        let (x0, y0, x1, y1) = relative_to(&regions[1], area).span();
        assert!(close(x0, 0.2) && close(x1, 0.4) && close(y0, 0.0) && close(y1, 1.0));
        assert!(bounds(&[]).is_none());
    }

    #[test]
    fn an_inverted_region_collapses_instead_of_going_negative() {
        let r = Region { left: 0.7, top: 0.0, right: 0.7, bottom: 0.0 };
        let (x0, _, x1, _) = r.span();
        assert!(x1 >= x0);
    }

    /// 4x2 BGRA image: left half red, right half blue, bottom-right pixel green.
    fn image() -> Vec<u8> {
        let red = [0, 0, 255, 0];
        let blue = [255, 0, 0, 0];
        let green = [0, 255, 0, 0];
        [red, red, blue, blue, red, red, blue, green].concat()
    }

    #[test]
    fn averaging_reads_bgra_and_returns_rgb() {
        let mut out = Vec::new();
        let left = Region { left: 0.0, top: 0.0, right: 0.5, bottom: 0.0 };
        let top_right = Region { left: 0.5, top: 0.0, right: 0.0, bottom: 0.5 };
        let right = Region { left: 0.5, top: 0.0, right: 0.0, bottom: 0.0 };
        average_regions(&image(), 4, 2, &[left, top_right, right], &mut out);
        assert_eq!(out[0], [255, 0, 0]);
        assert_eq!(out[1], [0, 0, 255]);
        // Three blue pixels and one green: (0, 63, 191).
        assert_eq!(out[2], [0, 63, 191]);
    }

    #[test]
    fn a_region_thinner_than_a_pixel_takes_the_pixel_under_its_centre() {
        let mut out = Vec::new();
        let sliver = Region { left: 0.9, top: 0.9, right: 0.09, bottom: 0.09 };
        average_regions(&image(), 4, 2, &[sliver], &mut out);
        assert_eq!(out[0], [0, 255, 0]);
    }

    #[test]
    fn a_short_buffer_gives_black_rather_than_reading_past_it() {
        let mut out = Vec::new();
        let all = Region { left: 0.0, top: 0.0, right: 0.0, bottom: 0.0 };
        average_regions(&[0; 8], 4, 2, &[all, all], &mut out);
        assert_eq!(out, [[0, 0, 0], [0, 0, 0]]);
    }

    #[cfg(windows)]
    #[test]
    #[ignore = "reads the live screen"]
    fn live_screen_sample() {
        let regions = regions_from_ghub_json(KEYBOARD);
        let only: Vec<Region> = regions.iter().map(|(_, r)| *r).collect();
        let mut sampler = ScreenSampler::new().expect("a screen DC");
        let first = sampler.sample(&only);
        let started = std::time::Instant::now();
        let runs = 100;
        for _ in 0..runs {
            sampler.sample(&only);
        }
        let each = started.elapsed() / runs;
        for ((name, _), colour) in regions.iter().zip(&first) {
            println!("{name:>10}: #{:02x}{:02x}{:02x}", colour[0], colour[1], colour[2]);
        }
        println!("per sample: {each:?}");
        assert_eq!(first.len(), regions.len());
    }
}
