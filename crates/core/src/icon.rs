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

// ============================================================================
// 自动生成图标（字符串种子 → GitHub identicon 风格像素图）
// ============================================================================

// 生成图标边长
const GEN_SIZE: u32 = 256;
// 网格规格（GitHub identicon 同款 5×5，左右镜像）
const GEN_GRID: usize = 5;
// 四周留白（像素块区 = GEN_SIZE − 2×MARGIN；256−36=220=5×44 整除无残边）
const GEN_MARGIN: u32 = 18;

/// FNV-1a 64 位哈希（跨进程/跨平台稳定——不用 DefaultHasher，它的跨版本
/// 稳定性无保证）。字符串种子 → PRNG 种子。
fn fnv1a64(s: &str) -> u64 {
    let mut h: u64 = 0xcbf29ce484222325;
    for b in s.as_bytes() {
        h ^= u64::from(*b);
        h = h.wrapping_mul(0x100000001b3);
    }
    h
}

/// splitmix64 PRNG（确定性；下一个伪随机数 + 均匀区间采样）。
struct SplitMix64(u64);

impl SplitMix64 {
    fn next_u64(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E3779B97F4A7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58476D1CE4E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D049BB133111EB);
        z ^ (z >> 31)
    }

    /// [0, n) 均匀整数
    fn below(&mut self, n: u64) -> u64 {
        self.next_u64() % n.max(1)
    }

    /// [0, 1) 均匀浮点
    fn unit(&mut self) -> f32 {
        (self.next_u64() >> 40) as f32 / (1u64 << 24) as f32
    }
}

/// HSL → RGB（h ∈ [0,360)，s/l ∈ [0,1]）。生成图标的前景色在 HSL 空间取：
/// 随机色相 + 受控饱和度/亮度 → 任意种子都清晰可辨。
fn hsl_to_rgb(h: f32, s: f32, l: f32) -> [u8; 3] {
    let c = (1.0 - (2.0 * l - 1.0).abs()) * s;
    let hp = h / 60.0;
    let x = c * (1.0 - (hp % 2.0 - 1.0).abs());
    let (r1, g1, b1) = match hp as u32 {
        0 => (c, x, 0.0),
        1 => (x, c, 0.0),
        2 => (0.0, c, x),
        3 => (0.0, x, c),
        4 => (x, 0.0, c),
        _ => (c, 0.0, x),
    };
    let m = l - c / 2.0;
    [
        ((r1 + m) * 255.0).round().clamp(0.0, 255.0) as u8,
        ((g1 + m) * 255.0).round().clamp(0.0, 255.0) as u8,
        ((b1 + m) * 255.0).round().clamp(0.0, 255.0) as u8,
    ]
}

/// 生成图标（确定性）：种子字符串 → FNV-1a → splitmix64 → GitHub identicon
/// 风格 5×5 像素图（左侧 3 列随机取格、镜像到右侧）。输出**满幅方形**不
/// 自带圆角——圆角由品牌包装 [`compose_app_icon`] 统一给（内容区裁角 +
/// 渐变边框），调用方直接用本函数输出时应自行决定是否包装。
/// 同一种子永远逐字节相同的 PNG。
///
/// 用途：容器没有可用图标源时的兜底视觉身份（同容器名 = 同图标，稳定可辨）。
pub fn generate_icon(seed: &str) -> Result<Vec<u8>> {
    let img = generate_icon_image(seed);
    let mut png = Vec::new();
    image::DynamicImage::ImageRgba8(img)
        .write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
        .map_err(|e| Error::Config(format!("生成图标 PNG 编码失败：{e}")))?;
    Ok(png)
}

/// 生成图标渲染主体（返回图像，供单测断言对称性/确定性）。
fn generate_icon_image(seed: &str) -> image::RgbaImage {
    let mut rng = SplitMix64(fnv1a64(seed));

    // 前景色：随机色相 + GitHub 风格固定饱和度/亮度（任何色相都清晰可辨）；
    // 背景浅灰（GitHub 同款 #F0F0F0 观感）
    let fg = hsl_to_rgb(rng.unit() * 360.0, 0.60, 0.42);
    let bg = [0xf0u8, 0xf0, 0xf0];

    // 5×5 取格：只随机左侧 3 列（含中列），镜像到右侧——保证左右对称
    let mut cells = [[false; GEN_GRID]; GEN_GRID];
    let mut any = false;
    for col in 0..=GEN_GRID / 2 {
        for cell_row in &mut cells {
            let on = rng.below(2) == 1;
            cell_row[col] = on;
            cell_row[GEN_GRID - 1 - col] = on;
            any |= on;
        }
    }
    // 全空兜底（概率 1/1024）：强制中列全亮，保证图标非空白
    if !any {
        for cell_row in &mut cells {
            cell_row[GEN_GRID / 2] = true;
        }
    }

    let mut img =
        image::RgbaImage::from_pixel(GEN_SIZE, GEN_SIZE, image::Rgba([bg[0], bg[1], bg[2], 255]));
    let cell = (GEN_SIZE - 2 * GEN_MARGIN) / GEN_GRID as u32; // 44，整除无累计误差
    for (row, cell_row) in cells.iter().enumerate() {
        for (col, &on) in cell_row.iter().enumerate() {
            if !on {
                continue;
            }
            let x0 = GEN_MARGIN + col as u32 * cell;
            let y0 = GEN_MARGIN + row as u32 * cell;
            for y in y0..y0 + cell {
                for x in x0..x0 + cell {
                    img.put_pixel(x, y, image::Rgba([fg[0], fg[1], fg[2], 255]));
                }
            }
        }
    }
    img
}

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
/// 把图标字节加载为 `image::DynamicImage`（image crate 支持的格式直接加载；
/// XPM 等 image 不支持的格式用自写解码器转 PNG 再加载）。
fn load_icon_to_image(bytes: &[u8]) -> Result<image::DynamicImage> {
    if let Ok(img) = image::load_from_memory(bytes) {
        return Ok(img);
    }
    // XPM：image crate 未启用 xpm 支持 → 自写解码器转 PNG 再加载
    if let Ok(png) = decode_xpm_to_png(bytes) {
        return image::load_from_memory(&png)
            .map_err(|e| Error::Config(format!("XPM 转 PNG 后解析失败：{e}")));
    }
    Err(Error::Config(
        "无法解析图标（支持 PNG/JPEG/BMP/ICO/TIFF/XPM）".to_string(),
    ))
}

pub fn compose_app_icon(base: &[u8], watermark: &[u8]) -> Result<Vec<u8>> {
    use image::{ImageFormat, Rgba, RgbaImage};

    // 基础图标 → 居中裁剪正方形 → 缩放 CONTENT×CONTENT
    let base_img = load_icon_to_image(base)?.to_rgba8();
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
                px as f32,
                py as f32,
                content_center,
                content_center,
                content_center,
                content_center,
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
                x as f32,
                y as f32,
                center,
                center,
                center,
                center,
                CORNER_RADIUS,
            );
            let a_out = (0.5 - d_out).clamp(0.0, 1.0);
            if a_out == 0.0 {
                continue;
            }
            let d_in = rounded_rect_sdf(
                x as f32,
                y as f32,
                inner_center,
                inner_center,
                inner_half,
                inner_half,
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
    image::imageops::overlay(
        &mut canvas,
        &wm,
        (SIZE - WATERMARK) as i64,
        (SIZE - WATERMARK) as i64,
    );

    // 编码 PNG
    let mut out = Vec::new();
    canvas
        .write_to(&mut std::io::Cursor::new(&mut out), ImageFormat::Png)
        .map_err(|e| Error::Config(format!("编码合成图标失败：{e}")))?;
    Ok(out)
}

// ============================================================================
// 浏览器可渲染格式转换（GUI 图标预览用）
// ============================================================================

/// 把图标字节转成**浏览器可直接渲染**的格式（GUI 预览用）。
///
/// WebKitGTK 无法在 `<img>` 里解码 XPM/TIFF/BMP/ICO，但能渲染 PNG/JPEG/
/// GIF/WebP/SVG。策略：
/// - 浏览器原生可渲染（png/jpg/jpeg/gif/webp/svg）：原样返回 + 正确 MIME。
/// - XPM：自写轻量解码器转 PNG。
/// - BMP/ICO/TIFF：经 image crate 转 PNG。
/// - 其它/解码失败：原样返回 + MIME（前端显示占位符，不黑不卡）。
pub fn to_browser_renderable(bytes: &[u8], path: &str) -> (Vec<u8>, String) {
    let ext = std::path::Path::new(path)
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_lowercase())
        .unwrap_or_default();
    match ext.as_str() {
        "png" => (bytes.to_vec(), "image/png".to_string()),
        "jpg" | "jpeg" => (bytes.to_vec(), "image/jpeg".to_string()),
        "gif" => (bytes.to_vec(), "image/gif".to_string()),
        "webp" => (bytes.to_vec(), "image/webp".to_string()),
        "svg" => (bytes.to_vec(), "image/svg+xml".to_string()),
        "bmp" | "ico" | "tif" | "tiff" => {
            if let Ok(img) = image::load_from_memory(bytes) {
                let mut out = Vec::new();
                if img
                    .write_to(&mut std::io::Cursor::new(&mut out), image::ImageFormat::Png)
                    .is_ok()
                {
                    return (out, "image/png".to_string());
                }
            }
            (bytes.to_vec(), mime_for_ext(&ext))
        }
        "xpm" => match decode_xpm_to_png(bytes) {
            Ok(png) => (png, "image/png".to_string()),
            Err(e) => {
                tracing::warn!("XPM 解码失败，回退原图（前端可能显示占位）：{e}");
                (bytes.to_vec(), "image/x-xpixmap".to_string())
            }
        },
        _ => (bytes.to_vec(), mime_for_ext(&ext)),
    }
}

fn mime_for_ext(ext: &str) -> String {
    match ext {
        "png" => "image/png".to_string(),
        "jpg" | "jpeg" => "image/jpeg".to_string(),
        "gif" => "image/gif".to_string(),
        "webp" => "image/webp".to_string(),
        "svg" => "image/svg+xml".to_string(),
        "bmp" => "image/bmp".to_string(),
        "ico" => "image/x-icon".to_string(),
        "xpm" => "image/x-xpixmap".to_string(),
        "tif" | "tiff" => "image/tiff".to_string(),
        _ => "application/octet-stream".to_string(),
    }
}

/// 轻量 XPM（X Pixel Map）解码器 → PNG 字节。
///
/// XPM 是文本格式：一个 C 字符串数组，首行 `width height colors cpp`，
/// 随后 colors 行颜色表（`name spec rgb`），再 height 行像素（每行
/// width*cpp 个字符）。仅支持常见子集（#RRGGBB/#RGB/#AARRGGBB/None/少量
/// 具名色；cpp=1 或 2）。
fn decode_xpm_to_png(data: &[u8]) -> Result<Vec<u8>> {
    use image::{ImageFormat, Rgba, RgbaImage};

    let text =
        std::str::from_utf8(data).map_err(|e| Error::Config(format!("XPM 非合法 UTF-8：{e}")))?;

    // 提取数组里每个引号字符串（= 每行）
    let mut lines: Vec<String> = Vec::new();
    let mut in_string = false;
    let mut current = String::new();
    for c in text.chars() {
        if c == '"' {
            if in_string {
                lines.push(std::mem::take(&mut current));
            }
            in_string = !in_string;
        } else if in_string {
            current.push(c);
        }
    }
    if lines.len() < 2 {
        return Err(Error::Config("XPM 格式无效（缺少行数据）".to_string()));
    }

    // 首行 header：width height colors cpp
    let h: Vec<&str> = lines[0].split_whitespace().collect();
    if h.len() < 4 {
        return Err(Error::Config("XPM header 无效".to_string()));
    }
    let width: u32 = h[0]
        .parse()
        .map_err(|_| Error::Config("XPM width 无效".to_string()))?;
    let height: u32 = h[1]
        .parse()
        .map_err(|_| Error::Config("XPM height 无效".to_string()))?;
    let colors: usize = h[2]
        .parse()
        .map_err(|_| Error::Config("XPM colors 无效".to_string()))?;
    let cpp: usize = h[3]
        .parse()
        .map_err(|_| Error::Config("XPM cpp 无效".to_string()))?;
    if cpp == 0 || cpp > 2 {
        return Err(Error::Config(format!("XPM cpp 不支持：{cpp}")));
    }

    // 颜色表：lines[1..=colors]。标准 XPM 行格式：
    //   `<key(cpp 字符)><空白>[<colortype: c/m/s/d/g>]<空白><colorvalue>`
    // key 取行首 cpp 个字符（可能是空格/点等，如透明色常为空白）；colorvalue
    // 取最后一个空白分隔 token（兼容可选的 colortype 前缀，如 `c #8DB0CE` / `None`）。
    let mut color_map: std::collections::HashMap<String, [u8; 4]> =
        std::collections::HashMap::new();
    let color_end = (1 + colors).min(lines.len());
    for line in &lines[1..color_end] {
        let key: String = line.chars().take(cpp).collect();
        if let Some(value) = line.split_whitespace().last() {
            color_map.insert(key, parse_xpm_color(value));
        }
    }

    // 像素行：从 (1+colors) 起 height 行
    let row_start = 1 + colors;
    if lines.len() < row_start + height as usize {
        return Err(Error::Config("XPM 像素行不足".to_string()));
    }

    let mut img = RgbaImage::new(width, height);
    for y in 0..height {
        let row = &lines[row_start + y as usize];
        for x in 0..width {
            let spec: String = row.chars().skip((x as usize) * cpp).take(cpp).collect();
            let rgba = color_map.get(&spec).copied().unwrap_or([0, 0, 0, 0]);
            img.put_pixel(x, y, Rgba(rgba));
        }
    }

    let mut out = Vec::new();
    img.write_to(&mut std::io::Cursor::new(&mut out), ImageFormat::Png)
        .map_err(|e| Error::Config(format!("XPM 编码 PNG 失败：{e}")))?;
    Ok(out)
}

/// 解析 XPM 颜色值：`#RRGGBB`/`#RGB`/`#AARRGGBB`/`None`/少量具名色。
fn parse_xpm_color(s: &str) -> [u8; 4] {
    let s = s.trim();
    if s.eq_ignore_ascii_case("none") {
        return [0, 0, 0, 0];
    }
    if let Some(hex) = s.strip_prefix('#') {
        let h = hex.trim();
        match h.len() {
            6 => [
                u8::from_str_radix(&h[0..2], 16).unwrap_or(0),
                u8::from_str_radix(&h[2..4], 16).unwrap_or(0),
                u8::from_str_radix(&h[4..6], 16).unwrap_or(0),
                255,
            ],
            3 => [
                u8::from_str_radix(&h[0..1], 16).unwrap_or(0) * 17,
                u8::from_str_radix(&h[1..2], 16).unwrap_or(0) * 17,
                u8::from_str_radix(&h[2..3], 16).unwrap_or(0) * 17,
                255,
            ],
            8 => [
                u8::from_str_radix(&h[2..4], 16).unwrap_or(0),
                u8::from_str_radix(&h[4..6], 16).unwrap_or(0),
                u8::from_str_radix(&h[6..8], 16).unwrap_or(0),
                u8::from_str_radix(&h[0..2], 16).unwrap_or(255),
            ],
            _ => [0, 0, 0, 0],
        }
    } else {
        match s.to_lowercase().as_str() {
            "black" => [0, 0, 0, 255],
            "white" => [255, 255, 255, 255],
            "red" => [255, 0, 0, 255],
            "green" => [0, 128, 0, 255],
            "blue" => [0, 0, 255, 255],
            "gray" | "grey" => [128, 128, 128, 255],
            "yellow" => [255, 255, 0, 255],
            "cyan" => [0, 255, 255, 255],
            "magenta" => [255, 0, 255, 255],
            "orange" => [255, 165, 0, 255],
            "purple" => [128, 0, 128, 255],
            _ => [0, 0, 0, 0],
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_generate_icon_deterministic() {
        // 同一种子 → 逐字节相同（跨调用稳定；FNV/splitmix 无随机源）
        let a = generate_icon("chrome").unwrap();
        let b = generate_icon("chrome").unwrap();
        assert_eq!(a, b);

        // 不同种子 → 不同图标
        let c = generate_icon("firefox").unwrap();
        assert_ne!(a, c);
    }

    #[test]
    fn test_generate_icon_shape_square_opaque() {
        // 输出满幅方形不透明背景（圆角交给品牌包装 compose_app_icon 统一做）
        let png = generate_icon("chrome").unwrap();
        let img = image::load_from_memory(&png).unwrap().to_rgba8();
        assert_eq!(img.dimensions(), (256, 256));
        for (x, y) in [(0, 0), (255, 0), (0, 255), (255, 255)] {
            assert_eq!(img.get_pixel(x, y).0[3], 255, "角 ({x},{y}) 应不透明");
        }
        assert_eq!(img.get_pixel(128, 128).0[3], 255);
    }

    #[test]
    fn test_generate_icon_mirror_symmetry() {
        // identicon 左右镜像：逐行比对中轴两侧像素颜色一致（留白区内）
        let img = generate_icon_image("chrome");
        let m = GEN_MARGIN;
        let cell = (GEN_SIZE - 2 * m) / GEN_GRID as u32;
        for row in 0..GEN_GRID as u32 {
            let y = m + row * cell + cell / 2;
            for col in 0..(GEN_GRID as u32 / 2) {
                let xl = m + col * cell + cell / 2;
                let xr = GEN_SIZE - m - (col + 1) * cell + cell / 2;
                assert_eq!(img.get_pixel(xl, y).0[3], img.get_pixel(xr, y).0[3]);
                assert_eq!(img.get_pixel(xl, y).0[0], img.get_pixel(xr, y).0[0]);
            }
        }
    }

    #[test]
    fn test_generate_icon_non_empty() {
        // 任何种子都至少有一格前景色（不会生成空白图标）
        for seed in ["a", "chrome", "zzz"] {
            let img = generate_icon_image(seed);
            let bg = [0xf0u8, 0xf0, 0xf0];
            let has_fg = img.pixels().any(|p| p.0[0..3] != bg && p.0[3] == 255);
            assert!(has_fg, "seed={seed} 不应生成空白图标");
        }
    }

    #[test]
    fn test_fnv1a64_known_vectors() {
        // FNV-1a 64 标准测试向量（跨平台稳定性的锚点）
        assert_eq!(fnv1a64(""), 0xcbf29ce484222325);
        assert_eq!(fnv1a64("a"), 0xaf63dc4c8601ec8c);
        assert_eq!(fnv1a64("foobar"), 0x85944171f73967e8);
    }

    #[test]
    fn test_hsl_to_rgb_basics() {
        assert_eq!(hsl_to_rgb(0.0, 1.0, 0.5), [255, 0, 0]);
        assert_eq!(hsl_to_rgb(120.0, 1.0, 0.5), [0, 255, 0]);
        assert_eq!(hsl_to_rgb(240.0, 1.0, 0.5), [0, 0, 255]);
    }

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
        assert_eq!(
            img.get_pixel(24, 24)[3],
            255,
            "内容区角部应为边框渐变(内圆角)"
        );
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

    #[test]
    fn test_compose_app_icon_real_xpm() {
        // 真实系统 XPM（cpp=2，316 色）——image crate 不支持 xpm，走自写解码器。
        // 文件不存在（非本机 / CI）时跳过。
        let path = "/usr/share/pixmaps/python3.13.xpm";
        let Ok(data) = std::fs::read(path) else {
            eprintln!("skip: {path} 不存在");
            return;
        };
        let wm = make_png(2, [0, 0, 0, 255]);
        let out = compose_app_icon(&data, &wm).expect("真实 XPM 应能经自写解码器加工");
        let img = image::load_from_memory(&out).expect("输出应为有效 PNG");
        assert_eq!((img.width(), img.height()), (256, 256));
    }

    #[test]
    fn test_decode_xpm_to_png() {
        // 2×2 XPM：cpp=1，三键（.→黑，x→红，o→透明）。标准格式 `<key><空白><value>`
        let xpm = "/* XPM */\nstatic char * t[] = {\n\"2 2 3 1\",\n\".  #000000\",\n\"x  #ff0000\",\n\"o  None\",\n\".x\",\n\"xo\"\n};\n";
        let png = decode_xpm_to_png(xpm.as_bytes()).expect("xpm decode failed");
        use image::{GenericImageView, Rgba};
        let img = image::load_from_memory(&png).expect("not a valid png");
        assert_eq!((img.width(), img.height()), (2, 2));
        // (0,0)=. → black；(1,0)=x → red；(0,1)=x → red；(1,1)=o → None(透明)
        assert_eq!(img.get_pixel(0, 0), Rgba([0, 0, 0, 255]));
        assert_eq!(img.get_pixel(1, 0), Rgba([255, 0, 0, 255]));
        assert_eq!(img.get_pixel(0, 1), Rgba([255, 0, 0, 255]));
        assert_eq!(img.get_pixel(1, 1), Rgba([0, 0, 0, 0]));
    }

    #[test]
    fn test_decode_xpm_cpp2_with_colortype() {
        // 2×2 XPM：cpp=2，带 colortype 前缀（`c #RRGGBB` / `c None`）——真实 XPM 常见格式。
        // 键为 2 字符：`  `(两空格,透明)、`. `(点+空格,蓝)。旧解析器按空白分列取
        // parts[1]/parts[2]，会把带 `c` 前缀的行当两 token 而丢弃 → 整图全透明。
        let xpm = "/* XPM */\nstatic char * t[] = {\n\"2 2 2 2\",\n\"  c None\",\n\". c #4985B7\",\n\"  . \",\n\". . \"\n};\n";
        let png = decode_xpm_to_png(xpm.as_bytes()).expect("xpm decode failed");
        use image::{GenericImageView, Rgba};
        let img = image::load_from_memory(&png).expect("not a valid png");
        assert_eq!((img.width(), img.height()), (2, 2));
        assert_eq!(img.get_pixel(0, 0), Rgba([0, 0, 0, 0])); // "  " 透明
        assert_eq!(img.get_pixel(1, 0), Rgba([0x49, 0x85, 0xB7, 255])); // ". " 蓝
        assert_eq!(img.get_pixel(0, 1), Rgba([0x49, 0x85, 0xB7, 255]));
        assert_eq!(img.get_pixel(1, 1), Rgba([0x49, 0x85, 0xB7, 255]));
    }

    #[test]
    fn test_to_browser_renderable_passthrough_and_xpm() {
        // png 原样返回
        let png = make_png(2, [1, 2, 3, 255]);
        let (b, mime) = to_browser_renderable(&png, "/a/icon.png");
        assert_eq!(mime, "image/png");
        assert_eq!(b, png);
        // xpm 转成 png
        let xpm = "static char * t[] = {\"1 1 1 1\",\"c #00000 c #ff0000\",\"c\"};\n";
        let (b, mime) = to_browser_renderable(xpm.as_bytes(), "/a/icon.xpm");
        assert_eq!(mime, "image/png");
        let img = image::load_from_memory(&b).expect("xpm→png invalid");
        assert_eq!((img.width(), img.height()), (1, 1));
    }

    #[test]
    fn test_real_xpm_conversion() {
        // 环境依赖：找一个真实 XPM 文件（找不到就跳过，不阻断 CI）
        let xpm_path = find_a_xpm();
        let Some(path) = xpm_path else { return };
        let bytes = std::fs::read(&path).expect("read xpm");
        let (png, mime) = to_browser_renderable(&bytes, path.to_str().unwrap_or_default());
        assert_eq!(mime, "image/png", "XPM 应转成 PNG");
        let img = image::load_from_memory(&png).expect("xpm→png 不是有效 PNG");
        assert!(img.width() > 0 && img.height() > 0);
        // 关键回归：转换后必须有非透明像素。颜色表解析失败（如漏掉 colortype
        // 前缀 `c`）会使整图全透明——浏览器显示空白，而尺寸检查却过不了关。
        use image::GenericImageView;
        let mut opaque = 0u32;
        for y in 0..img.height() {
            for x in 0..img.width() {
                if img.get_pixel(x, y).0[3] > 0 {
                    opaque += 1;
                }
            }
        }
        assert!(
            opaque > 0,
            "转换后全透明——颜色表未解析（colortype 前缀 bug）"
        );
        eprintln!(
            "real xpm ok: {} → {}x{} ({} 非透明像素)",
            path.display(),
            img.width(),
            img.height(),
            opaque
        );
    }

    fn find_a_xpm() -> Option<std::path::PathBuf> {
        fn search(dir: &std::path::Path, depth: u32) -> Option<std::path::PathBuf> {
            if depth == 0 {
                return None;
            }
            let entries = std::fs::read_dir(dir).ok()?;
            for e in entries.flatten() {
                let p = e.path();
                if p.is_dir() {
                    if let Some(f) = search(&p, depth - 1) {
                        return Some(f);
                    }
                } else if p.extension().map(|x| x == "xpm").unwrap_or(false) {
                    return Some(p);
                }
            }
            None
        }
        // 只查几个已知可能有 XPM 的目录，限深 4，找到即停（快、可预测）
        for root in [
            "/usr/share/pixmaps",
            "/usr/share/ghostscript",
            "/usr/share/icons",
        ] {
            if let Some(f) = search(std::path::Path::new(root), 4) {
                return Some(f);
            }
        }
        None
    }
}
