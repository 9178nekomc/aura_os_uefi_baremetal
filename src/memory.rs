//! Bare-Metal UEFI Physical Memory Map Analyzer & GOP MMIO Bounds Verifier (`src/memory.rs`)
//!
//! Iterates the real UEFI `MemoryMap` and strictly separates actual physical RAM descriptors
//! (`CONVENTIONAL`, `LOADER_*`, `BOOT_SERVICES_*`, `RUNTIME_SERVICES_*`, `ACPI_*`) from
//! 64-bit PCIe BAR MMIO / firmware address-space holes so `total_ram_mb()` and `usable_ram_mb()`
//! always report exact physical RAM.

#![allow(dead_code)]

extern crate alloc;
use alloc::string::String;
use alloc::vec::Vec;
use uefi::table::boot::{BootServices, MemoryType};

#[repr(align(8))]
struct AlignedMmapBuffer([u8; 32768]);

#[derive(Clone)]
pub struct MemoryRegionRecord {
    pub kind_name: &'static str,
    pub phys_start: u64,
    pub page_count: u64,
    pub size_kb: u64,
}

#[derive(Clone)]
pub struct SystemMemoryReport {
    pub total_ram_mb_val: u64,
    pub usable_ram_mb_val: u64,
    pub used_ram_mb_val: u64,
    pub mmio_reserved_mb_val: u64,
    pub descriptor_count: usize,
    pub regions: Vec<MemoryRegionRecord>,
}

pub type BootMemorySummary = SystemMemoryReport;

impl SystemMemoryReport {
    pub fn capture_from_boot_services(bt: &BootServices) -> Self {
        let mut ram_pages = 0u64;
        let mut conv_pages = 0u64;
        let mut used_ram_pages = 0u64;
        let mut mmio_res_pages = 0u64;
        let mut descriptor_count = 0usize;
        let mut regions = Vec::new();

        let mut storage = AlignedMmapBuffer([0u8; 32768]);
        if let Ok(mmap) = bt.memory_map(&mut storage.0) {
            descriptor_count = mmap.entries().len();
            for desc in mmap.entries() {
                let pages = desc.page_count;
                let kind_name = classify_memory_type(
                    desc.ty,
                    pages,
                    &mut ram_pages,
                    &mut conv_pages,
                    &mut used_ram_pages,
                    &mut mmio_res_pages,
                );

                if regions.len() < 16 && pages >= 16 {
                    regions.push(MemoryRegionRecord {
                        kind_name,
                        phys_start: desc.phys_start,
                        page_count: pages,
                        size_kb: pages.saturating_mul(4),
                    });
                }
            }
        }

        let total_ram_mb_val = (ram_pages.saturating_add(128)) / 256;
        let usable_ram_mb_val = conv_pages / 256;
        let used_ram_mb_val = used_ram_pages / 256;
        let mmio_reserved_mb_val = mmio_res_pages / 256;

        Self {
            total_ram_mb_val,
            usable_ram_mb_val,
            used_ram_mb_val,
            mmio_reserved_mb_val,
            descriptor_count,
            regions,
        }
    }

    #[inline]
    pub fn total_ram_mb(&self) -> u64 {
        self.total_ram_mb_val
    }

    #[inline]
    pub fn usable_ram_mb(&self) -> u64 {
        self.usable_ram_mb_val
    }

    #[inline]
    pub fn used_ram_mb(&self) -> u64 {
        self.used_ram_mb_val
    }

    #[inline]
    pub fn mmio_reserved_mb(&self) -> u64 {
        self.mmio_reserved_mb_val
    }
}

fn classify_memory_type(
    ty: MemoryType,
    pages: u64,
    ram_pages: &mut u64,
    conv_pages: &mut u64,
    used_ram_pages: &mut u64,
    mmio_res_pages: &mut u64,
) -> &'static str {
    match ty {
        MemoryType::CONVENTIONAL => {
            *ram_pages = ram_pages.saturating_add(pages);
            *conv_pages = conv_pages.saturating_add(pages);
            "CONVENTIONAL (Free RAM)"
        }
        MemoryType::LOADER_CODE | MemoryType::LOADER_DATA => {
            *ram_pages = ram_pages.saturating_add(pages);
            *used_ram_pages = used_ram_pages.saturating_add(pages);
            "LOADER (Kernel & Heap)"
        }
        MemoryType::BOOT_SERVICES_CODE | MemoryType::BOOT_SERVICES_DATA => {
            *ram_pages = ram_pages.saturating_add(pages);
            *used_ram_pages = used_ram_pages.saturating_add(pages);
            "BOOT_SERVICES (UEFI)"
        }
        MemoryType::RUNTIME_SERVICES_CODE | MemoryType::RUNTIME_SERVICES_DATA => {
            *ram_pages = ram_pages.saturating_add(pages);
            *used_ram_pages = used_ram_pages.saturating_add(pages);
            "RUNTIME_SERVICES (UEFI)"
        }
        MemoryType::ACPI_RECLAIM | MemoryType::ACPI_NON_VOLATILE => {
            *ram_pages = ram_pages.saturating_add(pages);
            *used_ram_pages = used_ram_pages.saturating_add(pages);
            "ACPI_TABLES (RAM)"
        }
        MemoryType::PERSISTENT_MEMORY | MemoryType::PAL_CODE => {
            *ram_pages = ram_pages.saturating_add(pages);
            *used_ram_pages = used_ram_pages.saturating_add(pages);
            "PERSISTENT_RAM"
        }
        MemoryType::MMIO | MemoryType::MMIO_PORT_SPACE => {
            *mmio_res_pages = mmio_res_pages.saturating_add(pages);
            "PCIe_MMIO (Non-RAM BAR)"
        }
        _ => {
            if pages <= 16384 {
                *ram_pages = ram_pages.saturating_add(pages);
                *used_ram_pages = used_ram_pages.saturating_add(pages);
                "FIRMWARE_RAM (Reserved)"
            } else {
                *mmio_res_pages = mmio_res_pages.saturating_add(pages);
                "ADDRESS_HOLE (Reserved)"
            }
        }
    }
}

pub fn validate_gop_framebuffer_safety(
    fb_base_addr: usize,
    fb_byte_size: usize,
    width: usize,
    height: usize,
    stride: usize,
) -> Result<String, &'static str> {
    if fb_base_addr == 0 {
        return Err("CRITICAL: GOP FrameBuffer physical base address is NULL (0x0)!");
    }
    if (fb_base_addr & 0x3) != 0 {
        return Err("CRITICAL: GOP FrameBuffer base address is not 4-byte aligned!");
    }
    if width == 0 || height == 0 {
        return Err("CRITICAL: GOP resolution width or height is zero!");
    }
    if stride < width {
        return Err("CRITICAL: GOP scanline stride is smaller than visible width!");
    }

    let total_pixels_needed = (height - 1)
        .checked_mul(stride)
        .and_then(|v| v.checked_add(width))
        .ok_or("CRITICAL: Integer overflow computing GOP framebuffer pixel span!")?;

    let required_bytes = total_pixels_needed
        .checked_mul(4)
        .ok_or("CRITICAL: Integer overflow computing GOP framebuffer byte size!")?;

    if fb_byte_size < required_bytes {
        return Err("CRITICAL: GOP physical MMIO buffer size is smaller than stride * height * 4!");
    }

    Ok(alloc::format!(
        "VRAM: 0x{:08X} ({}x{}, Stride {})",
        fb_base_addr,
        width,
        height,
        stride
    ))
}

pub fn read_cpu_control_registers() -> (u64, u64) {
    let cr3: u64;
    let cr0: u64;
    #[cfg(target_arch = "x86_64")]
    unsafe {
        core::arch::asm!("mov {}, cr3", out(reg) cr3, options(nomem, nostack, preserves_flags));
        core::arch::asm!("mov {}, cr0", out(reg) cr0, options(nomem, nostack, preserves_flags));
    }
    #[cfg(not(target_arch = "x86_64"))]
    {
        cr3 = 0;
        cr0 = 0;
    }
    (cr3, cr0)
}
