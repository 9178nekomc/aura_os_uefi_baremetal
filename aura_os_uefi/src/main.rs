#![no_std]
#![no_main]

extern crate alloc;

pub mod aa_font;
mod audio;
mod cjk_font;
mod framebuffer;
mod image_codec;
mod math_nostd;
mod mcp_agent;
mod memory;
mod net;
mod security;
mod storage;

use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use core::fmt::Write;
use uefi::prelude::*;
use uefi::proto::console::pointer::Pointer;
use uefi::proto::console::text::{Key, ScanCode};
use uefi::table::boot::{OpenProtocolAttributes, OpenProtocolParams, SearchType};
use uefi::Identify;

use crate::audio::SoundDriver;
use crate::framebuffer::GopFrameBuffer;
use crate::image_codec::{
    decode_image_bytes, draw_antialias_cursor, draw_graphical_app_icon, generate_builtin_wallpaper,
    AppIconKind, DecodedImage,
};
use crate::mcp_agent::{
    check_mcp_permission, AgentAutonomousEngine, McpToolId, AGENT_MCP_SYSTEM_PROMPT, MCP_TOOLS,
};
use crate::memory::SystemMemoryReport;
use crate::net::NetworkManager;
use crate::security::{
    AiPermissionLevel, CryptoEngine, SecurityAuditChain, UserAuthManager, UserRole,
};
use crate::storage::{DiskStorage, InstalledSystemConfig};

#[panic_handler]
fn panic(_info: &core::panic::PanicInfo) -> ! {
    loop {
        unsafe {
            core::arch::asm!("hlt", options(nomem, nostack, preserves_flags));
        }
    }
}

// ============================================================================
// 1. UI LANGUAGE & WHITE-BLUE LIGHTWEIGHT THEME PALETTE
// ============================================================================

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum UiLang {
    En,
    Zh,
}

impl UiLang {
    pub fn pick<'a>(self, en: &'a str, zh: &'a str) -> &'a str {
        match self {
            UiLang::En => en,
            UiLang::Zh => zh,
        }
    }
}

#[derive(Clone, Copy)]
pub struct ThemePalette {
    pub name_en: &'static str,
    pub name_zh: &'static str,
    pub win_bg: u32,
    pub win_panel: u32,
    pub win_border: u32,
    pub title_active: u32,
    pub title_inactive: u32,
    pub title_text: u32,
    pub accent: u32,
    pub accent_soft: u32,
    pub text_primary: u32,
    pub text_secondary: u32,
    pub taskbar_bg: u32,
    pub taskbar_text: u32,
}

pub const THEMES: [ThemePalette; 4] = [
    // Theme 0: Airy White-Blue (Default - 白蓝轻量化配色)
    ThemePalette {
        name_en: "Airy White-Blue",
        name_zh: "白蓝轻量化",
        win_bg: 0xF8FAFC,
        win_panel: 0xFFFFFF,
        win_border: 0xCBD5E1,
        title_active: 0x2563EB,
        title_inactive: 0x94A3B8,
        title_text: 0xFFFFFF,
        accent: 0x2563EB,
        accent_soft: 0xDBEAFE,
        text_primary: 0x0F172A,
        text_secondary: 0x475569,
        taskbar_bg: 0xF1F5F9,
        taskbar_text: 0x0F172A,
    },
    // Theme 1: Sky Porcelain (晴空瓷白)
    ThemePalette {
        name_en: "Sky Porcelain",
        name_zh: "晴空瓷白",
        win_bg: 0xF0F9FF,
        win_panel: 0xFFFFFF,
        win_border: 0xBAE6FD,
        title_active: 0x0284C7,
        title_inactive: 0x7DD3FC,
        title_text: 0xFFFFFF,
        accent: 0x0284C7,
        accent_soft: 0xE0F2FE,
        text_primary: 0x0C4A6E,
        text_secondary: 0x0369A1,
        taskbar_bg: 0xE0F2FE,
        taskbar_text: 0x0C4A6E,
    },
    // Theme 2: Mint Daylight (薄荷晨光)
    ThemePalette {
        name_en: "Mint Daylight",
        name_zh: "薄荷晨光",
        win_bg: 0xF0FDF4,
        win_panel: 0xFFFFFF,
        win_border: 0xBBF7D0,
        title_active: 0x0D9488,
        title_inactive: 0x94A3B8,
        title_text: 0xFFFFFF,
        accent: 0x0D9488,
        accent_soft: 0xCCFBF1,
        text_primary: 0x064E3B,
        text_secondary: 0x0F766E,
        taskbar_bg: 0xECFDF5,
        taskbar_text: 0x064E3B,
    },
    // Theme 3: Slate Frost (极简霜灰)
    ThemePalette {
        name_en: "Slate Frost",
        name_zh: "极简霜灰",
        win_bg: 0xF1F5F9,
        win_panel: 0xFFFFFF,
        win_border: 0x94A3B8,
        title_active: 0x334155,
        title_inactive: 0x94A3B8,
        title_text: 0xFFFFFF,
        accent: 0x3B82F6,
        accent_soft: 0xE2E8F0,
        text_primary: 0x1E293B,
        text_secondary: 0x475569,
        taskbar_bg: 0xE2E8F0,
        taskbar_text: 0x0F172A,
    },
];

// ============================================================================
// 2. FONT RENDERER (ASCII 8x16 + CJK 16x16 BITMAP)
// ============================================================================

pub struct FontRenderer;

impl FontRenderer {
    /// Renders text using 16-level (4-bit) grayscale FreeType/OpenType anti-aliased glyphs
    /// for both ASCII (`10x18`) and CJK (`16x16`), with automatic sub-pixel edge smoothing
    /// for all 6,779 GB2312 Chinese characters.
    pub fn draw_text(fb: &mut GopFrameBuffer, mut x: usize, y: usize, text: &str, color: u32) {
        for ch in text.chars() {
            if (ch as u32) > 0x7F {
                // 1. Try 4-bit (16-level grayscale) anti-aliased CJK glyph first
                if let Some(aa_glyph) = aa_font::lookup_aa_cjk(ch) {
                    for row in 0..16usize {
                        for pair in 0..8usize {
                            let packed = aa_glyph[row * 8 + pair];
                            let a0 = (packed >> 4) * 17;
                            let a1 = (packed & 0x0F) * 17;
                            if a0 > 0 {
                                fb.blend_pixel(x + pair * 2, y + row, color, a0);
                            }
                            if a1 > 0 {
                                fb.blend_pixel(x + pair * 2 + 1, y + row, color, a1);
                            }
                        }
                    }
                    x += 17;
                    continue;
                }
                // 2. Fallback to full 6,779-char GB2312 table with 4-neighbor sub-pixel AA halo
                if let Some(bitmap) = cjk_font::lookup_cjk(ch) {
                    for row_idx in 0..16usize {
                        let row_bits = bitmap[row_idx];
                        let up_bits = if row_idx > 0 { bitmap[row_idx - 1] } else { 0 };
                        let dn_bits = if row_idx + 1 < 16 { bitmap[row_idx + 1] } else { 0 };
                        for col_idx in 0..16usize {
                            let shift = 15 - col_idx;
                            let on = ((row_bits >> shift) & 1) == 1;
                            if on {
                                fb.blend_pixel(x + col_idx, y + row_idx, color, 255);
                            } else {
                                let l = if col_idx > 0 { (row_bits >> (shift + 1)) & 1 } else { 0 };
                                let r = if col_idx + 1 < 16 { (row_bits >> (shift - 1)) & 1 } else { 0 };
                                let u = (up_bits >> shift) & 1;
                                let d = (dn_bits >> shift) & 1;
                                let neighbors = l + r + u + d;
                                if neighbors >= 2 {
                                    fb.blend_pixel(x + col_idx, y + row_idx, color, 92);
                                } else if neighbors == 1 {
                                    fb.blend_pixel(x + col_idx, y + row_idx, color, 38);
                                }
                            }
                        }
                    }
                    x += 17;
                    continue;
                }
            }

            // 3. Render 4-bit (16-level grayscale) anti-aliased ASCII glyph (10x18)
            if let Some(aa_ascii) = aa_font::lookup_aa_ascii(ch) {
                for row in 0..aa_font::AA_ASCII_H {
                    for pair in 0..5usize {
                        let packed = aa_ascii[row * 5 + pair];
                        let a0 = (packed >> 4) * 17;
                        let a1 = (packed & 0x0F) * 17;
                        if a0 > 0 {
                            fb.blend_pixel(x + pair * 2, y + row.saturating_sub(1), color, a0);
                        }
                        if a1 > 0 {
                            fb.blend_pixel(x + pair * 2 + 1, y + row.saturating_sub(1), color, a1);
                        }
                    }
                }
            }
            x += 9;
        }
    }

    pub fn measure_width(text: &str) -> usize {
        let mut w = 0usize;
        for ch in text.chars() {
            if (ch as u32) > 0x7F {
                w += 17;
            } else {
                w += 9;
            }
        }
        w
    }

    pub fn draw_text_centered(
        fb: &mut GopFrameBuffer,
        cx: usize,
        y: usize,
        text: &str,
        color: u32,
    ) {
        let w = Self::measure_width(text);
        let start_x = cx.saturating_sub(w / 2);
        Self::draw_text(fb, start_x, y, text, color);
    }
}

// ============================================================================
// 3. HARDWARE & F12 KEYBOARD-EMULATED CURSOR CONTROLLER
// ============================================================================

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum HoldState {
    Released,
    LeftHeld,
    RightHeld,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum CursorState {
    Arrow,
    HandHover,
    GrabDrag,
}

pub struct CursorController {
    pub x: i32,
    pub y: i32,
    pub fx_fp8: i32,
    pub fy_fp8: i32,
    pub tx_fp8: i32,
    pub ty_fp8: i32,
    pub left_down: bool,
    pub right_down: bool,
    pub prev_left_down: bool,
    pub left_clicked: bool,
    pub right_clicked: bool,
    pub double_clicked: bool,
    pub last_click_tick: u64,
    pub kbd_mouse_enabled: bool,
    pub f12_press_count: u32,
    pub sonar_ticks: u32,
    pub hold_state: HoldState,
    pub state: CursorState,
    pub step_px: i32,
}

impl CursorController {
    pub fn new(screen_w: usize, screen_h: usize) -> Self {
        let cx = (screen_w / 2) as i32;
        let cy = (screen_h / 2) as i32;
        Self {
            x: cx,
            y: cy,
            fx_fp8: cx << 8,
            fy_fp8: cy << 8,
            tx_fp8: cx << 8,
            ty_fp8: cy << 8,
            left_down: false,
            right_down: false,
            prev_left_down: false,
            left_clicked: false,
            right_clicked: false,
            double_clicked: false,
            last_click_tick: 0,
            kbd_mouse_enabled: true,
            f12_press_count: 0,
            sonar_ticks: 40,
            hold_state: HoldState::Released,
            state: CursorState::Arrow,
            step_px: 18,
        }
    }

    pub fn begin_frame(&mut self) {
        self.prev_left_down = self.left_down;
        self.left_clicked = false;
        self.right_clicked = false;
        self.double_clicked = false;
        self.left_down = matches!(self.hold_state, HoldState::LeftHeld);
        self.right_down = matches!(self.hold_state, HoldState::RightHeld);

        // Smooth 120Hz sub-pixel exponential glide toward (tx_fp8, ty_fp8)
        let dx_fp = self.tx_fp8 - self.fx_fp8;
        let dy_fp = self.ty_fp8 - self.fy_fp8;
        if dx_fp.abs() <= 64 {
            self.fx_fp8 = self.tx_fp8;
        } else {
            self.fx_fp8 += (dx_fp * 96) / 256;
        }
        if dy_fp.abs() <= 64 {
            self.fy_fp8 = self.ty_fp8;
        } else {
            self.fy_fp8 += (dy_fp * 96) / 256;
        }
        self.x = self.fx_fp8 >> 8;
        self.y = self.fy_fp8 >> 8;

        if self.sonar_ticks > 0 {
            self.sonar_ticks -= 1;
        }
    }

    /// Toggles F12 Keyboard Mouse mode and guarantees cursor visibility with a sonar locator ring
    pub fn trigger_f12(&mut self, max_w: usize, max_h: usize) {
        if self.f12_press_count == 0 && self.kbd_mouse_enabled {
            // First time user presses F12 expecting to turn ON F12 mode: keep it ON and highlight!
            self.kbd_mouse_enabled = true;
        } else {
            self.kbd_mouse_enabled = !self.kbd_mouse_enabled;
        }
        self.f12_press_count = self.f12_press_count.wrapping_add(1);
        self.sonar_ticks = 48;
        self.clamp(max_w, max_h);
    }

    pub fn move_by(&mut self, dx: i32, dy: i32, max_w: usize, max_h: usize) {
        self.tx_fp8 += dx << 8;
        self.ty_fp8 += dy << 8;
        self.clamp(max_w, max_h);
    }

    pub fn clamp(&mut self, max_w: usize, max_h: usize) {
        // Keep the entire 28x28 cursor safely inside visible screen bounds so it never disappears!
        let min_x = 8i32;
        let min_y = 8i32;
        let mw = (max_w as i32).saturating_sub(28).max(min_x);
        let mh = (max_h as i32).saturating_sub(30).max(min_y);

        let min_x_fp = min_x << 8;
        let min_y_fp = min_y << 8;
        let mw_fp = mw << 8;
        let mh_fp = mh << 8;

        if self.tx_fp8 < min_x_fp {
            self.tx_fp8 = min_x_fp;
        }
        if self.ty_fp8 < min_y_fp {
            self.ty_fp8 = min_y_fp;
        }
        if self.tx_fp8 > mw_fp {
            self.tx_fp8 = mw_fp;
        }
        if self.ty_fp8 > mh_fp {
            self.ty_fp8 = mh_fp;
        }

        if self.fx_fp8 < min_x_fp {
            self.fx_fp8 = min_x_fp;
        }
        if self.fy_fp8 < min_y_fp {
            self.fy_fp8 = min_y_fp;
        }
        if self.fx_fp8 > mw_fp {
            self.fx_fp8 = mw_fp;
        }
        if self.fy_fp8 > mh_fp {
            self.fy_fp8 = mh_fp;
        }

        self.x = (self.fx_fp8 >> 8).clamp(min_x, mw);
        self.y = (self.fy_fp8 >> 8).clamp(min_y, mh);
    }

    /// Handles the user's exact numpad/number mouse emulation spec when `kbd_mouse_enabled`:
    /// - '0' -> Down
    /// - '2' -> Up
    /// - '1' -> Left
    /// - '3' -> Right
    /// - '7' -> Left Click (2 presses within 160 ticks = Double Click)
    /// - '9' -> Right Click
    /// - '4' -> Cycle Hold/Drag Mode: 1st press = LeftHeld (Drag), 2nd press = RightHeld, 3rd press = Released
    pub fn handle_kbd_mouse_char(
        &mut self,
        ch: char,
        tick: u64,
        max_w: usize,
        max_h: usize,
        sound: &mut SoundDriver,
    ) -> bool {
        if !self.kbd_mouse_enabled {
            return false;
        }
        match ch {
            '0' => {
                self.move_by(0, self.step_px, max_w, max_h);
                true
            }
            '2' => {
                self.move_by(0, -self.step_px, max_w, max_h);
                true
            }
            '1' => {
                self.move_by(-self.step_px, 0, max_w, max_h);
                true
            }
            '3' => {
                self.move_by(self.step_px, 0, max_w, max_h);
                true
            }
            '7' => {
                self.left_clicked = true;
                self.left_down = true;
                if tick.saturating_sub(self.last_click_tick) <= 160 {
                    self.double_clicked = true;
                }
                self.last_click_tick = tick;
                self.sonar_ticks = 18;
                sound.play_click();
                true
            }
            '9' => {
                self.right_clicked = true;
                self.right_down = true;
                self.sonar_ticks = 18;
                sound.play_click();
                true
            }
            '4' => {
                self.hold_state = match self.hold_state {
                    HoldState::Released => {
                        self.left_clicked = true;
                        self.left_down = true;
                        HoldState::LeftHeld
                    }
                    HoldState::LeftHeld => {
                        self.left_down = false;
                        self.right_down = true;
                        HoldState::RightHeld
                    }
                    HoldState::RightHeld => {
                        self.left_down = false;
                        self.right_down = false;
                        HoldState::Released
                    }
                };
                self.sonar_ticks = 28;
                sound.play_click();
                true
            }
            _ => false,
        }
    }

    pub fn draw(&self, fb: &mut GopFrameBuffer) {
        let cx = self.x.max(0) as usize;
        let cy = self.y.max(0) as usize;

        // Draw smooth SDF anti-aliased locator sonar ring when F12 is toggled or in Hold/Drag state
        if self.sonar_ticks > 0 {
            let r = 14 + ((48u32.saturating_sub(self.sonar_ticks)) / 2) as i32;
            let alpha = ((self.sonar_ticks * 4).min(190)) as u8;
            fb.draw_ring_aa(self.x + 6, self.y + 6, r, 3, 0x2563EB, alpha);
        } else if matches!(self.hold_state, HoldState::LeftHeld) {
            fb.draw_ring_aa(self.x + 6, self.y + 6, 18, 3, 0x16A34A, 180);
        } else if matches!(self.hold_state, HoldState::RightHeld) {
            fb.draw_ring_aa(self.x + 6, self.y + 6, 18, 3, 0xD97706, 180);
        }

        let variant = match self.hold_state {
            HoldState::LeftHeld => 2,
            HoldState::RightHeld => 3,
            HoldState::Released => match self.state {
                CursorState::Arrow => 0,
                CursorState::HandHover => 1,
                CursorState::GrabDrag => 2,
            },
        };

        let sw = fb.width;
        let sh = fb.height;
        draw_antialias_cursor(fb.backbuffer_mut(), sw, sh, cx, cy, variant);
    }
}

// ============================================================================
// 4. FLOATING WINDOW MANAGER (WITH MINIMIZE, MAXIMIZE, RESTORE, CLOSE, DRAG)
// ============================================================================

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum WindowId {
    Installer = 0,
    FileManager = 1,
    AiAgent = 2,
    Terminal = 3,
    Settings = 4,
}

#[derive(Clone, Copy)]
pub struct DesktopWindow {
    pub id: WindowId,
    pub title_en: &'static str,
    pub title_zh: &'static str,
    pub x: usize,
    pub y: usize,
    pub w: usize,
    pub h: usize,
    // Saved geometry before maximize so clicking Restore [❐] returns to exact original window size!
    pub saved_x: usize,
    pub saved_y: usize,
    pub saved_w: usize,
    pub saved_h: usize,
    pub is_open: bool,
    pub is_minimized: bool,
    pub is_maximized: bool,
    pub z_order: usize,
}

impl DesktopWindow {
    pub fn toggle_maximize(&mut self, screen_w: usize, screen_h: usize) {
        let taskbar_h = 44usize;
        if !self.is_maximized {
            // Save original geometry before maximizing
            self.saved_x = self.x;
            self.saved_y = self.y;
            self.saved_w = self.w;
            self.saved_h = self.h;
            self.x = 0;
            self.y = 0;
            self.w = screen_w;
            self.h = screen_h.saturating_sub(taskbar_h);
            self.is_maximized = true;
        } else {
            // Restore original floating window size and position!
            self.x = self.saved_x;
            self.y = self.saved_y;
            self.w = self.saved_w;
            self.h = self.saved_h;
            self.is_maximized = false;
        }
    }
}

// ============================================================================
// 5. UBUNTU-STYLE 6-STEP INSTALLER STATE
// ============================================================================

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum InstallerStep {
    WelcomeAsk = 0,
    LanguageSelect = 1,
    TimeNtpConfig = 2,
    AiConfig = 3,
    AdminRegister = 4,
    NetworkCheck = 5,
    ConfirmAndWrite = 6,
    Completed = 7,
}

pub struct InstallerWizard {
    pub step: InstallerStep,
    pub active_field: usize,
    pub lang: UiLang,
    pub ntp_enabled: bool,
    pub ntp_server: String,
    pub tz_offset: i32,
    pub ai_protocol: String,
    pub ai_model: String,
    pub ai_endpoint: String,
    pub ai_api_key: String,
    pub ai_perm: AiPermissionLevel,
    pub admin_user: String,
    pub admin_pass: String,
    pub std_user: String,
    pub std_pass: String,
    pub status_msg: String,
}

impl InstallerWizard {
    pub fn new() -> Self {
        Self {
            step: InstallerStep::WelcomeAsk,
            active_field: 0,
            lang: UiLang::En,
            ntp_enabled: true,
            ntp_server: String::from("pool.ntp.org"),
            tz_offset: 8,
            ai_protocol: String::from("HTTPS"),
            ai_model: String::from("deepseek-r1:8b"),
            ai_endpoint: String::from("https://api.openai.com/v1/chat/completions"),
            ai_api_key: String::from("sk-aura-live-key"),
            ai_perm: AiPermissionLevel::L2DesktopControl,
            admin_user: String::from("admin"),
            admin_pass: String::from("1234"),
            std_user: String::from("user"),
            std_pass: String::from("0000"),
            status_msg: String::new(),
        }
    }
}

// ============================================================================
// 6. FULL OPERATING SYSTEM STATE
// ============================================================================

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum FocusedInput {
    None,
    InstallerField,
    TerminalCmd,
    AiPrompt,
    AiEndpoint,
    AiModel,
    AiKey,
    FileMgrNewName,
    FileMgrNewContent,
    SettingsNtpServer,
}

pub struct AuraOsDesktop {
    pub is_installed: bool,
    pub lang: UiLang,
    pub theme_idx: usize,
    pub wallpaper_idx: usize,
    pub wallpaper_scaled: DecodedImage,
    pub windows: [DesktopWindow; 5],
    pub active_win: WindowId,
    pub dragging_win: Option<(WindowId, i32, i32)>,
    pub start_menu_open: bool,
    pub clipboard: String,
    pub focused_input: FocusedInput,

    // Subsystems
    pub sound: SoundDriver,
    pub net: NetworkManager,
    pub auth: UserAuthManager,
    pub ai_perm: AiPermissionLevel,
    pub agent: AgentAutonomousEngine,
    pub installer: InstallerWizard,
    pub audit_chain: SecurityAuditChain,

    // Time & NTP
    pub ntp_enabled: bool,
    pub ntp_server: String,
    pub tz_offset: i32,

    // File Manager state
    pub current_dir: String,
    pub dir_entries: Vec<(String, u64, bool)>,
    pub selected_file_idx: usize,
    pub file_preview_name: String,
    pub file_preview_content: String,
    pub new_file_name: String,
    pub new_file_content: String,

    // Terminal state
    pub term_input: String,
    pub term_logs: Vec<String>,

    // AI Agent state
    pub ai_prompt_input: String,
    pub show_mcp_prompt_modal: bool,
}

impl AuraOsDesktop {
    pub fn new(
        bt: &BootServices,
        screen_w: usize,
        screen_h: usize,
        installed_opt: Option<InstalledSystemConfig>,
    ) -> Self {
        let mut sound = SoundDriver::new();
        let net = NetworkManager::probe_hardware(bt);
        let mut auth = UserAuthManager::new_default();
        let mut audit_chain = SecurityAuditChain::new();
        audit_chain.AppendRecord(bt, "BOOT_UEFI_GOP_OK");

        let (
            is_installed,
            lang,
            theme_idx,
            wallpaper_idx,
            ntp_enabled,
            ntp_server,
            tz_offset,
            ai_perm,
        ) = if let Some(cfg) = installed_opt {
            auth = UserAuthManager::from_installed(
                &cfg.admin_username,
                cfg.admin_salt,
                cfg.admin_hash,
                &cfg.user_username,
                cfg.user_salt,
                cfg.user_hash,
            );
            let l = if cfg.language_zh {
                UiLang::Zh
            } else {
                UiLang::En
            };
            let perm = AiPermissionLevel::from_u8(cfg.ai_perm_level);
            (
                true,
                l,
                (cfg.theme_index as usize) % THEMES.len(),
                (cfg.wallpaper_index as usize) % 3,
                cfg.ntp_enabled,
                cfg.ntp_server,
                cfg.timezone_offset,
                perm,
            )
        } else {
            // Live CD / Trial mode before installation: Default language is English
            (
                false,
                UiLang::En,
                0,
                0,
                true,
                String::from("pool.ntp.org"),
                8,
                AiPermissionLevel::L2DesktopControl,
            )
        };

        // Load or generate initial wallpaper and adaptively scale to screen resolution
        let raw_wp = Self::load_or_build_wallpaper(bt, wallpaper_idx);
        let wallpaper_scaled = raw_wp.scale_adaptive(screen_w, screen_h);

        let windows = [
            DesktopWindow {
                id: WindowId::Installer,
                title_en: "Install AuraOS to Disk (Installer Wizard)",
                title_zh: "安装 AuraOS 到磁盘 (系统安装向导)",
                x: 160,
                y: 55,
                w: 720,
                h: 520,
                saved_x: 160,
                saved_y: 55,
                saved_w: 720,
                saved_h: 520,
                is_open: false, // In Live mode, user double-clicks Desktop icon to open!
                is_minimized: false,
                is_maximized: false,
                z_order: 5,
            },
            DesktopWindow {
                id: WindowId::FileManager,
                title_en: "File Manager (FAT32 Disk & Image Viewer)",
                title_zh: "文件管理器 (FAT32 磁盘与图片预览)",
                x: 130,
                y: 65,
                w: 760,
                h: 500,
                saved_x: 130,
                saved_y: 65,
                saved_w: 760,
                saved_h: 500,
                is_open: is_installed,
                is_minimized: false,
                is_maximized: false,
                z_order: 2,
            },
            DesktopWindow {
                id: WindowId::AiAgent,
                title_en: "AuraOS Autonomous MCP Agent & Model Studio",
                title_zh: "AuraOS 自主 MCP 智能体与模型中心",
                x: 190,
                y: 70,
                w: 780,
                h: 520,
                saved_x: 190,
                saved_y: 70,
                saved_w: 780,
                saved_h: 520,
                is_open: false,
                is_minimized: false,
                is_maximized: false,
                z_order: 4,
            },
            DesktopWindow {
                id: WindowId::Terminal,
                title_en: "AuraOS Kernel Terminal (ip / curl / mcp / fs)",
                title_zh: "AuraOS 内核终端 (ip / curl / mcp / 文件)",
                x: 220,
                y: 95,
                w: 720,
                h: 460,
                saved_x: 220,
                saved_y: 95,
                saved_w: 720,
                saved_h: 460,
                is_open: false,
                is_minimized: false,
                is_maximized: false,
                z_order: 3,
            },
            DesktopWindow {
                id: WindowId::Settings,
                title_en: "System Settings (Personalization, AI Authority, Users, Time)",
                title_zh: "系统设置 (个性化壁纸配色、AI权限、账户、时间)",
                x: 180,
                y: 80,
                w: 740,
                h: 500,
                saved_x: 180,
                saved_y: 80,
                saved_w: 740,
                saved_h: 500,
                is_open: false,
                is_minimized: false,
                is_maximized: false,
                z_order: 1,
            },
        ];

        let mut term_logs = Vec::new();
        term_logs.push(String::from(
            "AuraOS Bare-Metal Kernel v2.0 (UEFI + GOP + FAT32 + PCIe NIC + MCP Agent)",
        ));
        term_logs.push(format!(
            "NIC Detected: {} | MAC {} | IP {}",
            net.nic_model,
            net.mac_string(),
            net.ip_string()
        ));
        term_logs.push(String::from(
            "Type 'help', 'ip a', 'ip route', 'ls', 'cat \\AURAOS\\README.TXT', 'curl https://example.com', or 'mcp'.",
        ));

        sound.play_boot_chime(bt);

        let mut desktop = Self {
            is_installed,
            lang,
            theme_idx,
            wallpaper_idx,
            wallpaper_scaled,
            windows,
            active_win: if is_installed {
                WindowId::FileManager
            } else {
                WindowId::Installer
            },
            dragging_win: None,
            start_menu_open: false,
            clipboard: String::from("https://example.com"),
            focused_input: FocusedInput::None,
            sound,
            net,
            auth,
            ai_perm,
            agent: AgentAutonomousEngine::new(),
            installer: InstallerWizard::new(),
            audit_chain,
            ntp_enabled,
            ntp_server,
            tz_offset,
            current_dir: String::from("\\AURAOS"),
            dir_entries: Vec::new(),
            selected_file_idx: 0,
            file_preview_name: String::from("\\AURAOS\\README.TXT"),
            file_preview_content: String::new(),
            new_file_name: String::from("\\AURAOS\\NOTES.TXT"),
            new_file_content: String::from("Saved persistently to FAT32 disk!"),
            term_input: String::new(),
            term_logs,
            ai_prompt_input: String::from("Inspect system info, check IP, and write report"),
            show_mcp_prompt_modal: false,
        };
        desktop.refresh_file_manager(bt);
        desktop
    }

    pub fn load_or_build_wallpaper(bt: &BootServices, idx: usize) -> DecodedImage {
        let path = match idx % 3 {
            0 => "\\AURAOS\\WALLPAPERS\\SKY.PNG",
            1 => "\\AURAOS\\WALLPAPERS\\DAWN.JPG",
            _ => "\\AURAOS\\WALLPAPERS\\MINT.BMP",
        };
        if let Some(bytes) = DiskStorage::read_file(bt, path) {
            if let Some(img) = decode_image_bytes(&bytes) {
                return img;
            }
        }
        generate_builtin_wallpaper(idx, 320, 200)
    }

    pub fn switch_wallpaper(&mut self, bt: &BootServices, idx: usize, sw: usize, sh: usize) {
        self.wallpaper_idx = idx % 3;
        let raw = Self::load_or_build_wallpaper(bt, self.wallpaper_idx);
        self.wallpaper_scaled = raw.scale_adaptive(sw, sh);
    }

    pub fn refresh_file_manager(&mut self, bt: &BootServices) {
        self.dir_entries = DiskStorage::list_dir(bt, &self.current_dir);
        if let Some(bytes) = DiskStorage::read_file(bt, &self.file_preview_name) {
            if self.file_preview_name.ends_with(".PNG")
                || self.file_preview_name.ends_with(".JPG")
                || self.file_preview_name.ends_with(".BMP")
            {
                if let Some(img) = decode_image_bytes(&bytes) {
                    self.file_preview_content = format!(
                        "[IMAGE DECODED OK] {} ({}x{} pixels, {} bytes)",
                        self.file_preview_name,
                        img.width,
                        img.height,
                        bytes.len()
                    );
                } else {
                    self.file_preview_content =
                        format!("[Binary Image File: {} bytes]", bytes.len());
                }
            } else if let Ok(s) = core::str::from_utf8(&bytes) {
                self.file_preview_content = String::from(s);
            } else {
                self.file_preview_content = format!("[Binary File: {} bytes]", bytes.len());
            }
        }
    }

    pub fn open_and_focus_window(&mut self, id: WindowId) {
        let max_z = self.windows.iter().map(|w| w.z_order).max().unwrap_or(1);
        for w in self.windows.iter_mut() {
            if w.id == id {
                w.is_open = true;
                w.is_minimized = false;
                w.z_order = max_z + 1;
            }
        }
        self.active_win = id;
        self.focused_input = match id {
            WindowId::Installer => FocusedInput::InstallerField,
            WindowId::Terminal => FocusedInput::TerminalCmd,
            WindowId::AiAgent => FocusedInput::AiPrompt,
            WindowId::FileManager => FocusedInput::FileMgrNewContent,
            WindowId::Settings => FocusedInput::SettingsNtpServer,
        };
    }

    // ========================================================================
    // BUILT-IN OS MCP TOOL DISPATCHER (REAL KERNEL EXECUTION)
    // ========================================================================

    pub fn execute_mcp_tool(
        &mut self,
        bt: &BootServices,
        mem_report: &SystemMemoryReport,
        screen_w: usize,
        screen_h: usize,
        tool: McpToolId,
        arg: &str,
    ) -> String {
        if !check_mcp_permission(self.ai_perm, self.auth.current_role(), tool) {
            self.sound.play_error_beep(bt);
            return format!(
                "PERMISSION_DENIED: Tool '{}' requires higher AI/User authority (Current AI={}, Role={})",
                tool.name(),
                self.ai_perm.label_en(),
                self.auth.current_role().label_en()
            );
        }

        self.sound.play_agent_step(bt);

        match tool {
            McpToolId::SysGetInfo => {
                format!(
                    "RAM={}MB Free={}MB NIC={} Role={} AI_Perm={}",
                    mem_report.total_ram_mb(),
                    mem_report.usable_ram_mb(),
                    self.net.nic_model,
                    self.auth.current_role().label_en(),
                    self.ai_perm.label_en()
                )
            }
            McpToolId::NetGetIp => {
                format!(
                    "eth0 {} ip={}/24 gw={}",
                    self.net.nic_model,
                    self.net.ip_string(),
                    self.net.gw_string()
                )
            }
            McpToolId::FsListDir => {
                let path = if arg.trim().is_empty() {
                    "\\AURAOS"
                } else {
                    arg.trim()
                };
                let entries = DiskStorage::list_dir(bt, path);
                let mut out = format!("DIR {} ({} items): ", path, entries.len());
                for (name, sz, is_d) in entries.iter().take(6) {
                    let _ = write!(out, "{}{}({}B) ", name, if *is_d { "/" } else { "" }, sz);
                }
                out
            }
            McpToolId::FsReadFile => {
                let path = if arg.trim().is_empty() {
                    "\\AURAOS\\README.TXT"
                } else {
                    arg.trim()
                };
                if let Some(bytes) = DiskStorage::read_file(bt, path) {
                    let text = core::str::from_utf8(&bytes).unwrap_or("[binary]");
                    let snippet: String = text.chars().take(80).collect();
                    format!("READ {} ({}B): {}", path, bytes.len(), snippet)
                } else {
                    format!("ERR_NOT_FOUND: {}", path)
                }
            }
            McpToolId::UiSetTheme => {
                let idx = arg.trim().parse::<usize>().unwrap_or(1) % THEMES.len();
                self.theme_idx = idx;
                format!("THEME_CHANGED: {} ({})", idx, THEMES[idx].name_en)
            }
            McpToolId::UiSetWallpaper => {
                let idx = arg.trim().parse::<usize>().unwrap_or(1) % 3;
                self.switch_wallpaper(bt, idx, screen_w, screen_h);
                format!("WALLPAPER_CHANGED: index={}", idx)
            }
            McpToolId::UiOpenWindow => {
                let target = match arg.trim().to_ascii_lowercase().as_str() {
                    "files" | "filemanager" => WindowId::FileManager,
                    "settings" => WindowId::Settings,
                    "installer" => WindowId::Installer,
                    "ai" | "agent" => WindowId::AiAgent,
                    _ => WindowId::Terminal,
                };
                self.open_and_focus_window(target);
                format!("WINDOW_OPENED: {}", arg.trim())
            }
            McpToolId::AudioPlayTone => {
                let freq = arg.trim().parse::<u32>().unwrap_or(880).clamp(120, 4000);
                self.sound.play_tone_blocking(bt, freq, 45);
                format!("AUDIO_PLAYED: {}Hz (8254 PIT 0x42/0x61)", freq)
            }
            McpToolId::FsWriteFile => {
                let (path, content) = if let Some(pos) = arg.find('|') {
                    (arg[..pos].trim(), &arg[pos + 1..])
                } else {
                    ("\\AURAOS\\AGENT_OUT.TXT", arg)
                };
                if DiskStorage::write_file(bt, path, content.as_bytes()) {
                    self.refresh_file_manager(bt);
                    format!("DISK_WRITE_OK: {} ({} bytes persisted)", path, content.len())
                } else {
                    format!("DISK_WRITE_ERR: {}", path)
                }
            }
            McpToolId::ShExecCmd => {
                let cmd = arg.trim();
                self.execute_terminal_command(bt, mem_report, screen_w, screen_h, cmd)
            }
        }
    }

    // ========================================================================
    // KERNEL TERMINAL COMMAND EXECUTOR (ip, curl, ls, cat, write, mcp, etc.)
    // ========================================================================

    pub fn execute_terminal_command(
        &mut self,
        bt: &BootServices,
        mem_report: &SystemMemoryReport,
        screen_w: usize,
        screen_h: usize,
        raw_cmd: &str,
    ) -> String {
        let cmd = raw_cmd.trim();
        if cmd.is_empty() {
            return String::new();
        }
        self.term_logs.push(format!("{}@aura-os:# {}", self.auth.current_username(), cmd));

        let resp = if cmd == "help" {
            String::from(
                "Commands: ip [a|route|link|set], curl <http/https URL>, ls [dir], cat <file>, write <file> <text>, mkdir <dir>, rm <file>, whoami, su <admin|user> <pin>, mcp, beep",
            )
        } else if cmd == "ip" || cmd.starts_with("ip ") {
            let sub = if cmd.len() > 3 { &cmd[3..] } else { "a" };
            let lines = self.net.run_ip_command(sub);
            for l in &lines {
                self.term_logs.push(l.clone());
            }
            lines.first().cloned().unwrap_or_default()
        } else if cmd.starts_with("curl ") {
            let url = cmd[5..].trim();
            let res = self.net.http_request(bt, "GET", url, "");
            self.term_logs.push(format!(
                "[{}] HTTP {} ({} ms, via {})",
                res.protocol, res.status_code, res.latency_ms, self.net.nic_model
            ));
            for line in res.body.lines().take(5) {
                self.term_logs.push(String::from(line));
            }
            format!("HTTP {} ({}B)", res.status_code, res.body.len())
        } else if cmd == "ls" || cmd.starts_with("ls ") {
            let dir = if cmd.len() > 3 {
                cmd[3..].trim()
            } else {
                &self.current_dir
            };
            let list = DiskStorage::list_dir(bt, dir);
            let mut summary = String::new();
            for (name, sz, is_d) in list {
                let entry = format!(
                    "  {:<18} {:>8} B  {}",
                    name,
                    sz,
                    if is_d { "<DIR>" } else { "FILE" }
                );
                self.term_logs.push(entry);
                let _ = write!(summary, "{} ", name);
            }
            summary
        } else if cmd.starts_with("cat ") {
            let path = cmd[4..].trim();
            if let Some(bytes) = DiskStorage::read_file(bt, path) {
                let text = core::str::from_utf8(&bytes).unwrap_or("[Binary Data]");
                for line in text.lines().take(6) {
                    self.term_logs.push(String::from(line));
                }
                String::from(text)
            } else {
                let err = format!("cat: {}: No such file on FAT32 volume", path);
                self.term_logs.push(err.clone());
                err
            }
        } else if cmd.starts_with("write ") {
            if !self.auth.current_role().can_modify_system()
                && cmd.contains("INSTALLED.CFG")
            {
                let err = String::from("Permission denied: Admin required for system config");
                self.term_logs.push(err.clone());
                err
            } else {
                let rest = cmd[6..].trim();
                if let Some(sp) = rest.find(' ') {
                    let path = rest[..sp].trim();
                    let body = &rest[sp + 1..];
                    if DiskStorage::write_file(bt, path, body.as_bytes()) {
                        self.refresh_file_manager(bt);
                        let msg = format!("Persisted {} bytes to {}", body.len(), path);
                        self.term_logs.push(msg.clone());
                        msg
                    } else {
                        let msg = format!("Failed to write {}", path);
                        self.term_logs.push(msg.clone());
                        msg
                    }
                } else {
                    String::from("Usage: write <\\AURAOS\\FILE.TXT> <content>")
                }
            }
        } else if cmd.starts_with("mkdir ") {
            let path = cmd[6..].trim();
            if DiskStorage::mkdir(bt, path) {
                self.refresh_file_manager(bt);
                let msg = format!("Directory created: {}", path);
                self.term_logs.push(msg.clone());
                msg
            } else {
                String::from("mkdir failed")
            }
        } else if cmd.starts_with("rm ") {
            let path = cmd[3..].trim();
            if DiskStorage::delete_file(bt, path) {
                self.refresh_file_manager(bt);
                let msg = format!("Removed {}", path);
                self.term_logs.push(msg.clone());
                msg
            } else {
                String::from("rm failed")
            }
        } else if cmd == "whoami" {
            let msg = format!(
                "User={} Role={} AI_Perm={}",
                self.auth.current_username(),
                self.auth.current_role().label_en(),
                self.ai_perm.label_en()
            );
            self.term_logs.push(msg.clone());
            msg
        } else if cmd.starts_with("su ") {
            let parts: Vec<&str> = cmd[3..].split_whitespace().collect();
            if parts.len() >= 2 {
                if self.auth.authenticate(parts[0], parts[1]) {
                    let msg = format!(
                        "Switched to {} ({})",
                        self.auth.current_username(),
                        self.auth.current_role().label_en()
                    );
                    self.term_logs.push(msg.clone());
                    msg
                } else {
                    self.sound.play_error_beep(bt);
                    let msg = String::from("su: Authentication failed");
                    self.term_logs.push(msg.clone());
                    msg
                }
            } else {
                String::from("Usage: su <admin|user> <pin>")
            }
        } else if cmd == "mcp" {
            for spec in MCP_TOOLS.iter() {
                self.term_logs.push(format!(
                    "  [MCP] {:<16} (MinAI:L{}, AdminOnly:{}) - {}",
                    spec.name, spec.min_ai_level, spec.requires_admin_user, spec.description_en
                ));
            }
            String::from("Listed 10 built-in OS MCP tools")
        } else if cmd == "beep" {
            self.sound.play_boot_chime(bt);
            let msg = String::from("Played 8254 PIT hardware audio chime");
            self.term_logs.push(msg.clone());
            msg
        } else if cmd.starts_with("TOOL_CALL:") {
            if let Some((tid, arg)) = AgentAutonomousEngine::parse_tool_call(cmd) {
                let res = self.execute_mcp_tool(bt, mem_report, screen_w, screen_h, tid, &arg);
                self.term_logs.push(format!("  -> {}", res));
                res
            } else {
                String::from("Invalid TOOL_CALL syntax")
            }
        } else {
            let msg = format!("Unknown command '{}'. Type 'help'.", cmd);
            self.term_logs.push(msg.clone());
            msg
        };

        while self.term_logs.len() > 14 {
            self.term_logs.remove(0);
        }
        resp
    }

    // ========================================================================
    // FINISH INSTALLATION & PERSIST SYSTEM CONFIG TO DISK
    // ========================================================================

    pub fn commit_installation_to_disk(&mut self, bt: &BootServices, screen_w: usize, screen_h: usize) {
        let admin_salt: [u8; 16] = *b"AuraOS_AdminSalt";
        let user_salt: [u8; 16] = *b"AuraOS_User_Salt";
        let admin_hash =
            CryptoEngine::DeriveKeyPbkdf2(self.installer.admin_pass.as_bytes(), &admin_salt, 600);
        let user_hash =
            CryptoEngine::DeriveKeyPbkdf2(self.installer.std_pass.as_bytes(), &user_salt, 600);

        let cfg = InstalledSystemConfig {
            installed: true,
            boot_from_disk: true,
            language_zh: matches!(self.installer.lang, UiLang::Zh),
            ntp_enabled: self.installer.ntp_enabled,
            ntp_server: self.installer.ntp_server.clone(),
            timezone_offset: self.installer.tz_offset,
            ai_protocol: self.installer.ai_protocol.clone(),
            ai_model: self.installer.ai_model.clone(),
            ai_endpoint: self.installer.ai_endpoint.clone(),
            ai_api_key: self.installer.ai_api_key.clone(),
            ai_perm_level: self.installer.ai_perm as u8,
            admin_username: self.installer.admin_user.clone(),
            admin_salt,
            admin_hash,
            user_username: self.installer.std_user.clone(),
            user_salt,
            user_hash,
            theme_index: self.theme_idx as u8,
            wallpaper_index: self.wallpaper_idx as u8,
        };

        if DiskStorage::save_installed_system(bt, &cfg) {
            self.is_installed = true;
            self.lang = self.installer.lang;
            self.ntp_enabled = self.installer.ntp_enabled;
            self.ntp_server = self.installer.ntp_server.clone();
            self.tz_offset = self.installer.tz_offset;
            self.ai_perm = self.installer.ai_perm;
            self.agent.protocol = self.installer.ai_protocol.clone();
            self.agent.model = self.installer.ai_model.clone();
            self.agent.endpoint = self.installer.ai_endpoint.clone();
            self.agent.api_key = self.installer.ai_api_key.clone();
            self.auth = UserAuthManager::from_installed(
                &cfg.admin_username,
                cfg.admin_salt,
                cfg.admin_hash,
                &cfg.user_username,
                cfg.user_salt,
                cfg.user_hash,
            );
            self.installer.step = InstallerStep::Completed;
            self.installer.status_msg = String::from(
                "INSTALLED OK! Saved \\AURAOS\\INSTALLED.CFG & switched boot sequence to Disk Boot.",
            );
            self.refresh_file_manager(bt);
            self.switch_wallpaper(bt, self.wallpaper_idx, screen_w, screen_h);
            self.sound.play_boot_chime(bt);
        } else {
            self.installer.status_msg = String::from("ERROR: Could not write to FAT32 volume.");
            self.sound.play_error_beep(bt);
        }
    }

    pub fn save_current_settings_to_disk(&mut self, bt: &BootServices) {
        if !self.is_installed {
            return;
        }
        let cfg = InstalledSystemConfig {
            installed: true,
            boot_from_disk: true,
            language_zh: matches!(self.lang, UiLang::Zh),
            ntp_enabled: self.ntp_enabled,
            ntp_server: self.ntp_server.clone(),
            timezone_offset: self.tz_offset,
            ai_protocol: self.agent.protocol.clone(),
            ai_model: self.agent.model.clone(),
            ai_endpoint: self.agent.endpoint.clone(),
            ai_api_key: self.agent.api_key.clone(),
            ai_perm_level: self.ai_perm as u8,
            admin_username: self.auth.users[0].username.clone(),
            admin_salt: self.auth.users[0].salt,
            admin_hash: self.auth.users[0].password_hash,
            user_username: self.auth.users[1].username.clone(),
            user_salt: self.auth.users[1].salt,
            user_hash: self.auth.users[1].password_hash,
            theme_index: self.theme_idx as u8,
            wallpaper_index: self.wallpaper_idx as u8,
        };
        let _ = DiskStorage::save_installed_system(bt, &cfg);
    }

    // ========================================================================
    // KEYBOARD INPUT & CTRL+C / CTRL+V SHORTCUT HANDLER
    // ========================================================================

    pub fn active_input_mut(&mut self) -> Option<&mut String> {
        match self.focused_input {
            FocusedInput::None => None,
            FocusedInput::TerminalCmd => Some(&mut self.term_input),
            FocusedInput::AiPrompt => Some(&mut self.ai_prompt_input),
            FocusedInput::AiEndpoint => Some(&mut self.agent.endpoint),
            FocusedInput::AiModel => Some(&mut self.agent.model),
            FocusedInput::AiKey => Some(&mut self.agent.api_key),
            FocusedInput::FileMgrNewName => Some(&mut self.new_file_name),
            FocusedInput::FileMgrNewContent => Some(&mut self.new_file_content),
            FocusedInput::SettingsNtpServer => Some(&mut self.ntp_server),
            FocusedInput::InstallerField => match self.installer.step {
                InstallerStep::TimeNtpConfig => Some(&mut self.installer.ntp_server),
                InstallerStep::AiConfig => match self.installer.active_field % 3 {
                    0 => Some(&mut self.installer.ai_model),
                    1 => Some(&mut self.installer.ai_endpoint),
                    _ => Some(&mut self.installer.ai_api_key),
                },
                InstallerStep::AdminRegister => match self.installer.active_field % 4 {
                    0 => Some(&mut self.installer.admin_user),
                    1 => Some(&mut self.installer.admin_pass),
                    2 => Some(&mut self.installer.std_user),
                    _ => Some(&mut self.installer.std_pass),
                },
                _ => None,
            },
        }
    }

    pub fn handle_key_event(
        &mut self,
        bt: &BootServices,
        mem_report: &SystemMemoryReport,
        screen_w: usize,
        screen_h: usize,
        cursor: &mut CursorController,
        tick: u64,
        key: Key,
    ) {
        match key {
            Key::Special(scan) => match scan {
                // F12 activates/toggles Keyboard Mouse Emulation and triggers sonar locator ring
                ScanCode::FUNCTION_12 => {
                    cursor.trigger_f12(screen_w, screen_h);
                    self.sound.play_click();
                }
                // F2 cycles UI language
                ScanCode::FUNCTION_2 => {
                    self.lang = match self.lang {
                        UiLang::En => UiLang::Zh,
                        UiLang::Zh => UiLang::En,
                    };
                    self.save_current_settings_to_disk(bt);
                }
                // F3 cycles White-Blue themes
                ScanCode::FUNCTION_3 => {
                    self.theme_idx = (self.theme_idx + 1) % THEMES.len();
                    self.save_current_settings_to_disk(bt);
                }
                // F4 cycles Adaptive PNG/JPG Wallpapers
                ScanCode::FUNCTION_4 => {
                    let next_wp = (self.wallpaper_idx + 1) % 3;
                    self.switch_wallpaper(bt, next_wp, screen_w, screen_h);
                    self.save_current_settings_to_disk(bt);
                }
                // Support physical Numpad keys when NumLock is OFF in OVMF/UEFI:
                // - Physical Key '0' (Ins)  -> ScanCode::INSERT    -> Action '0': Down
                // - Physical Key '2' (Down) -> ScanCode::DOWN      -> Action '2': UP (2是往上!)
                // - Physical Key '1' (End)  -> ScanCode::END       -> Action '1': Left
                // - Physical Key '3' (PgDn) -> ScanCode::PAGE_DOWN -> Action '3': Right
                // - Physical Key '4' (Left) -> ScanCode::LEFT      -> Action '4': Cycle Hold/Drag mode!
                // - Physical Key '7' (Home) -> ScanCode::HOME      -> Action '7': Left Click / Double Click
                // - Physical Key '9' (PgUp) -> ScanCode::PAGE_UP   -> Action '9': Right Click
                ScanCode::INSERT => {
                    let _ = cursor.handle_kbd_mouse_char('0', tick, screen_w, screen_h, &mut self.sound);
                }
                ScanCode::DOWN => {
                    // Physical Key '2' on numpad emits ScanCode::DOWN when NumLock is OFF!
                    // User specification: '2' MUST MOVE UP (2是往上!)
                    let _ = cursor.handle_kbd_mouse_char('2', tick, screen_w, screen_h, &mut self.sound);
                }
                ScanCode::END => {
                    let _ = cursor.handle_kbd_mouse_char('1', tick, screen_w, screen_h, &mut self.sound);
                }
                ScanCode::PAGE_DOWN => {
                    let _ = cursor.handle_kbd_mouse_char('3', tick, screen_w, screen_h, &mut self.sound);
                }
                ScanCode::LEFT => {
                    // Physical Key '4' on numpad emits ScanCode::LEFT when NumLock is OFF!
                    // User specification: '4' MUST CYCLE HOLD/DRAG MODE!
                    let _ = cursor.handle_kbd_mouse_char('4', tick, screen_w, screen_h, &mut self.sound);
                }
                ScanCode::HOME => {
                    let _ = cursor.handle_kbd_mouse_char('7', tick, screen_w, screen_h, &mut self.sound);
                }
                ScanCode::PAGE_UP => {
                    let _ = cursor.handle_kbd_mouse_char('9', tick, screen_w, screen_h, &mut self.sound);
                }
                ScanCode::UP => {
                    cursor.move_by(0, -cursor.step_px, screen_w, screen_h);
                }
                ScanCode::RIGHT => {
                    cursor.move_by(cursor.step_px, 0, screen_w, screen_h);
                }
                _ => {}
            },
            Key::Printable(ch16) => {
                let raw_ch = char::from(ch16);

                // Check Ctrl+C (0x03) and Ctrl+V (0x16)
                if raw_ch == '\u{0003}' {
                    let copied = self.active_input_mut().map(|s| s.clone());
                    if let Some(txt) = copied {
                        if !txt.is_empty() {
                            self.clipboard = txt;
                            self.sound.play_click();
                        }
                    }
                    return;
                }
                if raw_ch == '\u{0016}' {
                    let clip = self.clipboard.clone();
                    if let Some(target) = self.active_input_mut() {
                        target.push_str(&clip);
                        self.sound.play_click();
                    }
                    return;
                }

                // Tab switches active field in Installer
                if raw_ch == '\t' {
                    self.installer.active_field = self.installer.active_field.wrapping_add(1);
                    return;
                }

                // First check if Keyboard Mouse Emulation handles 0/2/1/3/7/9/4
                if cursor.handle_kbd_mouse_char(raw_ch, tick, screen_w, screen_h, &mut self.sound) {
                    return;
                }

                // Enter key submits terminal command or triggers AI Agent
                if raw_ch == '\r' || raw_ch == '\n' {
                    if self.focused_input == FocusedInput::TerminalCmd {
                        let cmd = self.term_input.clone();
                        self.term_input.clear();
                        let _ = self.execute_terminal_command(bt, mem_report, screen_w, screen_h, &cmd);
                    } else if self.focused_input == FocusedInput::AiPrompt {
                        let goal = self.ai_prompt_input.clone();
                        self.agent.start_goal(&goal);
                    }
                    return;
                }

                // Backspace
                if raw_ch == '\u{0008}' || raw_ch == '\u{007f}' {
                    if let Some(target) = self.active_input_mut() {
                        target.pop();
                    }
                    return;
                }

                // Printable ASCII into focused input box
                if (' '..='~').contains(&raw_ch) {
                    if let Some(target) = self.active_input_mut() {
                        if target.len() < 96 {
                            target.push(raw_ch);
                        }
                    }
                }
            }
        }
    }

    // ========================================================================
    // MOUSE / EMULATED CURSOR INTERACTION & WINDOW DRAG / MAXIMIZE / RESTORE
    // ========================================================================

    pub fn handle_pointer_interaction(
        &mut self,
        bt: &BootServices,
        mem_report: &SystemMemoryReport,
        screen_w: usize,
        screen_h: usize,
        cursor: &mut CursorController,
    ) {
        let cx = cursor.x.max(0) as usize;
        let cy = cursor.y.max(0) as usize;
        let taskbar_y = screen_h.saturating_sub(44);

        // 1. Handle ongoing window drag when HoldState::LeftHeld or left_down
        if let Some((drag_id, off_x, off_y)) = self.dragging_win {
            if cursor.left_down {
                cursor.state = CursorState::GrabDrag;
                for w in self.windows.iter_mut() {
                    if w.id == drag_id && !w.is_maximized {
                        let nx = (cursor.x - off_x).max(0) as usize;
                        let ny = (cursor.y - off_y).max(0) as usize;
                        w.x = nx.min(screen_w.saturating_sub(120));
                        w.y = ny.min(taskbar_y.saturating_sub(40));
                    }
                }
                return;
            } else {
                self.dragging_win = None;
                cursor.state = CursorState::Arrow;
            }
        }

        // Update smooth hover cursor state (HandHover over desktop icons, taskbar, or window titlebars)
        cursor.state = if cy >= taskbar_y || (cx >= 18 && cx <= 118 && cy <= 540) {
            CursorState::HandHover
        } else {
            CursorState::Arrow
        };

        if !cursor.left_clicked && !cursor.double_clicked && !cursor.right_clicked {
            return;
        }

        // 2. Bottom Taskbar click handling
        if cy >= taskbar_y {
            // Start Button [AuraOS]
            if cx >= 8 && cx <= 118 {
                self.start_menu_open = !self.start_menu_open;
                return;
            }
            // Taskbar window tabs (x = 130 .. 730)
            let tab_ids = [
                WindowId::Installer,
                WindowId::FileManager,
                WindowId::AiAgent,
                WindowId::Terminal,
                WindowId::Settings,
            ];
            for (i, &wid) in tab_ids.iter().enumerate() {
                let tx = 130 + i * 116;
                if cx >= tx && cx <= tx + 110 {
                    let is_min = self
                        .windows
                        .iter()
                        .find(|w| w.id == wid)
                        .map(|w| w.is_minimized && w.is_open)
                        .unwrap_or(false);
                    let is_open = self
                        .windows
                        .iter()
                        .find(|w| w.id == wid)
                        .map(|w| w.is_open)
                        .unwrap_or(false);
                    if !is_open || is_min || self.active_win != wid {
                        self.open_and_focus_window(wid);
                    } else {
                        for w in self.windows.iter_mut() {
                            if w.id == wid {
                                w.is_minimized = true;
                            }
                        }
                    }
                    self.start_menu_open = false;
                    return;
                }
            }
            // Right System Tray quick toggles (Audio / Role / KbdMouse)
            if cx >= screen_w.saturating_sub(330) && cx < screen_w.saturating_sub(230) {
                cursor.kbd_mouse_enabled = !cursor.kbd_mouse_enabled;
                return;
            }
            if cx >= screen_w.saturating_sub(225) && cx < screen_w.saturating_sub(150) {
                self.sound.enabled = !self.sound.enabled;
                return;
            }
            if cx >= screen_w.saturating_sub(148) && cx < screen_w.saturating_sub(75) {
                self.auth.switch_role_quick();
                return;
            }
            return;
        }

        // 3. Start Menu popup click handling
        if self.start_menu_open {
            let sm_x = 8usize;
            let sm_y = taskbar_y.saturating_sub(270);
            let sm_w = 230usize;
            let sm_h = 264usize;
            if cx >= sm_x && cx <= sm_x + sm_w && cy >= sm_y && cy <= sm_y + sm_h {
                let rel_y = cy.saturating_sub(sm_y + 38);
                let row = rel_y / 36;
                match row {
                    0 => self.open_and_focus_window(WindowId::Installer),
                    1 => self.open_and_focus_window(WindowId::FileManager),
                    2 => self.open_and_focus_window(WindowId::AiAgent),
                    3 => self.open_and_focus_window(WindowId::Terminal),
                    4 => self.open_and_focus_window(WindowId::Settings),
                    _ => {
                        self.auth.switch_role_quick();
                    }
                }
                self.start_menu_open = false;
                return;
            } else {
                self.start_menu_open = false;
            }
        }

        // 4. Check open, non-minimized windows in descending Z-order
        let mut sorted_indices: [usize; 5] = [0, 1, 2, 3, 4];
        sorted_indices.sort_by(|&a, &b| self.windows[b].z_order.cmp(&self.windows[a].z_order));

        for &idx in &sorted_indices {
            let win = self.windows[idx];
            if !win.is_open || win.is_minimized {
                continue;
            }
            if cx >= win.x && cx <= win.x + win.w && cy >= win.y && cy <= win.y + win.h {
                self.open_and_focus_window(win.id);

                // Check Window Titlebar (height = 34px)
                if cy <= win.y + 34 {
                    // Minimize button [-]
                    let btn_min_x = win.x + win.w.saturating_sub(96);
                    // Maximize / Restore button [□] / [❐]
                    let btn_max_x = win.x + win.w.saturating_sub(64);
                    // Close button [×]
                    let btn_cls_x = win.x + win.w.saturating_sub(32);

                    if cx >= btn_min_x && cx < btn_min_x + 26 {
                        self.windows[idx].is_minimized = true;
                        return;
                    }
                    if cx >= btn_max_x && cx < btn_max_x + 26 {
                        self.windows[idx].toggle_maximize(screen_w, screen_h);
                        return;
                    }
                    if cx >= btn_cls_x && cx < btn_cls_x + 26 {
                        self.windows[idx].is_open = false;
                        return;
                    }

                    // Double-clicking titlebar also toggles Maximize / Restore!
                    if cursor.double_clicked {
                        self.windows[idx].toggle_maximize(screen_w, screen_h);
                        return;
                    }

                    // Start dragging floating window if not maximized
                    if !self.windows[idx].is_maximized {
                        let off_x = cursor.x - (win.x as i32);
                        let off_y = cursor.y - (win.y as i32);
                        self.dragging_win = Some((win.id, off_x, off_y));
                    }
                    return;
                }

                // Dispatch inside-window content clicks
                let wx = win.x;
                let wy = win.y;
                let ww = win.w;
                let wh = win.h;
                match win.id {
                    WindowId::Installer => {
                        self.click_installer_window(bt, screen_w, screen_h, wx, wy, ww, wh, cx, cy)
                    }
                    WindowId::FileManager => {
                        self.click_file_manager_window(bt, wx, wy, ww, wh, cx, cy)
                    }
                    WindowId::AiAgent => {
                        self.click_ai_agent_window(bt, mem_report, screen_w, screen_h, wx, wy, ww, wh, cx, cy)
                    }
                    WindowId::Terminal => {
                        self.click_terminal_window(bt, mem_report, screen_w, screen_h, wx, wy, ww, wh, cx, cy)
                    }
                    WindowId::Settings => {
                        self.click_settings_window(bt, screen_w, screen_h, wx, wy, ww, wh, cx, cy)
                    }
                }
                return;
            }
        }

        // 5. Desktop Icons (left column x = 22..112): Double-click or click opens app!
        if cx >= 18 && cx <= 118 {
            let icon_idx = cy.saturating_sub(24) / 86;
            // Allow single click or double click on desktop icons so both fast testing & standard double-click work smoothly
            if cursor.double_clicked || cursor.left_clicked {
                match icon_idx {
                    0 => self.open_and_focus_window(WindowId::Installer),
                    1 => self.open_and_focus_window(WindowId::FileManager),
                    2 => self.open_and_focus_window(WindowId::AiAgent),
                    3 => self.open_and_focus_window(WindowId::Terminal),
                    4 => self.open_and_focus_window(WindowId::Settings),
                    5 => self.auth.switch_role_quick(),
                    _ => {}
                }
            }
        }
    }

    fn click_installer_window(
        &mut self,
        bt: &BootServices,
        screen_w: usize,
        screen_h: usize,
        wx: usize,
        wy: usize,
        ww: usize,
        wh: usize,
        cx: usize,
        cy: usize,
    ) {
        self.focused_input = FocusedInput::InstallerField;
        let btn_y = wy + wh.saturating_sub(54);

        // Step-specific interactive buttons
        match self.installer.step {
            InstallerStep::WelcomeAsk => {
                // [Start Installation] button
                if cy >= wy + 250 && cy <= wy + 292 && cx >= wx + 60 && cx <= wx + 310 {
                    self.installer.step = InstallerStep::LanguageSelect;
                    return;
                }
                // [Continue Live Trial] button
                if cy >= wy + 250 && cy <= wy + 292 && cx >= wx + 330 && cx <= wx + 580 {
                    for w in self.windows.iter_mut() {
                        if w.id == WindowId::Installer {
                            w.is_open = false;
                        }
                    }
                    return;
                }
            }
            InstallerStep::LanguageSelect => {
                if cy >= wy + 150 && cy <= wy + 195 {
                    if cx >= wx + 60 && cx <= wx + 280 {
                        self.installer.lang = UiLang::En;
                    } else if cx >= wx + 300 && cx <= wx + 520 {
                        self.installer.lang = UiLang::Zh;
                    }
                }
            }
            InstallerStep::TimeNtpConfig => {
                if cy >= wy + 135 && cy <= wy + 170 {
                    if cx >= wx + 60 && cx <= wx + 250 {
                        self.installer.ntp_enabled = !self.installer.ntp_enabled;
                    } else if cx >= wx + 270 && cx <= wx + 450 {
                        self.installer.ntp_enabled = false;
                        self.installer.step = InstallerStep::AiConfig;
                        return;
                    }
                }
                if cy >= wy + 240 && cy <= wy + 274 {
                    if cx >= wx + 240 && cx <= wx + 290 {
                        self.installer.tz_offset = (self.installer.tz_offset - 1).clamp(-12, 14);
                    } else if cx >= wx + 360 && cx <= wx + 410 {
                        self.installer.tz_offset = (self.installer.tz_offset + 1).clamp(-12, 14);
                    }
                }
            }
            InstallerStep::AiConfig => {
                // Protocol toggle HTTP / HTTPS
                if cy >= wy + 125 && cy <= wy + 158 && cx >= wx + 220 && cx <= wx + 420 {
                    self.installer.ai_protocol = if self.installer.ai_protocol == "HTTPS" {
                        String::from("HTTP")
                    } else {
                        String::from("HTTPS")
                    };
                }
                // Field selection (Model / Endpoint / Key)
                if cy >= wy + 168 && cy <= wy + 200 {
                    self.installer.active_field = 0;
                } else if cy >= wy + 210 && cy <= wy + 242 {
                    self.installer.active_field = 1;
                } else if cy >= wy + 252 && cy <= wy + 284 {
                    self.installer.active_field = 2;
                }
                // AI Permission Level selector (L0..L3)
                if cy >= wy + 300 && cy <= wy + 336 {
                    let rel = cx.saturating_sub(wx + 60) / 145;
                    self.installer.ai_perm = AiPermissionLevel::from_u8(rel as u8);
                }
            }
            InstallerStep::AdminRegister => {
                if cy >= wy + 140 && cy <= wy + 174 {
                    self.installer.active_field = 0;
                } else if cy >= wy + 185 && cy <= wy + 219 {
                    self.installer.active_field = 1;
                } else if cy >= wy + 240 && cy <= wy + 274 {
                    self.installer.active_field = 2;
                } else if cy >= wy + 285 && cy <= wy + 319 {
                    self.installer.active_field = 3;
                }
            }
            InstallerStep::NetworkCheck => {
                if cy >= wy + 260 && cy <= wy + 296 && cx >= wx + 60 && cx <= wx + 280 {
                    self.net = NetworkManager::probe_hardware(bt);
                }
            }
            InstallerStep::ConfirmAndWrite => {
                if cy >= wy + 310 && cy <= wy + 354 && cx >= wx + 60 && cx <= wx + 380 {
                    self.commit_installation_to_disk(bt, screen_w, screen_h);
                    return;
                }
            }
            InstallerStep::Completed => {}
        }

        // Bottom [Previous] and [Next / Install] buttons
        if cy >= btn_y && cy <= btn_y + 38 {
            if cx >= wx + 40 && cx <= wx + 180 {
                self.installer.step = match self.installer.step {
                    InstallerStep::WelcomeAsk => InstallerStep::WelcomeAsk,
                    InstallerStep::LanguageSelect => InstallerStep::WelcomeAsk,
                    InstallerStep::TimeNtpConfig => InstallerStep::LanguageSelect,
                    InstallerStep::AiConfig => InstallerStep::TimeNtpConfig,
                    InstallerStep::AdminRegister => InstallerStep::AiConfig,
                    InstallerStep::NetworkCheck => InstallerStep::AdminRegister,
                    InstallerStep::ConfirmAndWrite => InstallerStep::NetworkCheck,
                    InstallerStep::Completed => InstallerStep::ConfirmAndWrite,
                };
            } else if cx >= wx + ww.saturating_sub(210) && cx <= wx + ww.saturating_sub(40) {
                match self.installer.step {
                    InstallerStep::WelcomeAsk => {
                        self.installer.step = InstallerStep::LanguageSelect
                    }
                    InstallerStep::LanguageSelect => {
                        self.installer.step = InstallerStep::TimeNtpConfig
                    }
                    InstallerStep::TimeNtpConfig => self.installer.step = InstallerStep::AiConfig,
                    InstallerStep::AiConfig => self.installer.step = InstallerStep::AdminRegister,
                    InstallerStep::AdminRegister => {
                        self.installer.step = InstallerStep::NetworkCheck
                    }
                    InstallerStep::NetworkCheck => {
                        self.installer.step = InstallerStep::ConfirmAndWrite
                    }
                    InstallerStep::ConfirmAndWrite => {
                        self.commit_installation_to_disk(bt, screen_w, screen_h);
                    }
                    InstallerStep::Completed => {
                        for w in self.windows.iter_mut() {
                            if w.id == WindowId::Installer {
                                w.is_open = false;
                            }
                        }
                    }
                }
            }
        }
    }

    fn click_file_manager_window(
        &mut self,
        bt: &BootServices,
        wx: usize,
        wy: usize,
        _ww: usize,
        _wh: usize,
        cx: usize,
        cy: usize,
    ) {
        // Top toolbar buttons: [\\AURAOS], [\\AURAOS\\WALLPAPERS], [Refresh], [Write File], [Delete]
        if cy >= wy + 44 && cy <= wy + 76 {
            if cx >= wx + 16 && cx <= wx + 125 {
                self.current_dir = String::from("\\AURAOS");
                self.refresh_file_manager(bt);
            } else if cx >= wx + 132 && cx <= wx + 310 {
                self.current_dir = String::from("\\AURAOS\\WALLPAPERS");
                self.refresh_file_manager(bt);
            } else if cx >= wx + 318 && cx <= wx + 420 {
                self.refresh_file_manager(bt);
            } else if cx >= wx + 428 && cx <= wx + 560 {
                let _ = DiskStorage::write_file(
                    bt,
                    &self.new_file_name,
                    self.new_file_content.as_bytes(),
                );
                self.file_preview_name = self.new_file_name.clone();
                self.refresh_file_manager(bt);
            } else if cx >= wx + 568 && cx <= wx + 690 {
                if self.auth.current_role().can_modify_system()
                    || !self.file_preview_name.contains("INSTALLED.CFG")
                {
                    let _ = DiskStorage::delete_file(bt, &self.file_preview_name);
                    self.refresh_file_manager(bt);
                } else {
                    self.sound.play_error_beep(bt);
                }
            }
            return;
        }

        // Left file list click
        if cx >= wx + 16 && cx <= wx + 310 && cy >= wy + 115 && cy <= wy + 380 {
            let idx = (cy - (wy + 115)) / 28;
            if idx < self.dir_entries.len() {
                self.selected_file_idx = idx;
                let (name, _, is_dir) = self.dir_entries[idx].clone();
                if is_dir {
                    self.current_dir = format!("{}\\{}", self.current_dir, name);
                    self.refresh_file_manager(bt);
                } else {
                    self.file_preview_name = format!("{}\\{}", self.current_dir, name);
                    self.refresh_file_manager(bt);
                }
            }
            return;
        }

        // Bottom file editor inputs
        if cy >= wy + 400 && cy <= wy + 432 {
            self.focused_input = FocusedInput::FileMgrNewName;
        } else if cy >= wy + 438 && cy <= wy + 472 {
            self.focused_input = FocusedInput::FileMgrNewContent;
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn click_ai_agent_window(
        &mut self,
        bt: &BootServices,
        mem_report: &SystemMemoryReport,
        screen_w: usize,
        screen_h: usize,
        wx: usize,
        wy: usize,
        _ww: usize,
        _wh: usize,
        cx: usize,
        cy: usize,
    ) {
        // Top config bar clicks
        if cy >= wy + 44 && cy <= wy + 74 {
            if cx >= wx + 16 && cx <= wx + 135 {
                self.agent.protocol = if self.agent.protocol == "HTTPS" {
                    String::from("HTTP")
                } else {
                    String::from("HTTPS")
                };
            } else if cx >= wx + 145 && cx <= wx + 340 {
                self.focused_input = FocusedInput::AiModel;
            } else if cx >= wx + 350 && cx <= wx + 590 {
                self.focused_input = FocusedInput::AiEndpoint;
            } else if cx >= wx + 600 && cx <= wx + 760 {
                self.show_mcp_prompt_modal = !self.show_mcp_prompt_modal;
            }
            return;
        }

        // AI Permission Level pills (L0, L1, L2, L3)
        if cy >= wy + 82 && cy <= wy + 112 {
            if cx >= wx + 150 && cx <= wx + 690 {
                let idx = (cx - (wx + 150)) / 135;
                self.ai_perm = AiPermissionLevel::from_u8(idx as u8);
                self.save_current_settings_to_disk(bt);
            }
            return;
        }

        // Prompt input box & [Run Autonomous Loop] / [Test HTTP/HTTPS] buttons
        if cy >= wy + 122 && cy <= wy + 156 {
            if cx >= wx + 16 && cx <= wx + 470 {
                self.focused_input = FocusedInput::AiPrompt;
            } else if cx >= wx + 478 && cx <= wx + 625 {
                let goal = self.ai_prompt_input.clone();
                self.agent.start_goal(&goal);
            } else if cx >= wx + 632 && cx <= wx + 765 {
                let url = self.agent.endpoint.clone();
                let res = self.net.http_request(
                    bt,
                    "POST",
                    &url,
                    "{\"model\":\"deepseek-r1:8b\",\"stream\":false}",
                );
                self.agent.logs.push(format!(
                    "[{} {}] {} ms via {}",
                    res.protocol, res.status_code, res.latency_ms, self.net.nic_model
                ));
            }
            return;
        }

        // Direct one-click MCP Tool triggers (10 built-in tools)
        if cy >= wy + 190 && cy <= wy + 345 && cx >= wx + 16 && cx <= wx + 350 {
            let row = (cy - (wy + 190)) / 30;
            let col = (cx - (wx + 16)) / 168;
            let tool_idx = row * 2 + col;
            if tool_idx < MCP_TOOLS.len() {
                let spec = &MCP_TOOLS[tool_idx];
                let out = self.execute_mcp_tool(
                    bt,
                    mem_report,
                    screen_w,
                    screen_h,
                    spec.id,
                    spec.sample_arg,
                );
                self.agent
                    .logs
                    .push(format!("[Manual MCP] {} -> {}", spec.name, out));
                while self.agent.logs.len() > 10 {
                    self.agent.logs.remove(0);
                }
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn click_terminal_window(
        &mut self,
        bt: &BootServices,
        mem_report: &SystemMemoryReport,
        screen_w: usize,
        screen_h: usize,
        wx: usize,
        wy: usize,
        _ww: usize,
        wh: usize,
        cx: usize,
        cy: usize,
    ) {
        self.focused_input = FocusedInput::TerminalCmd;
        // Quick command buttons bar
        if cy >= wy + 42 && cy <= wy + 70 {
            let cmds = [
                "ip a",
                "ip route",
                "curl https://example.com",
                "ls \\AURAOS",
                "mcp",
                "whoami",
            ];
            let idx = cx.saturating_sub(wx + 16) / 112;
            if idx < cmds.len() {
                let _ = self.execute_terminal_command(bt, mem_report, screen_w, screen_h, cmds[idx]);
            }
            return;
        }
        if cy >= wy + wh.saturating_sub(48) && cx >= wx + 560 {
            let cmd = self.term_input.clone();
            self.term_input.clear();
            let _ = self.execute_terminal_command(bt, mem_report, screen_w, screen_h, &cmd);
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn click_settings_window(
        &mut self,
        bt: &BootServices,
        screen_w: usize,
        screen_h: usize,
        wx: usize,
        wy: usize,
        _ww: usize,
        _wh: usize,
        cx: usize,
        cy: usize,
    ) {
        // 1. Theme selector
        if cy >= wy + 80 && cy <= wy + 116 {
            let idx = cx.saturating_sub(wx + 24) / 168;
            if idx < THEMES.len() {
                self.theme_idx = idx;
                self.save_current_settings_to_disk(bt);
            }
        }
        // 2. Wallpaper selector (PNG / JPG / BMP adaptive scaling)
        if cy >= wy + 150 && cy <= wy + 186 {
            let idx = cx.saturating_sub(wx + 24) / 220;
            if idx < 3 {
                self.switch_wallpaper(bt, idx, screen_w, screen_h);
                self.save_current_settings_to_disk(bt);
            }
        }
        // 3. Language & Sound toggle
        if cy >= wy + 220 && cy <= wy + 256 {
            if cx >= wx + 24 && cx <= wx + 220 {
                self.lang = match self.lang {
                    UiLang::En => UiLang::Zh,
                    UiLang::Zh => UiLang::En,
                };
                self.save_current_settings_to_disk(bt);
            } else if cx >= wx + 240 && cx <= wx + 440 {
                self.sound.enabled = !self.sound.enabled;
                self.sound.play_boot_chime(bt);
            }
        }
        // 4. User Role (Admin vs Standard User) & AI Permission Level
        if cy >= wy + 290 && cy <= wy + 326 {
            if cx >= wx + 24 && cx <= wx + 260 {
                self.auth.switch_role_quick();
            } else if cx >= wx + 280 && cx <= wx + 680 {
                self.ai_perm = self.ai_perm.next();
                self.save_current_settings_to_disk(bt);
            }
        }
        // 5. Time & NTP Server
        if cy >= wy + 360 && cy <= wy + 396 {
            if cx >= wx + 24 && cx <= wx + 180 {
                self.ntp_enabled = !self.ntp_enabled;
            } else if cx >= wx + 190 && cx <= wx + 460 {
                self.focused_input = FocusedInput::SettingsNtpServer;
            } else if cx >= wx + 470 && cx <= wx + 560 {
                self.tz_offset = (self.tz_offset - 1).clamp(-12, 14);
            } else if cx >= wx + 570 && cx <= wx + 660 {
                self.tz_offset = (self.tz_offset + 1).clamp(-12, 14);
            }
            self.save_current_settings_to_disk(bt);
        }
    }

    // ========================================================================
    // AUTONOMOUS AGENT TICK (EXECUTES NEXT STEP AUTOMATICALLY ACROSS FRAMES)
    // ========================================================================

    pub fn poll_autonomous_agent(
        &mut self,
        bt: &BootServices,
        mem_report: &SystemMemoryReport,
        screen_w: usize,
        screen_h: usize,
    ) {
        if let Some((tool_id, resolved_arg)) = self.agent.next_pending_action() {
            let output =
                self.execute_mcp_tool(bt, mem_report, screen_w, screen_h, tool_id, &resolved_arg);
            self.agent.record_step_output(output);
        }
    }

    // ========================================================================
    // RENDERING: WALLPAPER, GRAPHICAL DESKTOP ICONS, WINDOWS, TASKBAR
    // ========================================================================

    pub fn render(
        &self,
        fb: &mut GopFrameBuffer,
        rt: &RuntimeServices,
        mem_report: &SystemMemoryReport,
        cursor: &CursorController,
    ) {
        let theme = &THEMES[self.theme_idx % THEMES.len()];
        let sw = fb.width;
        let sh = fb.height;
        let taskbar_y = sh.saturating_sub(44);

        // 1. Blit adaptively-scaled PNG/JPG/BMP wallpaper to backbuffer
        let copy_len = self.wallpaper_scaled.pixels.len().min(fb.backbuffer.len());
        fb.backbuffer[..copy_len].copy_from_slice(&self.wallpaper_scaled.pixels[..copy_len]);

        // 2. Top subtle status banner for Live CD vs Installed Disk Mode + F12 Mouse Guide
        fb.fill_rect_alpha(140, 10, sw.saturating_sub(156), 28, 0xFFFFFF, 215);
        fb.draw_rect_outline(140, 10, sw.saturating_sub(156), 28, 1, theme.win_border);
        let mode_str = if self.is_installed {
            self.lang
                .pick("DISK BOOT (INSTALLED)", "磁盘启动 (已落盘安装)")
        } else {
            self.lang.pick(
                "LIVE DESKTOP TRIAL (Double-click 'Install OS' to install)",
                "桌面体验模式 (双击“安装系统”图标落盘安装)",
            )
        };
        let hold_str = match cursor.hold_state {
            HoldState::Released => "Released",
            HoldState::LeftHeld => "LEFT-HELD(DRAG)",
            HoldState::RightHeld => "RIGHT-HELD",
        };
        let top_info = format!(
            "{} | RAM: {}MB/{}MB | F12 KbdMouse[{}]: 0=Down 2=Up 1=Left 3=Right 7=LClick 9=RClick 4=Drag({})",
            mode_str,
            mem_report.usable_ram_mb(),
            mem_report.total_ram_mb(),
            if cursor.kbd_mouse_enabled { "ON" } else { "OFF" },
            hold_str
        );
        FontRenderer::draw_text(fb, 150, 16, &top_info, theme.text_primary);

        // 3. Left Desktop Graphical Icons (Real multi-color graphical icons, not text abbreviations!)
        let desktop_icons = [
            (
                AppIconKind::Installer,
                self.lang.pick("Install OS", "安装系统"),
            ),
            (
                AppIconKind::FileManager,
                self.lang.pick("Files", "文件管理"),
            ),
            (
                AppIconKind::AiAgent,
                self.lang.pick("AI Agent", "AI 智能体"),
            ),
            (
                AppIconKind::Terminal,
                self.lang.pick("Terminal", "内核终端"),
            ),
            (
                AppIconKind::Settings,
                self.lang.pick("Settings", "系统设置"),
            ),
            (
                AppIconKind::UserRole,
                self.lang.pick(
                    self.auth.current_role().label_en(),
                    self.auth.current_role().label_zh(),
                ),
            ),
        ];

        for (i, (kind, label)) in desktop_icons.iter().enumerate() {
            let ix = 26usize;
            let iy = 22 + i * 86;
            fb.fill_rounded_rect_aa(ix - 8, iy - 4, 88, 78, 12, 0xFFFFFF, 175);
            fb.stroke_rounded_rect_aa(ix - 8, iy - 4, 88, 78, 12, 1, theme.win_border, 190);
            draw_graphical_app_icon(
                fb.backbuffer_mut(),
                sw,
                sh,
                ix + 12,
                iy,
                *kind,
            );
            FontRenderer::draw_text_centered(fb, ix + 36, iy + 51, label, 0x0F172A);
        }

        // 4. Draw Floating Windows in ascending Z-Order
        let mut sorted_indices: [usize; 5] = [0, 1, 2, 3, 4];
        sorted_indices.sort_by(|&a, &b| self.windows[a].z_order.cmp(&self.windows[b].z_order));

        for &idx in &sorted_indices {
            let win = &self.windows[idx];
            if !win.is_open || win.is_minimized {
                continue;
            }
            self.draw_window_frame(fb, win, theme);
            match win.id {
                WindowId::Installer => self.draw_installer_window(fb, win, theme),
                WindowId::FileManager => self.draw_file_manager_window(fb, win, theme),
                WindowId::AiAgent => self.draw_ai_agent_window(fb, win, theme),
                WindowId::Terminal => self.draw_terminal_window(fb, win, theme),
                WindowId::Settings => self.draw_settings_window(fb, win, theme),
            }
        }

        // 5. Start Menu popup (if open)
        if self.start_menu_open {
            let sm_x = 8usize;
            let sm_y = taskbar_y.saturating_sub(270);
            let sm_w = 230usize;
            let sm_h = 264usize;
            fb.fill_rect(sm_x, sm_y, sm_w, sm_h, theme.win_panel);
            fb.draw_rect_outline(sm_x, sm_y, sm_w, sm_h, 2, theme.accent);
            fb.fill_rect(sm_x, sm_y, sm_w, 32, theme.accent);
            FontRenderer::draw_text(
                fb,
                sm_x + 12,
                sm_y + 8,
                self.lang.pick("AuraOS Applications", "AuraOS 应用菜单"),
                0xFFFFFF,
            );
            let items = [
                self.lang.pick("1. Install AuraOS", "1. 安装 AuraOS 系统"),
                self.lang.pick("2. File Manager", "2. 文件管理器"),
                self.lang.pick("3. AI MCP Agent", "3. AI MCP 智能体"),
                self.lang.pick("4. Kernel Terminal", "4. 内核终端 (ip/curl)"),
                self.lang.pick("5. System Settings", "5. 系统设置与个性化"),
                self.lang.pick("6. Switch Admin/User", "6. 切换管理员/普通用户"),
            ];
            for (r, item) in items.iter().enumerate() {
                let ry = sm_y + 38 + r * 36;
                fb.fill_rect(sm_x + 6, ry, sm_w - 12, 30, theme.win_bg);
                FontRenderer::draw_text(fb, sm_x + 14, ry + 7, item, theme.text_primary);
            }
        }

        // 6. Windows-Style Bottom Taskbar
        fb.fill_rect(0, taskbar_y, sw, 44, theme.taskbar_bg);
        fb.fill_rect(0, taskbar_y, sw, 2, theme.win_border);

        // Start Button [AuraOS]
        fb.fill_rect(8, taskbar_y + 6, 110, 32, theme.accent);
        FontRenderer::draw_text_centered(fb, 63, taskbar_y + 14, "AuraOS", 0xFFFFFF);

        // Taskbar Window Tabs
        let tab_defs = [
            (WindowId::Installer, self.lang.pick("Installer", "安装向导")),
            (WindowId::FileManager, self.lang.pick("Files", "文件管理")),
            (WindowId::AiAgent, self.lang.pick("AI Agent", "AI智能体")),
            (WindowId::Terminal, self.lang.pick("Terminal", "终端")),
            (WindowId::Settings, self.lang.pick("Settings", "设置")),
        ];
        for (i, (wid, label)) in tab_defs.iter().enumerate() {
            let tx = 130 + i * 116;
            let wstate = self.windows.iter().find(|w| w.id == *wid);
            let is_open = wstate.map(|w| w.is_open).unwrap_or(false);
            let is_active = is_open
                && !wstate.map(|w| w.is_minimized).unwrap_or(false)
                && self.active_win == *wid;
            let bg = if is_active {
                theme.accent_soft
            } else if is_open {
                0xFFFFFF
            } else {
                theme.win_bg
            };
            fb.fill_rect(tx, taskbar_y + 6, 110, 32, bg);
            fb.draw_rect_outline(
                tx,
                taskbar_y + 6,
                110,
                32,
                1,
                if is_active {
                    theme.accent
                } else {
                    theme.win_border
                },
            );
            FontRenderer::draw_text_centered(fb, tx + 55, taskbar_y + 14, label, theme.taskbar_text);
        }

        // Right System Tray (KbdMouse indicator, Audio, Role, Clock)
        let tray_x = sw.saturating_sub(335);
        fb.fill_rect(tray_x, taskbar_y + 6, 327, 32, 0xFFFFFF);
        fb.draw_rect_outline(tray_x, taskbar_y + 6, 327, 32, 1, theme.win_border);

        let clock_str = if let Ok(t) = rt.get_time() {
            let adj_h = ((t.hour() as i32 + self.tz_offset).rem_euclid(24)) as u8;
            format!("{:02}:{:02}:{:02}", adj_h, t.minute(), t.second())
        } else {
            String::from("12:00:00")
        };
        let tray_text = format!(
            "F12:{} | Snd:{} | {} | {}",
            if cursor.kbd_mouse_enabled { "ON" } else { "OFF" },
            if self.sound.enabled { "ON" } else { "MUTE" },
            self.auth.current_username(),
            clock_str
        );
        FontRenderer::draw_text(fb, tray_x + 8, taskbar_y + 14, &tray_text, theme.text_primary);

        // 7. Hardware / Keyboard-Emulated Cursor on top of everything
        cursor.draw(fb);
    }

    fn draw_window_frame(&self, fb: &mut GopFrameBuffer, win: &DesktopWindow, theme: &ThemePalette) {
        let is_focused = self.active_win == win.id;
        // Multi-ring smooth SDF Gaussian-like drop shadow
        fb.draw_soft_shadow_aa(win.x, win.y, win.w, win.h, 12, 6, if is_focused { 42 } else { 22 });

        // Smooth 12px SDF anti-aliased window surface
        fb.fill_rounded_rect_aa(win.x, win.y, win.w, win.h, 12, theme.win_bg, 250);

        // Smooth gradient titlebar
        let (tb_top, tb_bot) = if is_focused {
            (0x3B82F6, theme.title_active)
        } else {
            (0xCBD5E1, theme.title_inactive)
        };
        fb.fill_rounded_gradient_aa(win.x, win.y, win.w, 36, 10, tb_top, tb_bot, 255);
        fb.stroke_rounded_rect_aa(win.x, win.y, win.w, win.h, 12, 1, theme.win_border, 220);

        let title = self.lang.pick(win.title_en, win.title_zh);
        FontRenderer::draw_text(fb, win.x + 14, win.y + 9, title, theme.title_text);

        // Smooth anti-aliased circular / rounded pill Window Control Buttons:
        // Minimize [-], Maximize [□] / Restore [❐], Close [×]
        let btn_min_x = win.x + win.w.saturating_sub(96);
        let btn_max_x = win.x + win.w.saturating_sub(64);
        let btn_cls_x = win.x + win.w.saturating_sub(32);

        // [-] Minimize (Smooth anti-aliased pill)
        fb.fill_rounded_rect_aa(btn_min_x, win.y + 6, 26, 24, 8, 0xFFFFFF, 235);
        FontRenderer::draw_text(fb, btn_min_x + 9, win.y + 9, "-", 0x0F172A);

        // [□] Maximize OR [❐] Restore original size when maximized
        fb.fill_rounded_rect_aa(btn_max_x, win.y + 6, 26, 24, 8, 0xFFFFFF, 235);
        if win.is_maximized {
            fb.stroke_rounded_rect_aa(btn_max_x + 8, win.y + 9, 11, 9, 2, 1, 0x2563EB, 255);
            fb.stroke_rounded_rect_aa(btn_max_x + 5, win.y + 12, 11, 9, 2, 2, 0x0F172A, 255);
        } else {
            fb.stroke_rounded_rect_aa(btn_max_x + 6, win.y + 10, 14, 12, 3, 2, 0x0F172A, 255);
        }

        // [×] Close (Smooth anti-aliased coral-red pill)
        fb.fill_rounded_rect_aa(btn_cls_x, win.y + 6, 26, 24, 8, 0xEF4444, 245);
        FontRenderer::draw_text(fb, btn_cls_x + 9, win.y + 9, "x", 0xFFFFFF);
    }

    // ========================================================================
    // WINDOW 1: UBUNTU-STYLE 6-STEP INSTALLER WIZARD
    // ========================================================================

    fn draw_installer_window(
        &self,
        fb: &mut GopFrameBuffer,
        win: &DesktopWindow,
        theme: &ThemePalette,
    ) {
        let wx = win.x + 20;
        let wy = win.y + 46;
        let ww = win.w.saturating_sub(40);

        // Progress Step Pills
        let steps = [
            "0.Ask", "1.Lang", "2.Time", "3.AI", "4.Admin", "5.NIC", "6.Install",
        ];
        for (i, s) in steps.iter().enumerate() {
            let sx = wx + i * 94;
            let active = (self.installer.step as usize) == i;
            fb.fill_rect(
                sx,
                wy,
                88,
                26,
                if active { theme.accent } else { theme.win_panel },
            );
            fb.draw_rect_outline(sx, wy, 88, 26, 1, theme.win_border);
            FontRenderer::draw_text_centered(
                fb,
                sx + 44,
                wy + 5,
                s,
                if active { 0xFFFFFF } else { theme.text_secondary },
            );
        }

        let panel_y = wy + 38;
        let panel_h = win.h.saturating_sub(148);
        fb.fill_rect(wx, panel_y, ww, panel_h, theme.win_panel);
        fb.draw_rect_outline(wx, panel_y, ww, panel_h, 1, theme.win_border);

        match self.installer.step {
            InstallerStep::WelcomeAsk => {
                FontRenderer::draw_text(
                    fb,
                    wx + 24,
                    panel_y + 24,
                    self.lang.pick(
                        "Welcome to AuraOS Live Desktop! Start Disk Installation?",
                        "欢迎体验 AuraOS 桌面！是否现在开始安装系统到磁盘？",
                    ),
                    theme.accent,
                );
                FontRenderer::draw_text(
                    fb,
                    wx + 24,
                    panel_y + 62,
                    self.lang.pick(
                        "- Before installation: Default language is English; you can freely try all apps.",
                        "- 安装前默认英语，您可以先体验桌面应用，随时双击安装图标开始安装。",
                    ),
                    theme.text_primary,
                );
                FontRenderer::draw_text(
                    fb,
                    wx + 24,
                    panel_y + 92,
                    self.lang.pick(
                        "- After installation: Persists all configs to FAT32 & boots directly into AuraOS.",
                        "- 安装后自动写入 FAT32 磁盘并改为磁盘启动，下次开机直接进入系统。",
                    ),
                    theme.text_primary,
                );

                // Two action buttons
                fb.fill_rect(wx + 40, panel_y + 165, 250, 42, theme.accent);
                FontRenderer::draw_text_centered(
                    fb,
                    wx + 165,
                    panel_y + 178,
                    self.lang.pick("Start Installation ->", "开始正式安装 ->"),
                    0xFFFFFF,
                );
                fb.fill_rect(wx + 310, panel_y + 165, 250, 42, theme.accent_soft);
                fb.draw_rect_outline(wx + 310, panel_y + 165, 250, 42, 1, theme.accent);
                FontRenderer::draw_text_centered(
                    fb,
                    wx + 435,
                    panel_y + 178,
                    self.lang.pick("Try Live Desktop First", "先体验桌面系统"),
                    theme.accent,
                );
            }
            InstallerStep::LanguageSelect => {
                FontRenderer::draw_text(
                    fb,
                    wx + 24,
                    panel_y + 24,
                    "Step 1/6: Select Post-Install System Language (选择安装后系统语言):",
                    theme.accent,
                );
                let en_sel = matches!(self.installer.lang, UiLang::En);
                fb.fill_rect(
                    wx + 40,
                    panel_y + 70,
                    220,
                    44,
                    if en_sel { theme.accent } else { theme.win_bg },
                );
                FontRenderer::draw_text_centered(
                    fb,
                    wx + 150,
                    panel_y + 84,
                    "English (US)",
                    if en_sel { 0xFFFFFF } else { theme.text_primary },
                );

                fb.fill_rect(
                    wx + 280,
                    panel_y + 70,
                    220,
                    44,
                    if !en_sel { theme.accent } else { theme.win_bg },
                );
                FontRenderer::draw_text_centered(
                    fb,
                    wx + 390,
                    panel_y + 84,
                    "简体中文 (Chinese)",
                    if !en_sel { 0xFFFFFF } else { theme.text_primary },
                );
            }
            InstallerStep::TimeNtpConfig => {
                FontRenderer::draw_text(
                    fb,
                    wx + 24,
                    panel_y + 20,
                    "Step 2/6: Time & NTP Synchronization (时间服务与时区解析，可跳过):",
                    theme.accent,
                );
                fb.fill_rect(
                    wx + 40,
                    panel_y + 52,
                    190,
                    34,
                    if self.installer.ntp_enabled {
                        theme.accent
                    } else {
                        theme.win_bg
                    },
                );
                FontRenderer::draw_text_centered(
                    fb,
                    wx + 135,
                    panel_y + 61,
                    if self.installer.ntp_enabled {
                        "NTP Sync: ENABLED"
                    } else {
                        "NTP Sync: OFF"
                    },
                    if self.installer.ntp_enabled {
                        0xFFFFFF
                    } else {
                        theme.text_primary
                    },
                );

                fb.fill_rect(wx + 250, panel_y + 52, 180, 34, theme.accent_soft);
                FontRenderer::draw_text_centered(
                    fb,
                    wx + 340,
                    panel_y + 61,
                    "Skip Time Setup ->",
                    theme.accent,
                );

                FontRenderer::draw_text(
                    fb,
                    wx + 40,
                    panel_y + 106,
                    "NTP Server Address (Type or Ctrl+V):",
                    theme.text_secondary,
                );
                fb.fill_rect(wx + 40, panel_y + 128, 420, 30, theme.win_bg);
                fb.draw_rect_outline(wx + 40, panel_y + 128, 420, 30, 1, theme.accent);
                FontRenderer::draw_text(
                    fb,
                    wx + 48,
                    panel_y + 135,
                    &self.installer.ntp_server,
                    theme.text_primary,
                );

                let tz_str = format!("Timezone Offset: UTC {:+}", self.installer.tz_offset);
                FontRenderer::draw_text(fb, wx + 40, panel_y + 172, &tz_str, theme.text_primary);
                fb.fill_rect(wx + 220, panel_y + 168, 46, 28, theme.accent_soft);
                FontRenderer::draw_text_centered(fb, wx + 243, panel_y + 174, "[-1]", theme.accent);
                fb.fill_rect(wx + 340, panel_y + 168, 46, 28, theme.accent_soft);
                FontRenderer::draw_text_centered(fb, wx + 363, panel_y + 174, "[+1]", theme.accent);
            }
            InstallerStep::AiConfig => {
                FontRenderer::draw_text(
                    fb,
                    wx + 24,
                    panel_y + 16,
                    "Step 3/6: AI Model & Permission Level (AI 模型与操作权限分层):",
                    theme.accent,
                );
                let proto_lbl = format!("Protocol: [{}] (Click to toggle)", self.installer.ai_protocol);
                FontRenderer::draw_text(fb, wx + 40, panel_y + 48, &proto_lbl, theme.text_primary);

                let fields = [
                    ("Model Name:", &self.installer.ai_model),
                    ("API Endpoint:", &self.installer.ai_endpoint),
                    ("API Key:", &self.installer.ai_api_key),
                ];
                for (i, (k, v)) in fields.iter().enumerate() {
                    let fy = panel_y + 82 + i * 42;
                    let sel = (self.installer.active_field % 3) == i;
                    FontRenderer::draw_text(fb, wx + 40, fy + 6, k, theme.text_secondary);
                    fb.fill_rect(wx + 175, fy, 460, 30, theme.win_bg);
                    fb.draw_rect_outline(
                        wx + 175,
                        fy,
                        460,
                        30,
                        if sel { 2 } else { 1 },
                        if sel { theme.accent } else { theme.win_border },
                    );
                    FontRenderer::draw_text(fb, wx + 183, fy + 6, v, theme.text_primary);
                }

                FontRenderer::draw_text(
                    fb,
                    wx + 40,
                    panel_y + 214,
                    "Select AI Authority Level (用户自由选择 AI 权力大小):",
                    theme.text_secondary,
                );
                let perms = [
                    AiPermissionLevel::L0Disabled,
                    AiPermissionLevel::L1ReadOnly,
                    AiPermissionLevel::L2DesktopControl,
                    AiPermissionLevel::L3FullRoot,
                ];
                for (i, p) in perms.iter().enumerate() {
                    let px = wx + 40 + i * 145;
                    let active = self.installer.ai_perm == *p;
                    fb.fill_rect(
                        px,
                        panel_y + 236,
                        138,
                        30,
                        if active { theme.accent } else { theme.win_bg },
                    );
                    FontRenderer::draw_text_centered(
                        fb,
                        px + 69,
                        panel_y + 243,
                        p.label_en(),
                        if active { 0xFFFFFF } else { theme.text_primary },
                    );
                }
            }
            InstallerStep::AdminRegister => {
                FontRenderer::draw_text(
                    fb,
                    wx + 24,
                    panel_y + 18,
                    "Step 4/6: Register Administrator & Standard User (管理员与普通用户注册):",
                    theme.accent,
                );
                let rows = [
                    ("Admin Username:", &self.installer.admin_user),
                    ("Admin Password/PIN:", &self.installer.admin_pass),
                    ("Standard Username:", &self.installer.std_user),
                    ("Standard Password/PIN:", &self.installer.std_pass),
                ];
                for (i, (k, v)) in rows.iter().enumerate() {
                    let ry = panel_y + 56 + i * 45;
                    let sel = (self.installer.active_field % 4) == i;
                    FontRenderer::draw_text(fb, wx + 40, ry + 6, k, theme.text_secondary);
                    fb.fill_rect(wx + 240, ry, 360, 32, theme.win_bg);
                    fb.draw_rect_outline(
                        wx + 240,
                        ry,
                        360,
                        32,
                        if sel { 2 } else { 1 },
                        if sel { theme.accent } else { theme.win_border },
                    );
                    FontRenderer::draw_text(fb, wx + 248, ry + 7, v, theme.text_primary);
                }
                FontRenderer::draw_text(
                    fb,
                    wx + 40,
                    panel_y + 245,
                    "Note: Passwords are displayed clearly during setup & hashed with PBKDF2-SHA256.",
                    theme.text_secondary,
                );
            }
            InstallerStep::NetworkCheck => {
                FontRenderer::draw_text(
                    fb,
                    wx + 24,
                    panel_y + 20,
                    "Step 5/6: Wired Ethernet Hardware & Internet Check (检测有线网卡接入):",
                    theme.accent,
                );
                let lines = [
                    format!("PCIe Hardware NIC : {}", self.net.nic_model),
                    format!(
                        "Link Status       : {}",
                        if self.net.link_up {
                            "CONNECTED (1000Mb/s Full-Duplex)"
                        } else {
                            "DOWN"
                        }
                    ),
                    format!("Hardware MAC Addr : {}", self.net.mac_string()),
                    format!(
                        "IPv4 / Gateway    : {}/24 via {}",
                        self.net.ip_string(),
                        self.net.gw_string()
                    ),
                    format!(
                        "HTTP/HTTPS Stack  : READY (COM1 0x3F8 TLS Bridge + SimpleNetwork)"
                    ),
                ];
                for (i, l) in lines.iter().enumerate() {
                    FontRenderer::draw_text(
                        fb,
                        wx + 40,
                        panel_y + 62 + i * 30,
                        l,
                        theme.text_primary,
                    );
                }
                fb.fill_rect(wx + 40, panel_y + 225, 220, 34, theme.accent_soft);
                FontRenderer::draw_text_centered(
                    fb,
                    wx + 150,
                    panel_y + 234,
                    "Re-Probe Hardware NIC",
                    theme.accent,
                );
            }
            InstallerStep::ConfirmAndWrite | InstallerStep::Completed => {
                FontRenderer::draw_text(
                    fb,
                    wx + 24,
                    panel_y + 18,
                    "Step 6/6: Confirm Configuration & Write to Disk (确认信息并落盘安装):",
                    theme.accent,
                );
                let summary = [
                    format!(
                        "Language      : {}",
                        self.installer.lang.pick("English", "简体中文")
                    ),
                    format!(
                        "Time / NTP    : {} ({}, UTC{:+})",
                        if self.installer.ntp_enabled {
                            "Enabled"
                        } else {
                            "Skipped"
                        },
                        self.installer.ntp_server,
                        self.installer.tz_offset
                    ),
                    format!(
                        "AI Config     : [{}] {} | Authority: {}",
                        self.installer.ai_protocol,
                        self.installer.ai_model,
                        self.installer.ai_perm.label_en()
                    ),
                    format!(
                        "Accounts      : Admin='{}' | StandardUser='{}'",
                        self.installer.admin_user, self.installer.std_user
                    ),
                    String::from(
                        "Boot Policy   : Write \\AURAOS\\INSTALLED.CFG & switch to Disk Boot",
                    ),
                ];
                for (i, s) in summary.iter().enumerate() {
                    FontRenderer::draw_text(
                        fb,
                        wx + 40,
                        panel_y + 54 + i * 28,
                        s,
                        theme.text_primary,
                    );
                }

                fb.fill_rect(wx + 40, panel_y + 215, 320, 42, 0x16A34A);
                FontRenderer::draw_text_centered(
                    fb,
                    wx + 200,
                    panel_y + 228,
                    "Confirm & Write All Data to Disk",
                    0xFFFFFF,
                );

                if !self.installer.status_msg.is_empty() {
                    FontRenderer::draw_text(
                        fb,
                        wx + 40,
                        panel_y + 268,
                        &self.installer.status_msg,
                        0x15803D,
                    );
                }
            }
        }

        // Bottom Navigation Bar
        let btn_y = win.y + win.h.saturating_sub(54);
        fb.fill_rect(wx + 20, btn_y, 140, 36, theme.win_panel);
        fb.draw_rect_outline(wx + 20, btn_y, 140, 36, 1, theme.win_border);
        FontRenderer::draw_text_centered(
            fb,
            wx + 90,
            btn_y + 10,
            self.lang.pick("< Previous", "< 上一步"),
            theme.text_primary,
        );

        fb.fill_rect(wx + ww.saturating_sub(190), btn_y, 170, 36, theme.accent);
        FontRenderer::draw_text_centered(
            fb,
            wx + ww.saturating_sub(105),
            btn_y + 10,
            if self.installer.step == InstallerStep::ConfirmAndWrite {
                self.lang.pick("Install Now", "立即安装")
            } else {
                self.lang.pick("Next Step >", "下一步 >")
            },
            0xFFFFFF,
        );
    }

    // ========================================================================
    // WINDOW 2: GRAPHICAL FILE MANAGER (PERSISTENT FAT32 + PNG/JPG PREVIEW)
    // ========================================================================

    fn draw_file_manager_window(
        &self,
        fb: &mut GopFrameBuffer,
        win: &DesktopWindow,
        theme: &ThemePalette,
    ) {
        let wx = win.x + 16;
        let wy = win.y + 44;
        let ww = win.w.saturating_sub(32);

        // Top toolbar
        let btns = [
            (0, 108, "\\AURAOS"),
            (116, 176, "\\AURAOS\\WALLPAPERS"),
            (300, 100, self.lang.pick("Refresh", "刷新")),
            (408, 130, self.lang.pick("+ Save File", "+ 保存文件")),
            (546, 120, self.lang.pick("Delete", "删除文件")),
        ];
        for (off_x, bw, label) in btns {
            fb.fill_rect(wx + off_x, wy, bw, 30, theme.win_panel);
            fb.draw_rect_outline(wx + off_x, wy, bw, 30, 1, theme.accent);
            FontRenderer::draw_text_centered(
                fb,
                wx + off_x + bw / 2,
                wy + 7,
                label,
                theme.accent,
            );
        }

        // Current Path & Permission badge
        let hdr = format!(
            "FAT32 Volume Path: {} | Active Role: {}",
            self.current_dir,
            self.auth.current_role().label_en()
        );
        FontRenderer::draw_text(fb, wx, wy + 38, &hdr, theme.text_secondary);

        // Left pane: Directory Listing
        let list_y = wy + 64;
        fb.fill_rect(wx, list_y, 295, 275, theme.win_panel);
        fb.draw_rect_outline(wx, list_y, 295, 275, 1, theme.win_border);

        for (i, (name, sz, is_d)) in self.dir_entries.iter().take(9).enumerate() {
            let ry = list_y + 6 + i * 28;
            let sel = i == self.selected_file_idx;
            if sel {
                fb.fill_rect(wx + 4, ry, 287, 24, theme.accent_soft);
            }
            let row_txt = format!(
                "{} {:<16} {:>6}B",
                if *is_d { "[D]" } else { "[F]" },
                name,
                sz
            );
            FontRenderer::draw_text(fb, wx + 8, ry + 4, &row_txt, theme.text_primary);
        }

        // Right pane: File Content / Decoded PNG-JPG Image Inspector
        let prev_x = wx + 308;
        let prev_w = ww.saturating_sub(308);
        fb.fill_rect(prev_x, list_y, prev_w, 275, theme.win_panel);
        fb.draw_rect_outline(prev_x, list_y, prev_w, 275, 1, theme.win_border);
        FontRenderer::draw_text(
            fb,
            prev_x + 10,
            list_y + 8,
            &format!("Preview: {}", self.file_preview_name),
            theme.accent,
        );
        for (i, line) in self.file_preview_content.lines().take(8).enumerate() {
            let clipped: String = line.chars().take(44).collect();
            FontRenderer::draw_text(
                fb,
                prev_x + 10,
                list_y + 36 + i * 26,
                &clipped,
                theme.text_primary,
            );
        }

        // Bottom persistent file creation inputs
        let fy1 = wy + 350;
        let fy2 = wy + 388;
        FontRenderer::draw_text(fb, wx, fy1 + 6, "File Path:", theme.text_secondary);
        fb.fill_rect(wx + 110, fy1, ww.saturating_sub(110), 30, theme.win_panel);
        fb.draw_rect_outline(
            wx + 110,
            fy1,
            ww.saturating_sub(110),
            30,
            1,
            if self.focused_input == FocusedInput::FileMgrNewName {
                theme.accent
            } else {
                theme.win_border
            },
        );
        FontRenderer::draw_text(fb, wx + 118, fy1 + 7, &self.new_file_name, theme.text_primary);

        FontRenderer::draw_text(fb, wx, fy2 + 6, "Content:", theme.text_secondary);
        fb.fill_rect(wx + 110, fy2, ww.saturating_sub(110), 30, theme.win_panel);
        fb.draw_rect_outline(
            wx + 110,
            fy2,
            ww.saturating_sub(110),
            30,
            1,
            if self.focused_input == FocusedInput::FileMgrNewContent {
                theme.accent
            } else {
                theme.win_border
            },
        );
        FontRenderer::draw_text(
            fb,
            wx + 118,
            fy2 + 7,
            &self.new_file_content,
            theme.text_primary,
        );
    }

    // ========================================================================
    // WINDOW 3: AUTONOMOUS AI MCP AGENT & MODEL CONFIG STUDIO
    // ========================================================================

    fn draw_ai_agent_window(
        &self,
        fb: &mut GopFrameBuffer,
        win: &DesktopWindow,
        theme: &ThemePalette,
    ) {
        let wx = win.x + 16;
        let wy = win.y + 44;
        let ww = win.w.saturating_sub(32);

        // Top Model & Protocol Config Row
        fb.fill_rect(wx, wy, 118, 28, theme.accent);
        FontRenderer::draw_text_centered(
            fb,
            wx + 59,
            wy + 6,
            &format!("Proto: {}", self.agent.protocol),
            0xFFFFFF,
        );

        fb.fill_rect(wx + 126, wy, 195, 28, theme.win_panel);
        fb.draw_rect_outline(wx + 126, wy, 195, 28, 1, theme.win_border);
        FontRenderer::draw_text(
            fb,
            wx + 132,
            wy + 6,
            &format!("Model: {}", self.agent.model),
            theme.text_primary,
        );

        fb.fill_rect(wx + 328, wy, 245, 28, theme.win_panel);
        fb.draw_rect_outline(wx + 328, wy, 245, 28, 1, theme.win_border);
        let ep_short: String = self.agent.endpoint.chars().take(25).collect();
        FontRenderer::draw_text(fb, wx + 334, wy + 6, &ep_short, theme.text_secondary);

        fb.fill_rect(wx + 580, wy, 165, 28, theme.accent_soft);
        fb.draw_rect_outline(wx + 580, wy, 165, 28, 1, theme.accent);
        FontRenderer::draw_text_centered(
            fb,
            wx + 662,
            wy + 6,
            self.lang.pick("System Prompt", "MCP 提示词规范"),
            theme.accent,
        );

        // Row 2: AI Authority Tier Selector (L0 / L1 / L2 / L3)
        FontRenderer::draw_text(
            fb,
            wx,
            wy + 42,
            self.lang.pick("AI Authority:", "AI 权限等级:"),
            theme.text_secondary,
        );
        let levels = [
            AiPermissionLevel::L0Disabled,
            AiPermissionLevel::L1ReadOnly,
            AiPermissionLevel::L2DesktopControl,
            AiPermissionLevel::L3FullRoot,
        ];
        for (i, lvl) in levels.iter().enumerate() {
            let lx = wx + 135 + i * 135;
            let active = self.ai_perm == *lvl;
            fb.fill_rect(
                lx,
                wy + 36,
                128,
                28,
                if active { theme.accent } else { theme.win_panel },
            );
            fb.draw_rect_outline(lx, wy + 36, 128, 28, 1, theme.win_border);
            FontRenderer::draw_text_centered(
                fb,
                lx + 64,
                wy + 42,
                self.lang.pick(lvl.label_en(), lvl.label_zh()),
                if active { 0xFFFFFF } else { theme.text_primary },
            );
        }

        // Row 3: Goal Input + [Run Autonomous Loop] + [Test HTTP/HTTPS]
        let gy = wy + 76;
        fb.fill_rect(wx, gy, 455, 32, theme.win_panel);
        fb.draw_rect_outline(wx, gy, 455, 32, 2, theme.accent);
        let goal_clip: String = self.ai_prompt_input.chars().take(48).collect();
        FontRenderer::draw_text(fb, wx + 8, gy + 8, &goal_clip, theme.text_primary);

        fb.fill_rect(wx + 462, gy, 145, 32, theme.accent);
        FontRenderer::draw_text_centered(
            fb,
            wx + 534,
            gy + 8,
            self.lang.pick("Run Agent Loop", "自主连续执行"),
            0xFFFFFF,
        );

        fb.fill_rect(wx + 614, gy, 132, 32, 0x0284C7);
        FontRenderer::draw_text_centered(
            fb,
            wx + 680,
            gy + 8,
            self.lang.pick("Test HTTPS", "测试 HTTPS"),
            0xFFFFFF,
        );

        if self.show_mcp_prompt_modal {
            // Show the full built-in Agent MCP System Prompt specification
            let py = wy + 118;
            fb.fill_rect(wx, py, ww, 335, theme.win_panel);
            fb.draw_rect_outline(wx, py, ww, 335, 2, theme.accent);
            for (i, line) in AGENT_MCP_SYSTEM_PROMPT.lines().take(12).enumerate() {
                let clipped: String = line.chars().take(78).collect();
                FontRenderer::draw_text(
                    fb,
                    wx + 12,
                    py + 12 + i * 26,
                    &clipped,
                    theme.text_primary,
                );
            }
            return;
        }

        // Left pane: 10 Built-in OS MCP Tools (Click any tool to invoke directly)
        let ty = wy + 120;
        FontRenderer::draw_text(
            fb,
            wx,
            ty,
            self.lang.pick(
                "Built-in OS MCP Tools (Click to test):",
                "系统内置 10 大 MCP 工具接口 (点击调用):",
            ),
            theme.accent,
        );
        for (i, spec) in MCP_TOOLS.iter().enumerate() {
            let r = i / 2;
            let c = i % 2;
            let bx = wx + c * 168;
            let by = ty + 24 + r * 30;
            let allowed = check_mcp_permission(self.ai_perm, self.auth.current_role(), spec.id);
            fb.fill_rect(
                bx,
                by,
                162,
                26,
                if allowed {
                    theme.win_panel
                } else {
                    0xFEE2E2
                },
            );
            fb.draw_rect_outline(bx, by, 162, 26, 1, theme.win_border);
            FontRenderer::draw_text(
                fb,
                bx + 6,
                by + 5,
                spec.name,
                if allowed {
                    theme.text_primary
                } else {
                    0xB91C1C
                },
            );
        }

        // Right pane: Autonomous Multi-Step Execution Pipeline & Logs
        let lx = wx + 345;
        let lw = ww.saturating_sub(345);
        fb.fill_rect(lx, ty, lw, 330, theme.win_panel);
        fb.draw_rect_outline(lx, ty, lw, 330, 1, theme.win_border);
        FontRenderer::draw_text(
            fb,
            lx + 10,
            ty + 8,
            &format!(
                "Autonomous Pipeline (Step {}/{})",
                self.agent.current_step_idx,
                self.agent.steps.len()
            ),
            theme.accent,
        );
        for (i, log) in self.agent.logs.iter().rev().take(10).rev().enumerate() {
            let clipped: String = log.chars().take(44).collect();
            FontRenderer::draw_text(
                fb,
                lx + 10,
                ty + 36 + i * 28,
                &clipped,
                theme.text_primary,
            );
        }
    }

    // ========================================================================
    // WINDOW 4: KERNEL TERMINAL (ip, curl, ls, cat, write, mcp)
    // ========================================================================

    fn draw_terminal_window(
        &self,
        fb: &mut GopFrameBuffer,
        win: &DesktopWindow,
        theme: &ThemePalette,
    ) {
        let wx = win.x + 16;
        let wy = win.y + 42;
        let ww = win.w.saturating_sub(32);

        // Quick One-Click Command Pills
        let pills = [
            "ip a",
            "ip route",
            "curl https://...",
            "ls \\AURAOS",
            "mcp",
            "whoami",
        ];
        for (i, p) in pills.iter().enumerate() {
            let px = wx + i * 112;
            fb.fill_rect(px, wy, 106, 26, theme.accent_soft);
            fb.draw_rect_outline(px, wy, 106, 26, 1, theme.accent);
            FontRenderer::draw_text_centered(fb, px + 53, wy + 5, p, theme.accent);
        }

        // Terminal Output Area (Clean White-Blue readable console)
        let term_y = wy + 34;
        let term_h = win.h.saturating_sub(132);
        fb.fill_rect(wx, term_y, ww, term_h, 0xFFFFFF);
        fb.draw_rect_outline(wx, term_y, ww, term_h, 1, theme.win_border);

        for (i, line) in self.term_logs.iter().take(12).enumerate() {
            let clipped: String = line.chars().take(74).collect();
            FontRenderer::draw_text(
                fb,
                wx + 10,
                term_y + 10 + i * 24,
                &clipped,
                theme.text_primary,
            );
        }

        // Bottom Command Input Box
        let iy = win.y + win.h.saturating_sub(46);
        fb.fill_rect(wx, iy, ww.saturating_sub(140), 32, 0xFFFFFF);
        fb.draw_rect_outline(wx, iy, ww.saturating_sub(140), 32, 2, theme.accent);
        let prompt = format!(
            "{}@aura-os:# {}_",
            self.auth.current_username(),
            self.term_input
        );
        FontRenderer::draw_text(fb, wx + 8, iy + 8, &prompt, theme.text_primary);

        fb.fill_rect(wx + ww.saturating_sub(132), iy, 132, 32, theme.accent);
        FontRenderer::draw_text_centered(
            fb,
            wx + ww.saturating_sub(66),
            iy + 8,
            self.lang.pick("Execute", "执行命令"),
            0xFFFFFF,
        );
    }

    // ========================================================================
    // WINDOW 5: SYSTEM SETTINGS (THEME, ADAPTIVE WALLPAPER, ROLES, TIME)
    // ========================================================================

    fn draw_settings_window(
        &self,
        fb: &mut GopFrameBuffer,
        win: &DesktopWindow,
        theme: &ThemePalette,
    ) {
        let wx = win.x + 24;
        let wy = win.y + 52;

        // 1. Lightweight White-Blue Color Theme Switcher
        FontRenderer::draw_text(
            fb,
            wx,
            wy,
            self.lang.pick(
                "1. Color Palette (White-Blue Lightweight Themes - F3):",
                "1. 轻量化白蓝主题配色切换 (也可按 F3):",
            ),
            theme.accent,
        );
        for (i, th) in THEMES.iter().enumerate() {
            let bx = wx + i * 168;
            let active = self.theme_idx == i;
            fb.fill_rect(
                bx,
                wy + 26,
                158,
                34,
                if active { theme.accent } else { theme.win_panel },
            );
            fb.draw_rect_outline(bx, wy + 26, 158, 34, 1, theme.win_border);
            FontRenderer::draw_text_centered(
                fb,
                bx + 79,
                wy + 35,
                self.lang.pick(th.name_en, th.name_zh),
                if active { 0xFFFFFF } else { theme.text_primary },
            );
        }

        // 2. Adaptive PNG / JPG / BMP Wallpaper Switcher
        FontRenderer::draw_text(
            fb,
            wx,
            wy + 72,
            self.lang.pick(
                "2. Adaptive Wallpaper (PNG / JPG / BMP Bilinear Scaling - F4):",
                "2. 自适应壁纸切换 (支持 PNG / JPG / BMP 分辨率自适应 - F4):",
            ),
            theme.accent,
        );
        let wp_names = [
            "SKY.PNG (Airy Blue)",
            "DAWN.JPG (Porcelain)",
            "MINT.BMP (Daylight)",
        ];
        for (i, wname) in wp_names.iter().enumerate() {
            let bx = wx + i * 220;
            let active = self.wallpaper_idx == i;
            fb.fill_rect(
                bx,
                wy + 98,
                208,
                34,
                if active { theme.accent } else { theme.win_panel },
            );
            fb.draw_rect_outline(bx, wy + 98, 208, 34, 1, theme.win_border);
            FontRenderer::draw_text_centered(
                fb,
                bx + 104,
                wy + 107,
                wname,
                if active { 0xFFFFFF } else { theme.text_primary },
            );
        }

        // 3. Language & Bare-Metal Sound Card
        FontRenderer::draw_text(
            fb,
            wx,
            wy + 144,
            self.lang.pick(
                "3. Language & 8254 PIT Hardware Audio:",
                "3. 系统语言与硬件声卡音效:",
            ),
            theme.accent,
        );
        fb.fill_rect(wx, wy + 168, 195, 34, theme.win_panel);
        fb.draw_rect_outline(wx, wy + 168, 195, 34, 1, theme.accent);
        FontRenderer::draw_text_centered(
            fb,
            wx + 97,
            wy + 177,
            self.lang
                .pick("Lang: English (Switch)", "语言: 简体中文 (切换)"),
            theme.text_primary,
        );

        fb.fill_rect(wx + 215, wy + 168, 220, 34, theme.win_panel);
        fb.draw_rect_outline(wx + 215, wy + 168, 220, 34, 1, theme.accent);
        FontRenderer::draw_text_centered(
            fb,
            wx + 325,
            wy + 177,
            if self.sound.enabled {
                "Sound Card: ON (Test Chime)"
            } else {
                "Sound Card: MUTE"
            },
            theme.text_primary,
        );

        // 4. User Role & AI Permission Hierarchy
        FontRenderer::draw_text(
            fb,
            wx,
            wy + 214,
            self.lang.pick(
                "4. User Permission Tier & AI Authority Tier:",
                "4. 用户权限分层 (管理员/普通用户) 与 AI 权力级别:",
            ),
            theme.accent,
        );
        fb.fill_rect(wx, wy + 238, 235, 34, theme.accent_soft);
        FontRenderer::draw_text_centered(
            fb,
            wx + 117,
            wy + 247,
            &format!(
                "User: {} ({})",
                self.auth.current_username(),
                self.auth.current_role().label_en()
            ),
            theme.accent,
        );

        fb.fill_rect(wx + 255, wy + 238, 390, 34, theme.win_panel);
        fb.draw_rect_outline(wx + 255, wy + 238, 390, 34, 1, theme.accent);
        FontRenderer::draw_text_centered(
            fb,
            wx + 450,
            wy + 247,
            &format!(
                "AI Authority: {} (Click to cycle)",
                self.lang
                    .pick(self.ai_perm.label_en(), self.ai_perm.label_zh())
            ),
            theme.text_primary,
        );

        // 5. NTP Server & Timezone
        FontRenderer::draw_text(
            fb,
            wx,
            wy + 284,
            self.lang.pick(
                "5. Time Synchronization (NTP Server & Timezone):",
                "5. 时间同步服务 (NTP 服务器地址与时区设置):",
            ),
            theme.accent,
        );
        fb.fill_rect(wx, wy + 308, 150, 32, theme.win_panel);
        fb.draw_rect_outline(wx, wy + 308, 150, 32, 1, theme.win_border);
        FontRenderer::draw_text_centered(
            fb,
            wx + 75,
            wy + 316,
            if self.ntp_enabled {
                "NTP: ENABLED"
            } else {
                "NTP: OFF"
            },
            theme.text_primary,
        );

        fb.fill_rect(wx + 165, wy + 308, 270, 32, theme.win_panel);
        fb.draw_rect_outline(wx + 165, wy + 308, 270, 32, 1, theme.accent);
        FontRenderer::draw_text(
            fb,
            wx + 173,
            wy + 316,
            &self.ntp_server,
            theme.text_primary,
        );

        fb.fill_rect(wx + 448, wy + 308, 85, 32, theme.accent_soft);
        FontRenderer::draw_text_centered(fb, wx + 490, wy + 316, "UTC -1", theme.accent);
        fb.fill_rect(wx + 542, wy + 308, 85, 32, theme.accent_soft);
        FontRenderer::draw_text_centered(
            fb,
            wx + 584,
            wy + 316,
            &format!("UTC{:+}", self.tz_offset),
            theme.accent,
        );
    }
}

// ============================================================================
// 7. UEFI BARE-METAL ENTRY POINT (`#[entry]`)
// ============================================================================

#[entry]
fn main(image_handle: Handle, mut system_table: SystemTable<Boot>) -> Status {
    if uefi::helpers::init(&mut system_table).is_err() {
        return Status::ABORTED;
    }

    let bt = system_table.boot_services();
    let _ = bt.set_watchdog_timer(0, 0x10000, None);

    // 1. Initialize UEFI GOP Hardware FrameBuffer
    let mut gop_fb = match GopFrameBuffer::init_from_uefi(bt) {
        Ok(fb) => fb,
        Err(st) => return st,
    };

    // 2. Initialize Accurate Physical Memory Map Report
    let mem_report = SystemMemoryReport::capture_from_boot_services(bt);

    // 3. Ensure Default FAT32 Directory Tree & Check if AuraOS is Already Installed on Disk
    DiskStorage::ensure_default_files(bt);
    let installed_cfg = DiskStorage::load_installed_system(bt);

    // 4. Collect all SimplePointer handles for hardware mouse support alongside F12 Keyboard Mouse
    let pointer_handles: Vec<Handle> = bt
        .locate_handle_buffer(SearchType::ByProtocol(&Pointer::GUID))
        .map(|buf| buf.to_vec())
        .unwrap_or_default();
    for &h in &pointer_handles {
        let _ = bt.connect_controller(h, None, None, true);
        if let Ok(mut ptr) = unsafe {
            bt.open_protocol::<Pointer>(
                OpenProtocolParams {
                    handle: h,
                    agent: image_handle,
                    controller: None,
                },
                OpenProtocolAttributes::GetProtocol,
            )
        } {
            let _ = ptr.reset(false);
        }
    }

    // 5. Build Desktop Environment & Cursor Controller
    let screen_w = gop_fb.width;
    let screen_h = gop_fb.height;
    let mut desktop = AuraOsDesktop::new(bt, screen_w, screen_h, installed_cfg);
    let mut cursor = CursorController::new(screen_w, screen_h);
    let mut tick: u64 = 0;

    // 6. Main Bare-Metal Event, Autonomous Agent & GOP Render Loop
    loop {
        tick = tick.wrapping_add(1);
        cursor.begin_frame();

        // Poll UEFI Keyboard
        let key_opt = system_table.stdin().read_key().ok().flatten();
        if let Some(key) = key_opt {
            let bt_ref = system_table.boot_services();
            desktop.handle_key_event(
                bt_ref,
                &mem_report,
                screen_w,
                screen_h,
                &mut cursor,
                tick,
                key,
            );
        }

        // Poll Hardware SimplePointer handles (if host mouse sends deltas)
        let bt_ref = system_table.boot_services();
        for &h in &pointer_handles {
            if let Ok(mut ptr) = unsafe {
                bt_ref.open_protocol::<Pointer>(
                    OpenProtocolParams {
                        handle: h,
                        agent: image_handle,
                        controller: None,
                    },
                    OpenProtocolAttributes::GetProtocol,
                )
            } {
                if let Ok(Some(state)) = ptr.read_state() {
                    // Only apply hardware mouse delta if within reasonable non-spike bounds
                    let raw_dx = state.relative_movement[0] / 256;
                    let raw_dy = state.relative_movement[1] / 256;
                    if !cursor.kbd_mouse_enabled && raw_dx.abs() <= 48 && raw_dy.abs() <= 48 && (raw_dx != 0 || raw_dy != 0) {
                        cursor.move_by(raw_dx, raw_dy, screen_w, screen_h);
                    }
                    if state.button[0] {
                        if !cursor.prev_left_down {
                            cursor.left_clicked = true;
                            if tick.saturating_sub(cursor.last_click_tick) <= 160 {
                                cursor.double_clicked = true;
                            }
                            cursor.last_click_tick = tick;
                        }
                        cursor.left_down = true;
                    }
                    if state.button[1] {
                        cursor.right_clicked = true;
                        cursor.right_down = true;
                    }
                }
            }
        }

        // Process Pointer / Emulated Cursor Clicks, Window Dragging, Maximize & Restore
        desktop.handle_pointer_interaction(
            bt_ref,
            &mem_report,
            screen_w,
            screen_h,
            &mut cursor,
        );

        // Advance Autonomous Multi-Step AI Agent (1 step every 25 frames so UI stays responsive)
        if tick % 25 == 0 {
            desktop.poll_autonomous_agent(bt_ref, &mem_report, screen_w, screen_h);
        }

        // Render Desktop to Backbuffer and Flush to Hardware GOP VRAM
        let rt_ref = system_table.runtime_services();
        desktop.render(&mut gop_fb, rt_ref, &mem_report, &cursor);
        gop_fb.present();

        // Frame pacing (~120Hz)
        let _ = bt_ref.stall(8_000);
    }
}
