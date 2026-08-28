use image::RgbaImage;

const POOL_RADIUS: i32 = 1;
const PIXEL_THRESHOLD: u8 = 8;

#[derive(Debug, Clone, Copy)]
pub struct Metrics {
    pub width: u32,
    pub height: u32,
    pub mean: f32,
    pub p99: u8,
    pub max: u8,
    pub pct_above_threshold: f32,
}

pub(super) struct DiffOutput {
    pub(super) overlay: RgbaImage,
    pub(super) metrics: Metrics,
}

pub(super) fn diff_images(expected: &RgbaImage, actual: &RgbaImage) -> DiffOutput {
    let (width, height) = expected.dimensions();
    let pixel_count = width as usize * height as usize;
    let mut differences = vec![0u8; pixel_count];
    let mut histogram = [0u32; 256];
    for y in 0..height {
        for x in 0..width {
            let index = (y * width + x) as usize;
            let expected_pixel = pixel_at(expected.as_raw(), width, x, y);
            let actual_pixel = pixel_at(actual.as_raw(), width, x, y);
            let expected_delta =
                best_nearby_match(expected_pixel, actual.as_raw(), width, height, x, y);
            let actual_delta =
                best_nearby_match(actual_pixel, expected.as_raw(), width, height, x, y);
            let difference = expected_delta.max(actual_delta);
            differences[index] = difference;
            histogram[difference as usize] += 1;
        }
    }
    let metrics = histogram_metrics(&histogram, pixel_count, width, height);
    let overlay = build_overlay(expected, &differences);
    DiffOutput { overlay, metrics }
}

fn pixel_at(raw: &[u8], width: u32, x: u32, y: u32) -> [u8; 4] {
    let index = ((y * width + x) * 4) as usize;
    [raw[index], raw[index + 1], raw[index + 2], raw[index + 3]]
}

fn best_nearby_match(source: [u8; 4], other: &[u8], width: u32, height: u32, x: u32, y: u32) -> u8 {
    let mut best = u8::MAX;
    for dy in -POOL_RADIUS..=POOL_RADIUS {
        let other_y = y as i32 + dy;
        if !(0..height as i32).contains(&other_y) {
            continue;
        }
        for dx in -POOL_RADIUS..=POOL_RADIUS {
            let other_x = x as i32 + dx;
            if !(0..width as i32).contains(&other_x) {
                continue;
            }
            let index = ((other_y as u32 * width + other_x as u32) * 4) as usize;
            let difference = source[0]
                .abs_diff(other[index])
                .max(source[1].abs_diff(other[index + 1]))
                .max(source[2].abs_diff(other[index + 2]))
                .max(source[3].abs_diff(other[index + 3]));
            best = best.min(difference);
        }
    }
    best
}

fn histogram_metrics(
    histogram: &[u32; 256],
    pixel_count: usize,
    width: u32,
    height: u32,
) -> Metrics {
    let mut sum = 0u64;
    let mut above = 0u64;
    let mut max = 0u8;
    for (value, &count) in histogram.iter().enumerate() {
        sum += value as u64 * u64::from(count);
        if count != 0 {
            max = value as u8;
        }
        if value as u8 > PIXEL_THRESHOLD {
            above += u64::from(count);
        }
    }
    let p99_index = pixel_count as u64 * 99 / 100;
    let mut accumulated = 0u64;
    let mut p99 = 0u8;
    for (value, &count) in histogram.iter().enumerate() {
        accumulated += u64::from(count);
        if accumulated >= p99_index {
            p99 = value as u8;
            break;
        }
    }
    Metrics {
        width,
        height,
        mean: sum as f32 / pixel_count as f32,
        p99,
        max,
        pct_above_threshold: above as f32 * 100.0 / pixel_count as f32,
    }
}

fn build_overlay(expected: &RgbaImage, differences: &[u8]) -> RgbaImage {
    let mut output = expected.clone();
    for (pixel, &difference) in output.pixels_mut().zip(differences) {
        let luminance =
            (0.2126 * pixel[0] as f32 + 0.7152 * pixel[1] as f32 + 0.0722 * pixel[2] as f32) as u8;
        let dimmed = luminance / 2 + 64;
        if difference <= PIXEL_THRESHOLD {
            *pixel = image::Rgba([dimmed, dimmed, dimmed, 255]);
        } else {
            let strength = ((difference - PIXEL_THRESHOLD) as f32 / 48.0).clamp(0.0, 1.0);
            let red = (dimmed as f32 * (1.0 - strength) + 255.0 * strength) as u8;
            let other = (dimmed as f32 * (1.0 - strength)) as u8;
            *pixel = image::Rgba([red, other, other, 255]);
        }
    }
    output
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::Rgba;

    #[test]
    fn pooling_absorbs_a_one_pixel_translation() {
        let mut expected = RgbaImage::new(8, 8);
        let mut actual = RgbaImage::new(8, 8);
        expected.put_pixel(3, 3, Rgba([255, 64, 32, 255]));
        actual.put_pixel(4, 3, Rgba([255, 64, 32, 255]));

        let output = diff_images(&expected, &actual);

        assert_eq!(output.metrics.max, 0);
        assert_eq!(output.metrics.pct_above_threshold, 0.0);
    }

    #[test]
    fn pooling_still_detects_a_missing_feature() {
        let mut expected = RgbaImage::new(8, 8);
        let actual = RgbaImage::new(8, 8);
        for y in 2..=4 {
            for x in 2..=4 {
                expected.put_pixel(x, y, Rgba([255, 64, 32, 255]));
            }
        }

        let output = diff_images(&expected, &actual);

        assert_eq!(output.metrics.max, 255);
        assert!(output.metrics.pct_above_threshold > 0.0);
    }
}
