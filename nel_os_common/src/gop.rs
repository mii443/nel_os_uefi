pub enum PixelFormat {
    Rgb,
    Bgr,
}

pub struct FrameBuffer {
    pub frame_buffer: *mut u8,
    pub width: usize,
    pub height: usize,
    pub stride: usize,
    pub pixl_format: PixelFormat,
}
