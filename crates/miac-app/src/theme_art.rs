//! Rasterize theme artwork at its final physical pixel size. Slint's software
//! renderer samples ordinary RGBA textures without interpolating a second resize.
use slint::{ComponentHandle, Image, Rgba8Pixel, SharedPixelBuffer};
use std::{cell::RefCell, collections::VecDeque, rc::Rc, time::{Duration, Instant}};

const SOURCE_IDLE_TIMEOUT: Duration = Duration::from_secs(2);

struct Raster { width: u32, height: u32, rgba: Vec<u8> }

// Ignore empty canvas without changing the supplied image file or its pixels.
fn visible_bounds(source: &Raster) -> Option<(u32, u32, u32, u32)> {
    let (mut left, mut top, mut right, mut bottom) = (source.width, source.height, 0, 0);
    for (index, pixel) in source.rgba.chunks_exact(4).enumerate() {
        if pixel[3] == 0 { continue; }
        let (x, y) = (index as u32 % source.width, index as u32 / source.width);
        left = left.min(x); top = top.min(y);
        right = right.max(x + 1); bottom = bottom.max(y + 1);
    }
    (right > left && bottom > top).then_some((left, top, right.saturating_sub(left), bottom.saturating_sub(top)))
}

fn decode(bytes: &[u8]) -> Result<Raster, String> {
    let mut decoder = png::Decoder::new(std::io::Cursor::new(bytes));
    decoder.set_transformations(png::Transformations::EXPAND | png::Transformations::STRIP_16);
    let mut reader = decoder.read_info().map_err(|e| e.to_string())?;
    let mut rgba = vec![0; reader.output_buffer_size().ok_or("主题图片过大")?];
    let info = reader.next_frame(&mut rgba).map_err(|e| e.to_string())?;
    if info.color_type != png::ColorType::Rgba { return Err("主题图片需要透明 RGBA 格式".into()); }
    rgba.truncate(info.buffer_size());
    Ok(Raster { width: info.width, height: info.height, rgba })
}

// Include letterboxing in the texture, so Image.fill draws it at 1:1.
// Fractional source-pixel coverage keeps thin lines smooth at non-integer ratios.
fn filtered(source: &Raster, crop: (u32, u32, u32, u32), width: u32, height: u32) -> SharedPixelBuffer<Rgba8Pixel> {
    let (x0, y0, crop_w, crop_h) = crop;
    let mut result = SharedPixelBuffer::<Rgba8Pixel>::new(width, height);
    let scale = (width as f64 / crop_w as f64).min(height as f64 / crop_h as f64);
    let left = (width as f64 - crop_w as f64 * scale) / 2.0;
    let top = (height as f64 - crop_h as f64 * scale) / 2.0;
    for (i, pixel) in result.make_mut_slice().iter_mut().enumerate() {
        let x = (i as u32 % width) as f64;
        let y = (i as u32 / width) as f64;
        let mut sum = [0.0f64; 4];
        if scale > 1.0 {
            // Interpolate premultiplied colors when enlarging the supplied
            // artwork, so 150/200% DPI does not turn its outline into blocks.
            let sx = ((x + 0.5 - left) / scale - 0.5).clamp(0.0, crop_w as f64 - 1.0);
            let sy = ((y + 0.5 - top) / scale - 0.5).clamp(0.0, crop_h as f64 - 1.0);
            let (fx, fy) = (sx.fract(), sy.fract());
            for (dy, wy) in [(0, 1.0 - fy), (1, fy)] {
                for (dx, wx) in [(0, 1.0 - fx), (1, fx)] {
                    let sample_x = x0 + (sx.floor() as u32 + dx).min(crop_w - 1);
                    let sample_y = y0 + (sy.floor() as u32 + dy).min(crop_h - 1);
                    let index = ((sample_y * source.width + sample_x) * 4) as usize;
                    let rgba = &source.rgba[index..index + 4];
                    let alpha = rgba[3] as f64 * wx * wy;
                    for c in 0..3 { sum[c] += rgba[c] as f64 * alpha; }
                    sum[3] += alpha;
                }
            }
            let coverage_x = ((x + 1.0).min(left + crop_w as f64 * scale) - x.max(left)).clamp(0.0, 1.0);
            let coverage_y = ((y + 1.0).min(top + crop_h as f64 * scale) - y.max(top)).clamp(0.0, 1.0);
            let color = |c: usize| if sum[3] == 0.0 { 0 } else { (sum[c] / sum[3]).round().clamp(0.0, 255.0) as u8 };
            let alpha = (sum[3] * coverage_x * coverage_y).round().clamp(0.0, 255.0) as u8;
            *pixel = Rgba8Pixel::new(color(0), color(1), color(2), alpha);
            continue;
        }
        let sx0 = x0 as f64 + ((x - left) / scale).clamp(0.0, crop_w as f64);
        let sy0 = y0 as f64 + ((y - top) / scale).clamp(0.0, crop_h as f64);
        let sx1 = x0 as f64 + ((x + 1.0 - left) / scale).clamp(0.0, crop_w as f64);
        let sy1 = y0 as f64 + ((y + 1.0 - top) / scale).clamp(0.0, crop_h as f64);
        for sy in sy0.floor() as u32..sy1.ceil() as u32 {
            let wy = (sy1.min(sy as f64 + 1.0) - sy0.max(sy as f64)).max(0.0);
            for sx in sx0.floor() as u32..sx1.ceil() as u32 {
                let wx = (sx1.min(sx as f64 + 1.0) - sx0.max(sx as f64)).max(0.0);
                let index = ((sy * source.width + sx) * 4) as usize;
                let rgba = &source.rgba[index..index + 4];
                let alpha = rgba[3] as f64 * wx * wy;
                for c in 0..3 { sum[c] += rgba[c] as f64 * alpha; }
                sum[3] += alpha;
            }
        }
        let color = |c: usize| if sum[3] == 0.0 { 0 } else { (sum[c] / sum[3]).round().clamp(0.0, 255.0) as u8 };
        let alpha = (sum[3] * scale * scale).round().clamp(0.0, 255.0) as u8;
        *pixel = Rgba8Pixel::new(color(0), color(1), color(2), alpha);
    }
    result
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
struct Key { kind: i32, width: u32, height: u32 }

#[derive(Default)]
pub struct ArtCache {
    atlas: Option<Raster>,
    hero: Option<Raster>,
    standing: Option<(Raster, (u32, u32, u32, u32))>,
    images: VecDeque<(Key, Image)>,
    cached_bytes: u64,
    renders: usize,
    last_source_use: Option<Instant>,
    // The timer holds only a Weak<ArtCache>, so dropping the UI also drops it.
    source_release_timer: Option<slint::Timer>,
}

impl ArtCache {
    pub fn image(&mut self, kind: i32, physical_width: f32, physical_height: f32) -> Result<Image, String> {
        if !(0..=7).contains(&kind) { return Err("角色编号无效".into()); }
        if !physical_width.is_finite() || !physical_height.is_finite()
            || physical_width <= 0.0 || physical_height <= 0.0 { return Ok(Image::default()); }
        let key = Key { kind, width: (physical_width.round() as u32).clamp(1, 4096),
            height: (physical_height.round() as u32).clamp(1, 4096) };
        if let Some(index) = self.images.iter().position(|(cached, _)| *cached == key) {
            let entry = self.images.remove(index).unwrap();
            let image = entry.1.clone();
            self.images.push_back(entry);
            return Ok(image);
        }
        self.last_source_use = Some(Instant::now());
        let (source, crop) = if kind == 7 {
            if self.standing.is_none() {
                let raster = decode(include_bytes!("../assets/senren/murasame-standing.png"))?;
                let bounds = visible_bounds(&raster).ok_or("丛雨立绘没有可见内容")?;
                self.standing = Some((raster, bounds));
            }
            let (raster, bounds) = self.standing.as_ref().unwrap();
            (raster, *bounds)
        } else if kind == 6 {
            if self.hero.is_none() { self.hero = Some(decode(include_bytes!("../assets/senren/murasame.png"))?); }
            let hero = self.hero.as_ref().unwrap();
            (hero, (0, 0, hero.width, hero.height))
        } else {
            if self.atlas.is_none() { self.atlas = Some(decode(include_bytes!("../assets/senren/character-atlas.png"))?); }
            let atlas = self.atlas.as_ref().unwrap();
            let (cw, ch) = (atlas.width / 3, atlas.height / 2);
            if atlas.width % 3 != 0 || atlas.height % 2 != 0 || cw == 0 || ch == 0 {
                return Err("角色图集需要三列、两行".into());
            }
            let index = kind as u32;
            (atlas, (index % 3 * cw, index / 3 * ch, cw, ch))
        };
        let image = Image::from_rgba8(filtered(source, crop, key.width, key.height));
        let bytes = key.width as u64 * key.height as u64 * 4;
        while !self.images.is_empty() && (self.images.len() >= 64 || self.cached_bytes + bytes > 8 * 1024 * 1024) {
            let (old, _) = self.images.pop_front().unwrap();
            self.cached_bytes -= old.width as u64 * old.height as u64 * 4;
        }
        self.cached_bytes += bytes;
        self.images.push_back((key, image.clone()));
        self.renders += 1;
        Ok(image)
    }

    fn source_bytes(&self) -> usize {
        self.atlas.as_ref().map_or(0, |r| r.rgba.len())
            + self.hero.as_ref().map_or(0, |r| r.rgba.len())
            + self.standing.as_ref().map_or(0, |(r, _)| r.rgba.len())
    }

    fn release_idle_sources(&mut self, now: Instant) -> usize {
        if !self.last_source_use.is_some_and(|used| now.saturating_duration_since(used) >= SOURCE_IDLE_TIMEOUT) {
            return 0;
        }
        // Keep the small, final-size images and their exact pixels. The PNGs
        // remain embedded and can be decoded again for a new size or DPI.
        let released = self.source_bytes();
        self.atlas = None;
        self.hero = None;
        self.standing = None;
        self.last_source_use = None;
        released
    }

    #[allow(dead_code)] // Read by the headless UI regression example.
    pub fn cached_sizes(&self) -> Vec<(i32, u32, u32)> {
        self.images.iter().map(|(key, _)| (key.kind, key.width, key.height)).collect()
    }
}

pub fn install(ui: &crate::MainWindow) -> Rc<RefCell<ArtCache>> {
    let cache = Rc::new(RefCell::new(ArtCache::default()));
    let weak_cache = Rc::downgrade(&cache);
    let release_timer = slint::Timer::default();
    release_timer.start(slint::TimerMode::Repeated, Duration::from_secs(1), move || {
        if let Some(cache) = weak_cache.upgrade() {
            if let Ok(mut cache) = cache.try_borrow_mut() {
                cache.release_idle_sources(Instant::now());
            }
        }
    });
    cache.borrow_mut().source_release_timer = Some(release_timer);
    let callback_cache = cache.clone();
    ui.global::<crate::Theme>().on_image_for(move |kind, width, height| {
        callback_cache.borrow_mut().image(kind, width, height).unwrap_or_else(|error| {
            eprintln!("[主题] 图片绘制失败：{error}");
            Image::default()
        })
    });
    cache
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn idle_sources_are_released_without_losing_displayed_pixels_or_resize_support() {
        let mut cache = ArtCache::default();
        let icon = cache.image(0, 36.0, 36.0).unwrap().to_rgba8().unwrap();
        cache.image(6, 180.0, 240.0).unwrap();
        cache.image(7, 284.0, 520.0).unwrap();
        let raw_bytes = cache.source_bytes();
        assert_eq!(raw_bytes, 15_270_912);
        let used = cache.last_source_use.unwrap();
        assert_eq!(cache.release_idle_sources(used + SOURCE_IDLE_TIMEOUT - Duration::from_millis(1)), 0);
        assert_eq!(cache.source_bytes(), raw_bytes, "continuous resizing keeps reusable decoded sources");
        let cached_bytes = cache.cached_bytes;
        let renders = cache.renders;
        assert_eq!(cache.release_idle_sources(used + SOURCE_IDLE_TIMEOUT), raw_bytes);
        assert_eq!(cache.source_bytes(), 0);
        assert_eq!(cache.cached_bytes, cached_bytes);
        let reused = cache.image(0, 36.0, 36.0).unwrap().to_rgba8().unwrap();
        assert_eq!(icon.as_bytes(), reused.as_bytes(), "idle release preserves exact displayed pixels");
        assert_eq!(cache.renders, renders, "an ordinary redraw must reuse the final-size image");
        assert_eq!(cache.source_bytes(), 0, "redrawing a cached size must not decode the PNG again");
        let resized = cache.image(0, 72.0, 72.0).unwrap();
        assert_eq!((resized.size().width, resized.size().height), (72, 72));
        assert_eq!(cache.source_bytes(), 1536 * 1024 * 4, "a DPI change reloads only its required source");
    }
    #[test]
    fn transparent_background_colors_do_not_bleed_into_character_edges() {
        let source = Raster { width: 2, height: 1, rgba: vec![255, 0, 0, 0, 0, 0, 255, 255] };
        let pixel = filtered(&source, (0, 0, 2, 1), 1, 1).as_slice()[0];
        assert_eq!((pixel.r, pixel.g, pixel.b, pixel.a), (0, 0, 255, 64));
    }
    #[test]
    fn fractional_shrinking_keeps_partial_pixel_coverage() {
        let source = Raster { width: 3, height: 1, rgba: vec![255, 0, 0, 255, 0, 0, 255, 255, 0, 0, 255, 255] };
        let pixels = filtered(&source, (0, 0, 3, 1), 2, 1);
        let first = pixels.as_slice()[0];
        assert_eq!((first.r, first.g, first.b, first.a), (170, 0, 85, 170));
    }
    #[test]
    fn enlarging_transparent_edges_keeps_smooth_coverage_without_color_fringe() {
        let source = Raster { width: 2, height: 1, rgba: vec![255, 0, 0, 0, 0, 0, 255, 255] };
        let pixels = filtered(&source, (0, 0, 2, 1), 4, 2);
        let edge = pixels.as_slice()[1];
        assert_eq!((edge.r, edge.g, edge.b, edge.a), (0, 0, 255, 64));
    }
    #[test]
    fn standing_art_uses_visible_bounds_and_preserves_source_transparency() {
        let source = decode(include_bytes!("../assets/senren/murasame-standing.png")).unwrap();
        assert_eq!(visible_bounds(&source), Some((671, 153, 281, 515)));
        assert_eq!(source.rgba[3], 0, "the supplied white canvas is already transparent");
        let mut cache = ArtCache::default();
        let image = cache.image(7, 284.0, 520.0).unwrap();
        let pixels = image.to_rgba8().unwrap();
        assert_eq!(pixels.as_slice()[0].a, 0, "no rectangular canvas should be visible");
        assert!(pixels.as_slice().iter().any(|p| p.a > 240), "standing artwork must fill its own region");
    }
    #[test]
    fn each_screen_scale_gets_its_own_texture_and_reuses_it() {
        let mut cache = ArtCache::default();
        for scale in [1.0, 1.5, 2.0] {
            for logical in [26.0, 28.0, 36.0] {
                let image = cache.image(0, logical * scale, logical * scale).unwrap();
                assert_eq!(image.size().width, (logical * scale) as u32);
                let renders = cache.renders;
                cache.image(0, logical * scale, logical * scale).unwrap();
                assert_eq!(cache.renders, renders, "unchanged sizes must not re-filter artwork");
            }
            let image = cache.image(6, 180.0 * scale, 240.0 * scale).unwrap();
            assert_eq!((image.size().width, image.size().height), ((180.0 * scale) as u32, (240.0 * scale) as u32));
            let rgba = image.to_rgba8().unwrap();
            assert_eq!(rgba.as_slice()[0].a, 0, "letterboxed corners must stay transparent");
        }
    }
}
