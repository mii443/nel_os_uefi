use ab_glyph::{Font, FontRef, ScaleFont};
use alloc::vec::Vec;
use lazy_static::lazy_static;
use nel_os_common::gop::{FrameBuffer as RawFrameBuffer, PixelFormat as RawPixelFormat};
use spin::Mutex;

static FONT: &[u8] = include_bytes!("../Tamzen7x14r.ttf");

lazy_static! {
    pub static ref FRAME_BUFFER: Mutex<Option<FrameBuffer>> = Mutex::new(None);
}

pub enum PixelFormat {
    Rgb,
    Bgr,
}

pub struct FrameBuffer {
    pub frame_buffer: *mut u8,
    pub width: usize,
    pub height: usize,
    pub stride: usize,

    pub pixel_format: PixelFormat,

    pub background_color: (u8, u8, u8),

    pub text_cursor: (usize, usize),

    pub text_buffer: Vec<Vec<char>>,
    pub textscreen_width: usize,
    pub textscreen_height: usize,
}

unsafe impl Send for FrameBuffer {}
unsafe impl Sync for FrameBuffer {}

impl FrameBuffer {
    pub fn from_raw_buffer(raw_buffer: &RawFrameBuffer, background_color: (u8, u8, u8)) -> Self {
        Self {
            frame_buffer: raw_buffer.frame_buffer,
            width: raw_buffer.width,
            height: raw_buffer.height,

            stride: raw_buffer.stride,
            pixel_format: match raw_buffer.pixl_format {
                RawPixelFormat::Rgb => PixelFormat::Rgb,
                RawPixelFormat::Bgr => PixelFormat::Bgr,
            },
            background_color,
            text_cursor: (0, 0),
            text_buffer: Vec::new(),
            textscreen_width: raw_buffer.width / 8,
            textscreen_height: raw_buffer.height / 14,
        }
    }

    pub fn pixel_index(&self, x: usize, y: usize) -> Option<usize> {
        if x < self.width && y < self.height {
            Some((y * self.stride + x) * 4)
        } else {
            None
        }
    }

    pub fn draw_pixel(&self, r: u8, g: u8, b: u8, x: usize, y: usize) {
        if x >= self.width || y >= self.height {
            return;
        }

        let pixel_index = self.pixel_index(x, y).unwrap();
        unsafe {
            let pixel_ptr = self.frame_buffer.add(pixel_index);
            match self.pixel_format {
                PixelFormat::Rgb => {
                    *pixel_ptr.add(0) = r;
                    *pixel_ptr.add(1) = g;
                    *pixel_ptr.add(2) = b;
                }

                PixelFormat::Bgr => {
                    *pixel_ptr.add(0) = b;
                    *pixel_ptr.add(1) = g;
                    *pixel_ptr.add(2) = r;
                }
            }
        }
    }

    pub fn clear(&self) {
        let (r, g, b) = self.background_color;
        for y in 0..self.height {
            for x in 0..self.width {
                self.draw_pixel(r, g, b, x, y);
            }
        }
    }

    pub fn update_textscreen(&mut self) {
        self.clear();
        for (y, line) in self.text_buffer.iter().enumerate() {
            let mut x = 0;
            for &c in line {
                self.draw_char(c, x, y * 14);
                x += 8;
            }
        }
    }

    pub fn add_text_line(&mut self, text: &str) {
        if self.text_buffer.len() >= self.textscreen_height {
            self.text_buffer.remove(0);
        }
        self.text_buffer.push(text.chars().collect());
    }

    pub fn clear_text(&mut self) {
        self.text_cursor = (0, 0);
        self.text_buffer.clear();
        self.clear();
    }

    pub fn print_text(&mut self, text: &str) {
        let lines = text.lines();
        for line in lines {
            self.add_text_line(line);
        }
        self.update_textscreen();
    }

    pub fn draw_char(&self, c: char, x: usize, y: usize) {
        let font = FontRef::try_from_slice(FONT).unwrap();

        let font = font.as_scaled(14.0);

        let mut glyph = font.scaled_glyph(c);
        glyph.position = ab_glyph::point(0.0, font.ascent());
        if let Some(glyph) = font.outline_glyph(glyph) {
            let min_x = glyph.px_bounds().min.x as usize;

            let min_y = glyph.px_bounds().min.y as usize;

            glyph.draw(|fx, fy, c| {
                let pixel_x = fx + min_x as u32 + x as u32;
                let pixel_y = fy + min_y as u32 + y as u32;
                let color = if c > 0.0 { (c * 255.0) as u8 } else { 64 };

                self.draw_pixel(color, color, color, pixel_x as usize, pixel_y as usize);
            });
        }
    }
}
