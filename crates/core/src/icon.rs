//! 应用图标合成（easytidy 品牌风格）。
//!
//! 从容器拿到基础图标 → 居中裁剪正方形 → 缩放为 256×0.9 = 230px 内容区,
//! 外围 256×0.1 = 13px 使用天蓝色 → 深蓝色 45° 线性渐变边框包边,
//! 右下角附 easytidy 水印(64×64)。输出 256×256 PNG。

use crate::error::{Error, Result};

/// 合成尺寸
const SIZE: u32 = 256;
/// 内容区边长(256×0.9)
const CONTENT: u32 = 230;
/// 边框宽度(256×0.1 的一半,居中)
const BORDER: i64 = ((SIZE - CONTENT) / 2) as i64;
/// 水印尺寸(右下角)
const WATERMARK: u32 = 96;

/// 天蓝色(渐变起点,左上)
const SKY: [u8; 3] = [0x6a, 0xa2, 0xe7];
/// 深蓝色(渐变终点,右下)
const DEEP: [u8; 3] = [0x1e, 0x66, 0xf5];

fn is_in_content_area(x: u32, y: u32) -> bool {
    let b = BORDER as u32;
    x > b && x < b + CONTENT && y > b && y < b + CONTENT
}

/// 合成应用图标。
///
/// - `base`:容器内基础图标(PNG 字节)
/// - `watermark`:easytidy 品牌图标(PNG 字节,右下角水印)
///
/// 返回合成后的 256×256 PNG 字节。
pub fn compose_app_icon(base: &[u8], watermark: &[u8]) -> Result<Vec<u8>> {
    use image::{ImageFormat, Rgba, RgbaImage};

    // 基础图标 → 居中裁剪正方形 → 缩放 CONTENT×CONTENT
    let base_img = image::load_from_memory(base)
        .map_err(|e| Error::Config(format!("解析基础图标失败：{e}")))?
        .to_rgba8();
    let (w, h) = base_img.dimensions();
    let side = w.min(h);
    let cropped = image::imageops::crop_imm(&base_img, (w - side) / 2, (h - side) / 2, side, side);
    let content = image::imageops::resize(
        &cropped.to_image(),
        CONTENT,
        CONTENT,
        image::imageops::FilterType::Lanczos3,
    );

    // 画布:45° 渐变边框(左上天空蓝 → 右下深蓝,t = (x+y)/对角和)
    let mut canvas = RgbaImage::new(SIZE, SIZE);
    let max_t = 2.0 * (SIZE - 1) as f32;
    for y in 0..SIZE {
        for x in 0..SIZE {
            // 内容区由 overlay 覆盖：跳过渐变底色（半透明基础图标的透明区
            // 不再透出品牌色；content 为不透明图标时视觉效果不变）
            if is_in_content_area(x, y) {
                continue;
            }

            let t = (x + y) as f32 / max_t;
            let r = SKY[0] as f32 + (DEEP[0] as f32 - SKY[0] as f32) * t;
            let g = SKY[1] as f32 + (DEEP[1] as f32 - SKY[1] as f32) * t;
            let b = SKY[2] as f32 + (DEEP[2] as f32 - SKY[2] as f32) * t;

            canvas.put_pixel(x, y, Rgba([r as u8, g as u8, b as u8, 255]));
        }
    }

    // 中心内容(居中,留边框)
    image::imageops::overlay(&mut canvas, &content, BORDER, BORDER);

    // 右下角水印:品牌图标缩放
    let wm_img = image::load_from_memory(watermark)
        .map_err(|e| Error::Config(format!("解析水印图标失败：{e}")))?
        .to_rgba8();
    let wm = image::imageops::resize(
        &wm_img,
        WATERMARK,
        WATERMARK,
        image::imageops::FilterType::Lanczos3,
    );
    image::imageops::overlay(&mut canvas, &wm, (SIZE - WATERMARK) as i64, (SIZE - WATERMARK) as i64);

    // 编码 PNG
    let mut out = Vec::new();
    canvas
        .write_to(&mut std::io::Cursor::new(&mut out), ImageFormat::Png)
        .map_err(|e| Error::Config(format!("编码合成图标失败：{e}")))?;
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_compose_app_icon() {
        // 2×2 基础图标(红) + 2×2 水印(蓝)
        let base = make_png(2, [255, 0, 0, 255]);
        let wm = make_png(2, [0, 0, 255, 255]);
        let out = compose_app_icon(&base, &wm).expect("compose failed");
        // 输出为有效 PNG(256×256)
        let img = image::load_from_memory(&out).expect("output not a valid image");
        assert_eq!(img.width(), 256);
        assert_eq!(img.height(), 256);
    }

    fn make_png(size: u32, pixel: [u8; 4]) -> Vec<u8> {
        use image::{ImageFormat, Rgba, RgbaImage};
        let img = RgbaImage::from_pixel(size, size, Rgba(pixel));
        let mut out = Vec::new();
        img.write_to(&mut std::io::Cursor::new(&mut out), ImageFormat::Png)
            .unwrap();
        out
    }
}
