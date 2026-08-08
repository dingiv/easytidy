//! 应用图标合成（easytidy 品牌风格）。
//!
//! 从容器拿到基础图标 → 居中裁剪正方形 → 缩放为 208px 内容区(24px 边框),
//! 外围使用天蓝色 → 深蓝色 45° 线性渐变**圆角**边框包边(外角 48px / 内角 24px
//! 同心圆弧),内容区同样裁 24px 圆角,右下角附 easytidy 水印(96×96)。
//! 输出 256×256 PNG。
//!
//! 圆角实现:圆角矩形 SDF(有符号距离场)逐像素判断——image crate 无圆角
//! 绘制 API,imageproc 只能画纯色(渐变边框用不上),SDF 零依赖且边缘
//! 按距离渐变 alpha 自带抗锯齿。

use crate::error::{Error, Result};

/// 合成尺寸
const SIZE: u32 = 256;
/// 内容区边长(边框 24px)
const CONTENT: u32 = 208;
/// 边框宽度((SIZE - CONTENT) / 2)
const BORDER: i64 = ((SIZE - CONTENT) / 2) as i64;
/// 边框圆角半径(GNOME 图标比例 ≈ 256×0.19)
const CORNER_RADIUS: f32 = 48.0;
/// 内容区圆角半径(与外角同心:CORNER_RADIUS − BORDER)
const INNER_RADIUS: f32 = 24.0;
/// 水印尺寸(右下角)
const WATERMARK: u32 = 96;

/// 天蓝色(渐变起点,左上)
const SKY: [u8; 3] = [0x6a, 0xa2, 0xe7];
/// 深蓝色(渐变终点,右下)
const DEEP: [u8; 3] = [0x1e, 0x66, 0xf5];

/// 圆角矩形有符号距离场(SDF,负值 = 内部)。
///
/// 标准 Inigo Quilez 公式:把点折到第一象限,圆角部分用角点圆弧判定。
/// 对逐像素合成,`d` 直接当抗锯齿用:边缘 ±0.5px 内按 `0.5 - d` 渐变 alpha。
fn rounded_rect_sdf(px: f32, py: f32, cx: f32, cy: f32, hw: f32, hh: f32, radius: f32) -> f32 {
    let qx = (px - cx).abs() - (hw - radius);
    let qy = (py - cy).abs() - (hh - radius);
    let ax = qx.max(0.0);
    let ay = qy.max(0.0);
    (ax * ax + ay * ay).sqrt() + qx.max(qy).min(0.0) - radius
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
    let mut content = image::imageops::resize(
        &cropped.to_image(),
        CONTENT,
        CONTENT,
        image::imageops::FilterType::Lanczos3,
    );

    // 内容裁圆角(与边框内沿同半径):蒙版 alpha 乘进内容图,角部透明——
    // 半透明/不规则基础图标不再在内容区露出直角
    let content_center = CONTENT as f32 / 2.0; // 104.0(内容图像素中心坐标)
    for py in 0..CONTENT {
        for px in 0..CONTENT {
            let d = rounded_rect_sdf(
                px as f32, py as f32,
                content_center, content_center,
                content_center, content_center,
                INNER_RADIUS,
            );
            let a = (0.5 - d).clamp(0.0, 1.0);
            if a == 1.0 {
                continue;
            }
            let p = content.get_pixel_mut(px, py);
            p[3] = (p[3] as f32 * a) as u8;
        }
    }

    // 画布:45° 渐变圆角边框(左上天空蓝 → 右下深蓝,t = (x+y)/对角和;
    // 边框 = 外圆角矩形 − 内圆角矩形,SDF 覆盖度相乘,边缘 0.5px 距离渐变抗锯齿)
    let mut canvas = RgbaImage::new(SIZE, SIZE);
    let max_t = 2.0 * (SIZE - 1) as f32;
    let center = (SIZE - 1) as f32 / 2.0; // 127.5(外框像素中心坐标)
    let inner_center = BORDER as f32 + CONTENT as f32 / 2.0; // 128.0(内孔圆角矩形中心)
    let inner_half = CONTENT as f32 / 2.0; // 104.0
    for y in 0..SIZE {
        for x in 0..SIZE {
            // 外圆角矩形覆盖度 × 内孔未覆盖度 = 边框像素(外角/内沿均抗锯齿)
            let d_out = rounded_rect_sdf(
                x as f32, y as f32,
                center, center,
                center, center,
                CORNER_RADIUS,
            );
            let a_out = (0.5 - d_out).clamp(0.0, 1.0);
            if a_out == 0.0 {
                continue;
            }
            let d_in = rounded_rect_sdf(
                x as f32, y as f32,
                inner_center, inner_center,
                inner_half, inner_half,
                INNER_RADIUS,
            );
            let a_in = (0.5 - d_in).clamp(0.0, 1.0); // 内孔覆盖度
            let a = a_out * (1.0 - a_in);
            if a == 0.0 {
                continue;
            }

            let t = (x + y) as f32 / max_t;
            let r = SKY[0] as f32 + (DEEP[0] as f32 - SKY[0] as f32) * t;
            let g = SKY[1] as f32 + (DEEP[1] as f32 - SKY[1] as f32) * t;
            let b = SKY[2] as f32 + (DEEP[2] as f32 - SKY[2] as f32) * t;

            canvas.put_pixel(x, y, Rgba([r as u8, g as u8, b as u8, (a * 255.0) as u8]));
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
        use image::GenericImageView;
        let img = image::load_from_memory(&out).expect("output not a valid image");
        assert_eq!(img.width(), 256);
        assert_eq!(img.height(), 256);
        // 圆角:外角透明 + 内孔角部由边框渐变填充(内容裁圆角) + 中心内容不透明
        assert_eq!(img.get_pixel(0, 0)[3], 0, "左上角应为透明(外圆角)");
        assert_eq!(img.get_pixel(255, 0)[3], 0, "右上角应为透明(外圆角)");
        assert_eq!(img.get_pixel(24, 24)[3], 255, "内容区角部应为边框渐变(内圆角)");
        assert_eq!(img.get_pixel(127, 127)[3], 255, "中心应为不透明(内容区)");
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
