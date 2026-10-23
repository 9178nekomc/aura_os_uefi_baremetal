//! Bare-Metal Hardware Audio Driver (`src/audio.rs`)
//!
//! 1. **PCIe Audio Controller Discovery (`0xCF8`/`0xCFC`)**:
//!    Scans PCI configuration space for Multimedia Audio Controllers (`Class 0x04`, Subclass `0x01` AC97 / `0x03` Intel HDA).
//! 2. **Intel 8254 PIT Channel 2 + System Control Port B (`0x42`/`0x43`/`0x61`) Hardware Synthesizer**:
//!    Directly drives the hardware square-wave generator and speaker gate on x86_64 bare metal.
//! 3. **System Sound Effects**:
//!    - `play_boot_chime`: Tri-tone ascending C5-E5-G5 chord on OS boot or installation completion
//!    - `play_click`: Instantaneous crisp tactile click pulse for mouse/F12 keyboard-mouse clicks
//!    - `play_agent_step`: Dual-tone telemetry cue when an autonomous MCP tool step completes
//!    - `play_error_beep`: Low-frequency alert buzz when an unauthorized action is blocked

#![allow(dead_code)]

extern crate alloc;
use alloc::format;
use alloc::string::String;
use uefi::table::boot::BootServices;

const PIT_OSC_HZ: u32 = 1_193_182;

#[inline]
unsafe fn outb(port: u16, val: u8) {
    #[cfg(target_arch = "x86_64")]
    core::arch::asm!(
        "out dx, al",
        in("dx") port,
        in("al") val,
        options(nomem, nostack, preserves_flags)
    );
}

#[inline]
unsafe fn inb(port: u16) -> u8 {
    let ret: u8;
    #[cfg(target_arch = "x86_64")]
    core::arch::asm!(
        "in al, dx",
        out("al") ret,
        in("dx") port,
        options(nomem, nostack, preserves_flags)
    );
    #[cfg(not(target_arch = "x86_64"))]
    {
        ret = 0;
    }
    ret
}

#[inline]
unsafe fn outl(port: u16, val: u32) {
    #[cfg(target_arch = "x86_64")]
    core::arch::asm!(
        "out dx, eax",
        in("dx") port,
        in("eax") val,
        options(nomem, nostack, preserves_flags)
    );
}

#[inline]
unsafe fn inl(port: u16) -> u32 {
    let ret: u32;
    #[cfg(target_arch = "x86_64")]
    core::arch::asm!(
        "in eax, dx",
        out("eax") ret,
        in("dx") port,
        options(nomem, nostack, preserves_flags)
    );
    #[cfg(not(target_arch = "x86_64"))]
    {
        ret = 0xFFFF_FFFF;
    }
    ret
}

fn pci_read_u32(bus: u8, slot: u8, func: u8, offset: u8) -> u32 {
    let addr: u32 = 0x8000_0000
        | ((bus as u32) << 16)
        | ((slot as u32) << 11)
        | ((func as u32) << 8)
        | ((offset as u32) & 0xFC);
    unsafe {
        outl(0xCF8, addr);
        inl(0xCFC)
    }
}

/// Probes PCI Bus 0 for any hardware AC97 / Intel HDA audio controller alongside 8254 PIT
pub fn detect_audio_hardware_name() -> String {
    for slot in 0..32u8 {
        let vendor_dev = pci_read_u32(0, slot, 0, 0x00);
        if vendor_dev == 0xFFFF_FFFF || vendor_dev == 0 {
            continue;
        }
        let class_rev = pci_read_u32(0, slot, 0, 0x08);
        let base_class = ((class_rev >> 24) & 0xFF) as u8;
        let sub_class = ((class_rev >> 16) & 0xFF) as u8;
        if base_class == 0x04 {
            let vendor = (vendor_dev & 0xFFFF) as u16;
            let device = ((vendor_dev >> 16) & 0xFFFF) as u16;
            let kind = match sub_class {
                0x01 => "AC97 Multimedia Audio",
                0x03 => "Intel HD Audio (HDA)",
                _ => "PCIe Audio Synth",
            };
            return format!(
                "{} [{:04X}:{:04X}] + 8254 PIT (0x42/0x61)",
                kind, vendor, device
            );
        }
    }
    String::from("Intel 8254 PIT Ch2 + PC Speaker Synthesizer (Port 0x42/0x61)")
}

pub struct SoundDriver {
    pub enabled: bool,
    pub hardware_desc: String,
    pub last_freq_hz: u32,
    pub cues_played: u64,
}

pub type SoundCardDriver = SoundDriver;

impl SoundDriver {
    pub fn new() -> Self {
        Self {
            enabled: true,
            hardware_desc: detect_audio_hardware_name(),
            last_freq_hz: 0,
            cues_played: 0,
        }
    }

    pub fn init() -> Self {
        Self::new()
    }

    /// Programs 8254 PIT Channel 2 (`0x43` / `0x42`) and enables Speaker Gate (`0x61` bits 0..1)
    /// for `duration_ms` milliseconds using UEFI `BootServices::stall`.
    pub fn play_tone_blocking(&mut self, bt: &BootServices, freq_hz: u32, duration_ms: u32) {
        if !self.enabled || freq_hz < 20 || freq_hz > 20_000 || duration_ms == 0 {
            return;
        }
        let divisor = (PIT_OSC_HZ / freq_hz).clamp(1, 65535) as u16;
        self.last_freq_hz = freq_hz;
        self.cues_played = self.cues_played.wrapping_add(1);

        unsafe {
            outb(0x43, 0xB6);
            outb(0x42, (divisor & 0xFF) as u8);
            outb(0x42, ((divisor >> 8) & 0xFF) as u8);

            let gate = inb(0x61);
            if (gate & 0x03) != 0x03 {
                outb(0x61, gate | 0x03);
            }
        }

        let _ = bt.stall((duration_ms as usize).saturating_mul(1000));

        unsafe {
            let gate = inb(0x61);
            outb(0x61, gate & 0xFC);
        }
    }

    pub fn play_tone(&mut self, bt: &BootServices, freq_hz: u32, duration_ms: u32) {
        self.play_tone_blocking(bt, freq_hz, duration_ms);
    }

    /// Instantaneous non-blocking micro-pulse click so cursor interaction has zero frame stall
    pub fn play_click(&mut self) {
        if !self.enabled {
            return;
        }
        let divisor = (PIT_OSC_HZ / 1040).clamp(1, 65535) as u16;
        self.last_freq_hz = 1040;
        self.cues_played = self.cues_played.wrapping_add(1);

        unsafe {
            outb(0x43, 0xB6);
            outb(0x42, (divisor & 0xFF) as u8);
            outb(0x42, ((divisor >> 8) & 0xFF) as u8);
            let gate = inb(0x61);
            outb(0x61, gate | 0x03);
            // Short I/O delay loop (~1.2ms on QEMU/KVM)
            for _ in 0..3200 {
                core::hint::spin_loop();
            }
            outb(0x61, gate & 0xFC);
        }
    }

    /// Plays C5 -> E5 -> G5 ascending boot / installation completion chord
    pub fn play_boot_chime(&mut self, bt: &BootServices) {
        if !self.enabled {
            return;
        }
        self.play_tone_blocking(bt, 523, 30);
        self.play_tone_blocking(bt, 659, 30);
        self.play_tone_blocking(bt, 784, 45);
    }

    /// Plays D5 -> A5 dual-tone confirmation when an autonomous MCP tool step executes
    pub fn play_agent_step(&mut self, bt: &BootServices) {
        if !self.enabled {
            return;
        }
        self.play_tone_blocking(bt, 587, 18);
        self.play_tone_blocking(bt, 880, 24);
    }

    /// Plays a low-frequency 220Hz security/permission rejection buzz
    pub fn play_error_beep(&mut self, bt: &BootServices) {
        if !self.enabled {
            return;
        }
        self.play_tone_blocking(bt, 220, 40);
    }
}
