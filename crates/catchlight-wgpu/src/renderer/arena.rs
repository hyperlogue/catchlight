/// A GPU buffer filled front to back within one submit.
///
/// `write_buffer` lands before the whole submit, so an offset written twice
/// reads as the later write for every draw. Each frame therefore stages its
/// bytes and uploads them once, and a later frame in the same submit continues
/// where the previous one stopped.
pub(super) struct FrameArena {
    buffer: wgpu::Buffer,
    label: &'static str,
    usage: wgpu::BufferUsages,
    min_capacity: u64,
    capacity: u64,
    cursor: u64,
    /// Where this frame's bytes start; `staging[0]` lands here.
    frame_base: u64,
    watermark: u64,
    staging: Vec<u8>,
    staging_len: usize,
}

impl FrameArena {
    pub(super) fn new(
        device: &wgpu::Device,
        label: &'static str,
        usage: wgpu::BufferUsages,
        capacity: u64,
    ) -> Self {
        Self {
            buffer: Self::create(device, label, usage, capacity),
            label,
            usage,
            min_capacity: capacity,
            capacity,
            cursor: 0,
            frame_base: 0,
            watermark: 0,
            staging: Vec::new(),
            staging_len: 0,
        }
    }

    fn create(
        device: &wgpu::Device,
        label: &'static str,
        usage: wgpu::BufferUsages,
        size: u64,
    ) -> wgpu::Buffer {
        device.create_buffer(&wgpu::BufferDescriptor {
            label: Some(label),
            size,
            usage: usage | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        })
    }

    pub(super) fn buffer(&self) -> &wgpu::Buffer {
        &self.buffer
    }

    /// `needed` must cover every reservation of the frame: growing mid-frame
    /// would strand recorded passes on the old buffer. `continues` keeps the
    /// bytes of earlier frames in the same submit. Returns whether the buffer
    /// was replaced; passes recorded earlier keep the old one, already written.
    pub(super) fn begin_frame(
        &mut self,
        device: &wgpu::Device,
        needed: u64,
        continues: bool,
    ) -> bool {
        let mut base = if continues { self.cursor } else { 0 };
        let grew = base + needed > self.capacity;
        if grew {
            // Sized for the whole submit, so its next run fits without growing.
            self.capacity = ((base + needed) * 2).max(self.min_capacity);
            self.buffer = Self::create(device, self.label, self.usage, self.capacity);
            base = 0;
        }
        self.cursor = base;
        self.frame_base = base;
        self.watermark = base;
        self.staging_len = 0;
        grew
    }

    pub(super) fn reserve(&mut self, bytes: u64) -> u64 {
        let offset = self.cursor;
        self.cursor += bytes;
        debug_assert!(
            self.cursor <= self.capacity,
            "{} overrun — begin_frame undersized: cursor={} cap={}",
            self.label,
            self.cursor,
            self.capacity,
        );
        offset
    }

    pub(super) fn write(&mut self, offset: u64, bytes: &[u8]) {
        debug_assert!(
            offset >= self.watermark,
            "{} offset {offset} rewrites bytes already staged this frame (watermark {}); \
             every draw reading that range would see the later write",
            self.label,
            self.watermark,
        );
        self.watermark = offset + bytes.len() as u64;
        let start = (offset - self.frame_base) as usize;
        let end = start + bytes.len();
        if self.staging.len() < end {
            self.staging.resize(end, 0);
        }
        self.staging[start..end].copy_from_slice(bytes);
        self.staging_len = self.staging_len.max(end);
    }

    /// Returns the bytes uploaded.
    pub(super) fn flush(&mut self, queue: &wgpu::Queue) -> u64 {
        let len = self.staging_len;
        if len > 0 {
            queue.write_buffer(&self.buffer, self.frame_base, &self.staging[..len]);
        }
        self.staging_len = 0;
        len as u64
    }
}
