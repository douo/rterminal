const MAX_SIXEL_DIMENSION: usize = 8192;
const MAX_SIXEL_PIXELS: usize = 32 * 1024 * 1024;

#[derive(Clone, Debug)]
pub struct SixelImage {
    pub row: usize,
    pub col: usize,
    pub width: usize,
    pub height: usize,
    /// RGBA pixels in row-major order.
    pub rgba: Vec<u8>,
}

pub fn decode_sixel_payload(row: usize, col: usize, bytes: &[u8]) -> Option<SixelImage> {
    let mut decoder = SixelDecoder::new(row, col);
    decoder.decode(bytes)
}

pub fn decode_tmux_passthrough_sixel(row: usize, col: usize, bytes: &[u8]) -> Option<SixelImage> {
    let payload = bytes.strip_prefix(b"mux;")?;
    let mut unescaped = Vec::with_capacity(payload.len());
    let mut i = 0;
    while i < payload.len() {
        if payload[i] == 0x1b && payload.get(i + 1) == Some(&0x1b) {
            unescaped.push(0x1b);
            i += 2;
        } else {
            unescaped.push(payload[i]);
            i += 1;
        }
    }

    let start = unescaped.windows(2).position(|w| w == b"\x1bP")? + 2;
    let end = unescaped[start..]
        .windows(2)
        .position(|w| w == b"\x1b\\")
        .map(|offset| start + offset)
        .unwrap_or(unescaped.len());
    let dcs = &unescaped[start..end];
    let q = dcs.iter().position(|byte| *byte == b'q')?;
    decode_sixel_payload(row, col, &dcs[q + 1..])
}

struct SixelDecoder {
    row: usize,
    col: usize,
    x: usize,
    y: usize,
    width: usize,
    height: usize,
    max_x: usize,
    max_y: usize,
    declared_width: Option<usize>,
    declared_height: Option<usize>,
    color_index: usize,
    palette: [[u8; 4]; 256],
    pixels: Vec<u8>,
}

impl SixelDecoder {
    fn new(row: usize, col: usize) -> Self {
        Self {
            row,
            col,
            x: 0,
            y: 0,
            width: 0,
            height: 0,
            max_x: 0,
            max_y: 0,
            declared_width: None,
            declared_height: None,
            color_index: 0,
            palette: default_palette(),
            pixels: Vec::new(),
        }
    }

    fn decode(&mut self, bytes: &[u8]) -> Option<SixelImage> {
        let mut i = 0;
        let mut repeat = 1usize;

        while i < bytes.len() {
            match bytes[i] {
                b'#' => {
                    let (params, next) = parse_params(bytes, i + 1);
                    self.apply_color_params(&params);
                    i = next;
                }
                b'!' => {
                    let (count, next) = parse_number(bytes, i + 1);
                    repeat = count.unwrap_or(1).max(1).min(MAX_SIXEL_DIMENSION);
                    i = next;
                }
                b'"' => {
                    let (params, next) = parse_params(bytes, i + 1);
                    self.apply_raster_attributes(&params);
                    i = next;
                }
                b'$' => {
                    self.x = 0;
                    i += 1;
                }
                b'-' => {
                    self.x = 0;
                    self.y = self.y.saturating_add(6);
                    i += 1;
                }
                byte @ 0x3f..=0x7e => {
                    self.draw_sixel(byte - 0x3f, repeat);
                    repeat = 1;
                    i += 1;
                }
                _ => i += 1,
            }
        }

        self.finish()
    }

    fn apply_color_params(&mut self, params: &[usize]) {
        let Some(&index) = params.first() else {
            return;
        };
        let index = index.min(255);
        self.color_index = index;

        if params.len() >= 5 {
            self.palette[index] = match params[1] {
                1 => hls_to_rgba(params[2], params[3], params[4]),
                2 => [
                    percent_to_byte(params[2]),
                    percent_to_byte(params[3]),
                    percent_to_byte(params[4]),
                    0xff,
                ],
                _ => self.palette[index],
            };
        }
    }

    fn apply_raster_attributes(&mut self, params: &[usize]) {
        if params.len() < 4 {
            return;
        }

        let width = params[2].min(MAX_SIXEL_DIMENSION);
        let height = params[3].min(MAX_SIXEL_DIMENSION);
        if width > 0 && height > 0 && width.saturating_mul(height) <= MAX_SIXEL_PIXELS {
            self.declared_width = Some(width);
            self.declared_height = Some(height);
            self.ensure_canvas(width, height);
        }
    }

    fn draw_sixel(&mut self, bits: u8, repeat: usize) {
        for _ in 0..repeat {
            for bit in 0..6 {
                if bits & (1 << bit) != 0 {
                    self.set_pixel(self.x, self.y + bit);
                }
            }
            self.x = self.x.saturating_add(1);
        }
    }

    fn set_pixel(&mut self, x: usize, y: usize) {
        if x >= MAX_SIXEL_DIMENSION || y >= MAX_SIXEL_DIMENSION {
            return;
        }
        if x.saturating_add(1).saturating_mul(y.saturating_add(1)) > MAX_SIXEL_PIXELS {
            return;
        }

        self.ensure_canvas(x + 1, y + 1);
        if x >= self.width || y >= self.height {
            return;
        }

        let offset = (y * self.width + x) * 4;
        self.pixels[offset..offset + 4].copy_from_slice(&self.palette[self.color_index]);
        self.max_x = self.max_x.max(x + 1);
        self.max_y = self.max_y.max(y + 1);
    }

    fn ensure_canvas(&mut self, min_width: usize, min_height: usize) {
        if min_width <= self.width && min_height <= self.height {
            return;
        }

        let target_width = grow_dimension(self.width, min_width, self.declared_width);
        let target_height = grow_dimension(self.height, min_height, self.declared_height);
        if target_width.saturating_mul(target_height) > MAX_SIXEL_PIXELS {
            return;
        }

        if self.width == 0 || self.height == 0 {
            self.width = target_width;
            self.height = target_height;
            self.pixels = vec![0; target_width * target_height * 4];
            return;
        }

        let mut next = vec![0; target_width * target_height * 4];
        for y in 0..self.height.min(target_height) {
            let old_start = y * self.width * 4;
            let new_start = y * target_width * 4;
            let len = self.width.min(target_width) * 4;
            next[new_start..new_start + len].copy_from_slice(&self.pixels[old_start..old_start + len]);
        }

        self.width = target_width;
        self.height = target_height;
        self.pixels = next;
    }

    fn finish(&self) -> Option<SixelImage> {
        let width = self.declared_width.unwrap_or(self.max_x).max(self.max_x);
        let height = self.declared_height.unwrap_or(self.max_y).max(self.max_y);
        if width == 0 || height == 0 || width > self.width || height > self.height {
            return None;
        }

        let mut rgba = vec![0; width * height * 4];
        for y in 0..height {
            let source = y * self.width * 4;
            let target = y * width * 4;
            rgba[target..target + width * 4].copy_from_slice(&self.pixels[source..source + width * 4]);
        }

        Some(SixelImage {
            row: self.row,
            col: self.col,
            width,
            height,
            rgba,
        })
    }
}

fn grow_dimension(current: usize, needed: usize, declared: Option<usize>) -> usize {
    let declared = declared.unwrap_or(0);
    let doubled = current.saturating_mul(2).max(64);
    needed.max(declared).max(doubled).min(MAX_SIXEL_DIMENSION)
}

fn parse_params(bytes: &[u8], mut index: usize) -> (Vec<usize>, usize) {
    let mut params = Vec::new();
    let mut value = None::<usize>;

    while let Some(&byte) = bytes.get(index) {
        match byte {
            b'0'..=b'9' => {
                let next = value.unwrap_or(0).saturating_mul(10) + usize::from(byte - b'0');
                value = Some(next.min(MAX_SIXEL_DIMENSION));
                index += 1;
            }
            b';' => {
                params.push(value.take().unwrap_or(0));
                index += 1;
            }
            _ => break,
        }
    }

    if value.is_some() || !params.is_empty() {
        params.push(value.unwrap_or(0));
    }

    (params, index)
}

fn parse_number(bytes: &[u8], mut index: usize) -> (Option<usize>, usize) {
    let mut value = None::<usize>;
    while let Some(byte @ b'0'..=b'9') = bytes.get(index).copied() {
        value = Some(
            value
                .unwrap_or(0)
                .saturating_mul(10)
                .saturating_add(usize::from(byte - b'0'))
                .min(MAX_SIXEL_DIMENSION),
        );
        index += 1;
    }
    (value, index)
}

fn percent_to_byte(value: usize) -> u8 {
    ((value.min(100) * 255 + 50) / 100) as u8
}

fn hls_to_rgba(hue: usize, lightness: usize, saturation: usize) -> [u8; 4] {
    let h = (hue % 360) as f32 / 360.0;
    let l = lightness.min(100) as f32 / 100.0;
    let s = saturation.min(100) as f32 / 100.0;

    if s == 0.0 {
        let gray = (l * 255.0).round() as u8;
        return [gray, gray, gray, 0xff];
    }

    let q = if l < 0.5 {
        l * (1.0 + s)
    } else {
        l + s - l * s
    };
    let p = 2.0 * l - q;
    [
        unit_to_byte(hue_to_unit(p, q, h + 1.0 / 3.0)),
        unit_to_byte(hue_to_unit(p, q, h)),
        unit_to_byte(hue_to_unit(p, q, h - 1.0 / 3.0)),
        0xff,
    ]
}

fn hue_to_unit(p: f32, q: f32, mut t: f32) -> f32 {
    if t < 0.0 {
        t += 1.0;
    }
    if t > 1.0 {
        t -= 1.0;
    }
    if t < 1.0 / 6.0 {
        p + (q - p) * 6.0 * t
    } else if t < 1.0 / 2.0 {
        q
    } else if t < 2.0 / 3.0 {
        p + (q - p) * (2.0 / 3.0 - t) * 6.0
    } else {
        p
    }
}

fn unit_to_byte(value: f32) -> u8 {
    (value.clamp(0.0, 1.0) * 255.0).round() as u8
}

fn default_palette() -> [[u8; 4]; 256] {
    let mut palette = [[0, 0, 0, 0xff]; 256];
    let ansi = [
        [0x00, 0x00, 0x00, 0xff],
        [0xcd, 0x00, 0x00, 0xff],
        [0x00, 0xcd, 0x00, 0xff],
        [0xcd, 0xcd, 0x00, 0xff],
        [0x00, 0x00, 0xee, 0xff],
        [0xcd, 0x00, 0xcd, 0xff],
        [0x00, 0xcd, 0xcd, 0xff],
        [0xe5, 0xe5, 0xe5, 0xff],
        [0x7f, 0x7f, 0x7f, 0xff],
        [0xff, 0x00, 0x00, 0xff],
        [0x00, 0xff, 0x00, 0xff],
        [0xff, 0xff, 0x00, 0xff],
        [0x5c, 0x5c, 0xff, 0xff],
        [0xff, 0x00, 0xff, 0xff],
        [0x00, 0xff, 0xff, 0xff],
        [0xff, 0xff, 0xff, 0xff],
    ];
    palette[..ansi.len()].copy_from_slice(&ansi);

    for index in 16..232 {
        let value = index - 16;
        let r = value / 36;
        let g = (value / 6) % 6;
        let b = value % 6;
        palette[index] = [cube_component(r), cube_component(g), cube_component(b), 0xff];
    }

    for (offset, index) in (232..256).enumerate() {
        let gray = 8 + offset as u8 * 10;
        palette[index] = [gray, gray, gray, 0xff];
    }

    palette
}

fn cube_component(value: usize) -> u8 {
    if value == 0 {
        0
    } else {
        (55 + value * 40) as u8
    }
}

#[cfg(test)]
mod tests {
    use super::{decode_sixel_payload, decode_tmux_passthrough_sixel};

    #[test]
    fn decodes_rgb_sixel_payload() {
        let image = decode_sixel_payload(2, 3, br#""1;1;1;6#1;2;100;0;0~"#).unwrap();

        assert_eq!(image.row, 2);
        assert_eq!(image.col, 3);
        assert_eq!(image.width, 1);
        assert_eq!(image.height, 6);
        for pixel in image.rgba.chunks_exact(4) {
            assert_eq!(pixel, &[255, 0, 0, 255]);
        }
    }

    #[test]
    fn decodes_hls_sixel_palette_entries() {
        let image = decode_sixel_payload(0, 0, br#""1;1;1;6#1;1;0;50;100~"#).unwrap();

        for pixel in image.rgba.chunks_exact(4) {
            assert_eq!(pixel, &[255, 0, 0, 255]);
        }
    }

    #[test]
    fn decodes_tmux_passthrough_sixel() {
        let image =
            decode_tmux_passthrough_sixel(4, 5, b"mux;\x1b\x1bPq~\x1b\x1b\\").unwrap();

        assert_eq!(image.row, 4);
        assert_eq!(image.col, 5);
        assert_eq!(image.width, 1);
        assert_eq!(image.height, 6);
    }
}
