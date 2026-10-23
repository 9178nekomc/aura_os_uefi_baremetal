//! Bare-Metal UEFI GOP FrameBuffer & SDF Sub-Pixel Anti-Aliased Vector Compositor (`src/framebuffer.rs`)
//!
//! Eliminates jagged pixel-art edges by implementing Signed Distance Field (SDF) sub-pixel
//! coverage alpha blending for rounded rectangles, linear gradients, soft drop shadows,
//! anti-aliased borders, and smooth circles/rings.

#![allow(dead_code)]

extern crate alloc;
use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;
use uefi::proto::console::gop::{GraphicsOutput, PixelFormat};
use uefi::table::boot::{BootServices, OpenProtocolAttributes, OpenProtocolParams};
use uefi::Status;

use crate::memory::validate_gop_framebuffer_safety;

/// Fast integer square root in 16.4 fixed point (returns `sqrt(val * 256)` so sub-pixel precision is 1/16th pixel)
#[inline]
pub fn isqrt_fp4(val_fp8: u32) -> u32 {
    if val_fp8 == 0 {
        return 0;
    }
    let mut x = val_fp8;
    let mut y = (x + 1) >> 1;
    while y < x {
        x = y;
        y = (x + val_fp8 / x) >> 1;
    }
    x
}

#[inline]
pub fn lerp_rgb(c0: u32, c1: u32, t_256: u32) -> u32 {
    let t = t_256.min(256);
    let inv = 256 - t;
    let r = (((c0 >> 16) & 0xFF) * inv + ((c1 >> 16) & 0xFF) * t) >> 8;
    let g = (((c0 >> 8) & 0xFF) * inv + ((c1 >> 8) & 0xFF) * t) >> 8;
    let b = ((c0 & 0xFF) * inv + (c1 & 0xFF) * t) >> 8;
    (r << 16) | (g << 8) | b
}

pub struct GopFrameBuffer {
    vram_base_ptr: *mut u32,
    pub vram_byte_size: usize,
    pub width: usize,
    pub height: usize,
    pub stride: usize,
    pub is_bgr_format: bool,
    pub hardware_summary: String,
    pub backbuffer: Vec<u32>,
}

pub type GopDisplayDriver = GopFrameBuffer;

impl GopFrameBuffer {
    pub fn init_from_uefi(bt: &BootServices) -> Result<Self, Status> {
        let gop_handle = bt
            .get_handle_for_protocol::<GraphicsOutput>()
            .map_err(|e| e.status())?;

        let mut gop = unsafe {
            bt.open_protocol::<GraphicsOutput>(
                OpenProtocolParams {
                    handle: gop_handle,
                    agent: bt.image_handle(),
                    controller: None,
                },
                OpenProtocolAttributes::GetProtocol,
            )
            .map_err(|e| e.status())?
        };

        // Prefer 1280x800, 1280x720, or 1024x768 for crisp high-DPI desktop layout
        let mut preferred_mode = None;
        let mut fallback_mode = None;

        for mode in gop.modes(bt) {
            let info = mode.info();
            let fmt = info.pixel_format();
            if !matches!(fmt, PixelFormat::Rgb | PixelFormat::Bgr) {
                continue;
            }
            let (w, h) = info.resolution();
            if w == 1280 && (h == 800 || h == 720) {
                preferred_mode = Some(mode);
                break;
            }
            if w == 1024 && h == 768 {
                fallback_mode = Some(mode);
            } else if fallback_mode.is_none() && w >= 800 && h >= 600 {
                fallback_mode = Some(mode);
            }
        }

        if let Some(target_mode) = preferred_mode.or(fallback_mode) {
            let _ = gop.set_mode(&target_mode);
        }

        let mode_info = gop.current_mode_info();
        let (width, height) = mode_info.resolution();
        let stride = mode_info.stride();
        let is_bgr_format = matches!(mode_info.pixel_format(), PixelFormat::Bgr);

        let mut fb = gop.frame_buffer();
        let vram_base_ptr = fb.as_mut_ptr() as *mut u32;
        let vram_byte_size = fb.size();

        let hardware_summary = validate_gop_framebuffer_safety(
            vram_base_ptr as usize,
            vram_byte_size,
            width,
            height,
            stride,
        )
        .map_err(|_| Status::DEVICE_ERROR)?;

        let backbuffer = vec![0xF8FAFCu32; width.saturating_mul(height)];

        let mut instance = Self {
            vram_base_ptr,
            vram_byte_size,
            width,
            height,
            stride,
            is_bgr_format,
            hardware_summary,
            backbuffer,
        };

        instance.present();
        Ok(instance)
    }

    #[inline]
    pub fn backbuffer_mut(&mut self) -> &mut [u32] {
        &mut self.backbuffer
    }

    #[inline]
    pub fn put_pixel(&mut self, x: usize, y: usize, color: u32) {
        if x < self.width && y < self.height {
            self.backbuffer[y * self.width + x] = color;
        }
    }

    #[inline]
    pub fn blend_pixel(&mut self, x: usize, y: usize, color: u32, alpha: u8) {
        if alpha == 0 || x >= self.width || y >= self.height {
            return;
        }
        let idx = y * self.width + x;
        if alpha == 255 {
            self.backbuffer[idx] = color;
            return;
        }
        let dst = self.backbuffer[idx];
        let a = alpha as u32;
        let inv = 255 - a;
        let r = (((color >> 16) & 0xFF) * a + ((dst >> 16) & 0xFF) * inv) / 255;
        let g = (((color >> 8) & 0xFF) * a + ((dst >> 8) & 0xFF) * inv) / 255;
        let b = ((color & 0xFF) * a + (dst & 0xFF) * inv) / 255;
        self.backbuffer[idx] = (r << 16) | (g << 8) | b;
    }

    /// Computes SDF sub-pixel coverage (0..=255) for a rounded rectangle at `(px, py)`
    #[inline]
    fn rounded_rect_sdf_coverage(
        px: usize,
        py: usize,
        x: usize,
        y: usize,
        w: usize,
        h: usize,
        radius: usize,
    ) -> u8 {
        let r = radius.min(w / 2).min(h / 2);
        if r == 0 {
            return 255;
        }
        let lx = px.saturating_sub(x);
        let ly = py.saturating_sub(y);

        let dx = if lx < r {
            r - lx
        } else if lx >= w.saturating_sub(r) {
            lx - (w.saturating_sub(r) - 1)
        } else {
            0
        };

        let dy = if ly < r {
            r - ly
        } else if ly >= h.saturating_sub(r) {
            ly - (h.saturating_sub(r) - 1)
        } else {
            0
        };

        if dx == 0 || dy == 0 {
            return 255;
        }

        // 16x sub-pixel distance in 1/16th pixel units
        let d2_fp8 = ((dx * dx + dy * dy) as u32) << 8;
        let dist_fp4 = isqrt_fp4(d2_fp8);
        let r_fp4 = (r as u32) << 4;

        if dist_fp4 <= r_fp4.saturating_sub(8) {
            255
        } else if dist_fp4 >= r_fp4 + 8 {
            0
        } else {
            // Smooth 1.0px anti-aliased ramp across [r - 0.5, r + 0.5]
            let rem = (r_fp4 + 8) - dist_fp4;
            ((rem * 255) >> 4).min(255) as u8
        }
    }

    /// Fills a rounded rectangle with smooth SDF sub-pixel anti-aliased corners
    pub fn fill_rounded_rect_aa(
        &mut self,
        x: usize,
        y: usize,
        w: usize,
        h: usize,
        radius: usize,
        color: u32,
        alpha: u8,
    ) {
        if w == 0 || h == 0 || alpha == 0 {
            return;
        }
        let x_end = (x + w).min(self.width);
        let y_end = (y + h).min(self.height);
        if x >= x_end || y >= y_end {
            return;
        }
        let r = radius.min(w / 2).min(h / 2);

        for py in y..y_end {
            let ly = py - y;
            let in_corner_y = ly < r || ly >= h.saturating_sub(r);
            if !in_corner_y && alpha == 255 {
                let row_start = py * self.width + x;
                self.backbuffer[row_start..row_start + (x_end - x)].fill(color);
                continue;
            }
            for px in x..x_end {
                let cov = Self::rounded_rect_sdf_coverage(px, py, x, y, w, h, r);
                if cov > 0 {
                    let eff_a = ((cov as u32 * alpha as u32) / 255) as u8;
                    self.blend_pixel(px, py, color, eff_a);
                }
            }
        }
    }

    /// Fills a vertical gradient rounded rectangle with SDF sub-pixel anti-aliased corners
    pub fn fill_rounded_gradient_aa(
        &mut self,
        x: usize,
        y: usize,
        w: usize,
        h: usize,
        radius: usize,
        top_color: u32,
        bottom_color: u32,
        alpha: u8,
    ) {
        if w == 0 || h == 0 || alpha == 0 {
            return;
        }
        let x_end = (x + w).min(self.width);
        let y_end = (y + h).min(self.height);
        if x >= x_end || y >= y_end {
            return;
        }
        let r = radius.min(w / 2).min(h / 2);
        let h_denom = h.saturating_sub(1).max(1);

        for py in y..y_end {
            let ly = py - y;
            let t_256 = ((ly * 256) / h_denom) as u32;
            let row_col = lerp_rgb(top_color, bottom_color, t_256);
            let in_corner_y = ly < r || ly >= h.saturating_sub(r);
            if !in_corner_y && alpha == 255 {
                let row_start = py * self.width + x;
                self.backbuffer[row_start..row_start + (x_end - x)].fill(row_col);
                continue;
            }
            for px in x..x_end {
                let cov = Self::rounded_rect_sdf_coverage(px, py, x, y, w, h, r);
                if cov > 0 {
                    let eff_a = ((cov as u32 * alpha as u32) / 255) as u8;
                    self.blend_pixel(px, py, row_col, eff_a);
                }
            }
        }
    }

    /// Draws a smooth anti-aliased rounded border around `(x, y, w, h)`
    pub fn stroke_rounded_rect_aa(
        &mut self,
        x: usize,
        y: usize,
        w: usize,
        h: usize,
        radius: usize,
        thickness: usize,
        color: u32,
        alpha: u8,
    ) {
        if w == 0 || h == 0 || thickness == 0 || alpha == 0 {
            return;
        }
        let x_end = (x + w).min(self.width);
        let y_end = (y + h).min(self.height);
        let r_out = radius.min(w / 2).min(h / 2);
        let t = thickness.max(1);
        let r_in = r_out.saturating_sub(t);
        let iw = w.saturating_sub(t * 2);
        let ih = h.saturating_sub(t * 2);

        for py in y..y_end {
            let ly = py - y;
            // Fast skip interior middle rows where only left & right vertical borders exist
            if ly >= r_out.max(t) && ly < h.saturating_sub(r_out.max(t)) {
                for px in x..(x + t).min(x_end) {
                    self.blend_pixel(px, py, color, alpha);
                }
                for px in (x + w).saturating_sub(t)..x_end {
                    self.blend_pixel(px, py, color, alpha);
                }
                continue;
            }

            for px in x..x_end {
                let cov_out = Self::rounded_rect_sdf_coverage(px, py, x, y, w, h, r_out);
                if cov_out == 0 {
                    continue;
                }
                let cov_in = if px >= x + t
                    && px < x + t + iw
                    && py >= y + t
                    && py < y + t + ih
                {
                    Self::rounded_rect_sdf_coverage(px, py, x + t, y + t, iw, ih, r_in)
                } else {
                    0
                };
                let ring_cov = cov_out.saturating_sub(cov_in);
                if ring_cov > 0 {
                    let eff_a = ((ring_cov as u32 * alpha as u32) / 255) as u8;
                    self.blend_pixel(px, py, color, eff_a);
                }
            }
        }
    }

    /// Renders a smooth multi-ring Gaussian-like soft drop shadow around `(x, y, w, h)`
    pub fn draw_soft_shadow_aa(
        &mut self,
        x: usize,
        y: usize,
        w: usize,
        h: usize,
        radius: usize,
        spread: usize,
        max_alpha: u8,
    ) {
        let layers = spread.clamp(2, 10);
        for i in (1..=layers).rev() {
            let pad = i * 2;
            let sx = x.saturating_sub(pad / 2);
            let sy = (y + 4).saturating_sub(pad / 3);
            let sw = w + pad;
            let sh = h + pad;
            let falloff = (layers + 1 - i) as u32;
            let layer_a = ((max_alpha as u32 * falloff * falloff)
                / ((layers * layers * 3) as u32).max(1))
            .min(40) as u8;
            if layer_a > 0 {
                self.stroke_rounded_rect_aa(
                    sx,
                    sy,
                    sw,
                    sh,
                    radius + pad / 2,
                    2,
                    0x0F172A,
                    layer_a,
                );
            }
        }
    }

    /// Draws an anti-aliased filled circle using SDF sub-pixel coverage
    pub fn draw_circle_aa(&mut self, cx: i32, cy: i32, radius: i32, color: u32, alpha: u8) {
        if radius <= 0 || alpha == 0 {
            return;
        }
        let r_fp4 = (radius as u32) << 4;
        let x0 = (cx - radius - 1).max(0) as usize;
        let y0 = (cy - radius - 1).max(0) as usize;
        let x1 = ((cx + radius + 2).max(0) as usize).min(self.width);
        let y1 = ((cy + radius + 2).max(0) as usize).min(self.height);

        for py in y0..y1 {
            let dy = (py as i32) - cy;
            for px in x0..x1 {
                let dx = (px as i32) - cx;
                let d2_fp8 = ((dx * dx + dy * dy) as u32) << 8;
                let dist_fp4 = isqrt_fp4(d2_fp8);
                if dist_fp4 <= r_fp4.saturating_sub(8) {
                    self.blend_pixel(px, py, color, alpha);
                } else if dist_fp4 < r_fp4 + 8 {
                    let cov = (((r_fp4 + 8 - dist_fp4) * 255) >> 4).min(255);
                    let eff_a = ((cov * alpha as u32) / 255) as u8;
                    self.blend_pixel(px, py, color, eff_a);
                }
            }
        }
    }

    /// Draws an anti-aliased ring (e.g. for F12 cursor locator sonar or modern icons)
    pub fn draw_ring_aa(
        &mut self,
        cx: i32,
        cy: i32,
        radius: i32,
        thickness: i32,
        color: u32,
        alpha: u8,
    ) {
        if radius <= 0 || thickness <= 0 || alpha == 0 {
            return;
        }
        let r_out_fp4 = (radius as u32) << 4;
        let r_in_fp4 = (radius.saturating_sub(thickness) as u32) << 4;
        let x0 = (cx - radius - 2).max(0) as usize;
        let y0 = (cy - radius - 2).max(0) as usize;
        let x1 = ((cx + radius + 3).max(0) as usize).min(self.width);
        let y1 = ((cy + radius + 3).max(0) as usize).min(self.height);

        for py in y0..y1 {
            let dy = (py as i32) - cy;
            for px in x0..x1 {
                let dx = (px as i32) - cx;
                let d2_fp8 = ((dx * dx + dy * dy) as u32) << 8;
                let dist_fp4 = isqrt_fp4(d2_fp8);
                if dist_fp4 >= r_out_fp4 + 8 || dist_fp4 + 8 <= r_in_fp4 {
                    continue;
                }
                let cov_out = if dist_fp4 <= r_out_fp4.saturating_sub(8) {
                    255
                } else {
                    (((r_out_fp4 + 8 - dist_fp4) * 255) >> 4).min(255)
                };
                let cov_in = if dist_fp4 <= r_in_fp4.saturating_sub(8) {
                    255
                } else if dist_fp4 < r_in_fp4 + 8 {
                    (((r_in_fp4 + 8 - dist_fp4) * 255) >> 4).min(255)
                } else {
                    0
                };
                let ring_cov = cov_out.saturating_sub(cov_in);
                if ring_cov > 0 {
                    let eff_a = ((ring_cov * alpha as u32) / 255) as u8;
                    self.blend_pixel(px, py, color, eff_a);
                }
            }
        }
    }

    /// Standard fill_rect automatically renders with subtle anti-aliased 5px rounded corners
    /// when used for buttons/cards/inputs (`h >= 22 && w < self.width`) so no raw boxy edges appear!
    pub fn fill_rect(&mut self, x: usize, y: usize, w: usize, h: usize, color: u32) {
        if w < self.width && h >= 22 && h <= 64 {
            self.fill_rounded_rect_aa(x, y, w, h, 7, color, 255);
        } else if w < self.width && h > 64 {
            self.fill_rounded_rect_aa(x, y, w, h, 10, color, 255);
        } else {
            let x_end = (x + w).min(self.width);
            let y_end = (y + h).min(self.height);
            if x >= x_end || y >= y_end {
                return;
            }
            let span = x_end - x;
            for row in y..y_end {
                let start = row * self.width + x;
                self.backbuffer[start..start + span].fill(color);
            }
        }
    }

    pub fn fill_rect_alpha(
        &mut self,
        x: usize,
        y: usize,
        w: usize,
        h: usize,
        color: u32,
        alpha: u8,
    ) {
        let radius = if w < self.width && h >= 22 { 8 } else { 0 };
        self.fill_rounded_rect_aa(x, y, w, h, radius, color, alpha);
    }

    pub fn draw_rect_outline(
        &mut self,
        x: usize,
        y: usize,
        w: usize,
        h: usize,
        thickness: usize,
        color: u32,
    ) {
        let radius = if w < self.width && h >= 22 && h <= 64 {
            7
        } else if w < self.width && h > 64 {
            10
        } else {
            0
        };
        if radius > 0 {
            self.stroke_rounded_rect_aa(x, y, w, h, radius, thickness, color, 255);
        } else {
            if w == 0 || h == 0 || thickness == 0 {
                return;
            }
            let t = thickness.min(w / 2).min(h / 2).max(1);
            let x_end = (x + w).min(self.width);
            let y_end = (y + h).min(self.height);
            for py in y..(y + t).min(y_end) {
                for px in x..x_end {
                    self.put_pixel(px, py, color);
                }
            }
            for py in (y + h).saturating_sub(t)..y_end {
                for px in x..x_end {
                    self.put_pixel(px, py, color);
                }
            }
            for py in y..y_end {
                for px in x..(x + t).min(x_end) {
                    self.put_pixel(px, py, color);
                }
                for px in (x + w).saturating_sub(t)..x_end {
                    self.put_pixel(px, py, color);
                }
            }
        }
    }

    pub fn present(&mut self) {
        let w = self.width;
        let h = self.height;
        let stride = self.stride;
        let is_bgr = self.is_bgr_format;
        let vram = self.vram_base_ptr;

        if is_bgr {
            if stride == w {
                unsafe {
                    core::ptr::copy_nonoverlapping(self.backbuffer.as_ptr(), vram, w * h);
                }
            } else {
                for y in 0..h {
                    let src_offset = y * w;
                    let dst_offset = y * stride;
                    unsafe {
                        core::ptr::copy_nonoverlapping(
                            self.backbuffer.as_ptr().add(src_offset),
                            vram.add(dst_offset),
                            w,
                        );
                    }
                }
            }
        } else {
            for y in 0..h {
                let src_row_offset = y * w;
                let dst_row_offset = y * stride;
                let src_row = &self.backbuffer[src_row_offset..src_row_offset + w];

                for (x, &rgb_pixel) in src_row.iter().enumerate() {
                    let r = (rgb_pixel >> 16) & 0xFF;
                    let g = (rgb_pixel >> 8) & 0xFF;
                    let b = rgb_pixel & 0xFF;
                    let hw_pixel = (b << 16) | (g << 8) | r;
                    unsafe {
                        core::ptr::write_volatile(vram.add(dst_row_offset + x), hw_pixel);
                    }
                }
            }
        }
    }

    pub fn present_to_vram(&mut self) {
        self.present();
    }
}
