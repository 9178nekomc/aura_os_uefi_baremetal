//! Bare-Metal UEFI `SimpleFileSystem` Persistent FAT32 Storage & Installer Config Manager (`src/storage.rs`)
//!
//! Persists all files, directories, and OS installation state (`\AURAOS\INSTALLED.CFG` and `\AURAOS\BOOT.CFG`)
//! directly onto the FAT32 disk partition via UEFI `SimpleFileSystem`.

#![allow(dead_code)]

extern crate alloc;
use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec;
use alloc::vec::Vec;
use uefi::proto::media::file::{
    Directory, File, FileAttribute, FileInfo, FileMode, FileType,
};
use uefi::proto::media::fs::SimpleFileSystem;
use uefi::table::boot::{BootServices, OpenProtocolAttributes, OpenProtocolParams};
use uefi::CStr16;

#[repr(align(8))]
struct AlignedFileInfoBuf([u8; 512]);

#[derive(Clone, Debug)]
pub struct InstalledSystemConfig {
    pub installed: bool,
    pub boot_from_disk: bool,
    pub language_zh: bool,
    pub ntp_enabled: bool,
    pub ntp_server: String,
    pub timezone_offset: i32,
    pub ai_protocol: String,
    pub ai_model: String,
    pub ai_endpoint: String,
    pub ai_api_key: String,
    pub ai_perm_level: u8,
    pub admin_username: String,
    pub admin_salt: [u8; 16],
    pub admin_hash: [u8; 32],
    pub user_username: String,
    pub user_salt: [u8; 16],
    pub user_hash: [u8; 32],
    pub theme_index: u8,
    pub wallpaper_index: u8,
}

fn bytes_to_hex(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut s = String::with_capacity(bytes.len() * 2);
    for &b in bytes {
        s.push(HEX[(b >> 4) as usize] as char);
        s.push(HEX[(b & 0x0F) as usize] as char);
    }
    s
}

fn hex_to_bytes<const N: usize>(hex: &str, default_fill: u8) -> [u8; N] {
    let mut out = [default_fill; N];
    let raw = hex.trim().as_bytes();
    for i in 0..N {
        if i * 2 + 1 < raw.len() {
            let hi = match raw[i * 2] {
                b'0'..=b'9' => raw[i * 2] - b'0',
                b'a'..=b'f' => raw[i * 2] - b'a' + 10,
                b'A'..=b'F' => raw[i * 2] - b'A' + 10,
                _ => 0,
            };
            let lo = match raw[i * 2 + 1] {
                b'0'..=b'9' => raw[i * 2 + 1] - b'0',
                b'a'..=b'f' => raw[i * 2 + 1] - b'a' + 10,
                b'A'..=b'F' => raw[i * 2 + 1] - b'A' + 10,
                _ => 0,
            };
            out[i] = (hi << 4) | lo;
        }
    }
    out
}

impl InstalledSystemConfig {
    pub fn to_config_text(&self) -> String {
        format!(
            "INSTALLED={}\nBOOT_FROM_DISK={}\nLANG_ZH={}\nNTP_ENABLED={}\nNTP_SERVER={}\nTZ_OFFSET={}\nAI_PROTO={}\nAI_MODEL={}\nAI_ENDPOINT={}\nAI_KEY={}\nAI_PERM={}\nADMIN_USER={}\nADMIN_SALT={}\nADMIN_HASH={}\nSTD_USER={}\nSTD_SALT={}\nSTD_HASH={}\nTHEME={}\nWALLPAPER={}\n",
            if self.installed { "1" } else { "0" },
            if self.boot_from_disk { "1" } else { "0" },
            if self.language_zh { "1" } else { "0" },
            if self.ntp_enabled { "1" } else { "0" },
            self.ntp_server,
            self.timezone_offset,
            self.ai_protocol,
            self.ai_model,
            self.ai_endpoint,
            self.ai_api_key,
            self.ai_perm_level,
            self.admin_username,
            bytes_to_hex(&self.admin_salt),
            bytes_to_hex(&self.admin_hash),
            self.user_username,
            bytes_to_hex(&self.user_salt),
            bytes_to_hex(&self.user_hash),
            self.theme_index,
            self.wallpaper_index
        )
    }

    pub fn from_config_text(text: &str) -> Option<Self> {
        let mut cfg = Self {
            installed: false,
            boot_from_disk: true,
            language_zh: false,
            ntp_enabled: true,
            ntp_server: String::from("pool.ntp.org"),
            timezone_offset: 8,
            ai_protocol: String::from("HTTPS"),
            ai_model: String::from("deepseek-r1:8b"),
            ai_endpoint: String::from("https://api.openai.com/v1/chat/completions"),
            ai_api_key: String::from("sk-aura-live-key"),
            ai_perm_level: 2,
            admin_username: String::from("admin"),
            admin_salt: *b"AuraOS_AdminSalt",
            admin_hash: [0u8; 32],
            user_username: String::from("user"),
            user_salt: *b"AuraOS_User_Salt",
            user_hash: [0u8; 32],
            theme_index: 0,
            wallpaper_index: 0,
        };

        for line in text.lines() {
            let line = line.trim();
            if let Some(pos) = line.find('=') {
                let k = line[..pos].trim();
                let v = line[pos + 1..].trim();
                match k {
                    "INSTALLED" => cfg.installed = v == "1" || v.eq_ignore_ascii_case("true"),
                    "BOOT_FROM_DISK" => {
                        cfg.boot_from_disk = v == "1" || v.eq_ignore_ascii_case("true")
                    }
                    "LANG_ZH" => cfg.language_zh = v == "1" || v.eq_ignore_ascii_case("true"),
                    "NTP_ENABLED" => cfg.ntp_enabled = v == "1" || v.eq_ignore_ascii_case("true"),
                    "NTP_SERVER" => cfg.ntp_server = v.to_string(),
                    "TZ_OFFSET" => cfg.timezone_offset = v.parse::<i32>().unwrap_or(8),
                    "AI_PROTO" => cfg.ai_protocol = v.to_string(),
                    "AI_MODEL" => cfg.ai_model = v.to_string(),
                    "AI_ENDPOINT" => cfg.ai_endpoint = v.to_string(),
                    "AI_KEY" => cfg.ai_api_key = v.to_string(),
                    "AI_PERM" => cfg.ai_perm_level = v.parse::<u8>().unwrap_or(2),
                    "ADMIN_USER" => cfg.admin_username = v.to_string(),
                    "ADMIN_SALT" => cfg.admin_salt = hex_to_bytes::<16>(v, b'A'),
                    "ADMIN_HASH" => cfg.admin_hash = hex_to_bytes::<32>(v, 0),
                    "STD_USER" => cfg.user_username = v.to_string(),
                    "STD_SALT" => cfg.user_salt = hex_to_bytes::<16>(v, b'U'),
                    "STD_HASH" => cfg.user_hash = hex_to_bytes::<32>(v, 0),
                    "THEME" => cfg.theme_index = v.parse::<u8>().unwrap_or(0),
                    "WALLPAPER" => cfg.wallpaper_index = v.parse::<u8>().unwrap_or(0),
                    _ => {}
                }
            }
        }

        if cfg.installed {
            Some(cfg)
        } else {
            None
        }
    }
}

fn to_ucs2_buf<'a>(path: &str, buf: &'a mut [u16; 256]) -> Option<&'a CStr16> {
    let mut idx = 0usize;
    for ch in path.chars() {
        if idx + 1 >= buf.len() {
            break;
        }
        let mapped = if ch == '/' { '\\' } else { ch };
        buf[idx] = mapped as u16;
        idx += 1;
    }
    buf[idx] = 0;
    CStr16::from_u16_with_nul(&buf[..=idx]).ok()
}

fn open_root_volume(bt: &BootServices) -> Option<Directory> {
    let fs_handle = bt.get_handle_for_protocol::<SimpleFileSystem>().ok()?;
    let mut fs = unsafe {
        bt.open_protocol::<SimpleFileSystem>(
            OpenProtocolParams {
                handle: fs_handle,
                agent: bt.image_handle(),
                controller: None,
            },
            OpenProtocolAttributes::GetProtocol,
        )
        .ok()?
    };
    fs.open_volume().ok()
}

pub struct DiskStorage;

impl DiskStorage {
    pub fn mkdir(bt: &BootServices, path: &str) -> bool {
        let mut root = match open_root_volume(bt) {
            Some(r) => r,
            None => return false,
        };
        let mut ucs2 = [0u16; 256];
        let cpath = match to_ucs2_buf(path, &mut ucs2) {
            Some(p) => p,
            None => return false,
        };
        root.open(
            cpath,
            FileMode::CreateReadWrite,
            FileAttribute::DIRECTORY,
        )
        .is_ok()
    }

    pub fn write_file(bt: &BootServices, path: &str, data: &[u8]) -> bool {
        let mut root = match open_root_volume(bt) {
            Some(r) => r,
            None => return false,
        };
        let mut ucs2 = [0u16; 256];
        let cpath = match to_ucs2_buf(path, &mut ucs2) {
            Some(p) => p,
            None => return false,
        };

        // Delete existing file first so truncation is guaranteed on FAT32
        if let Ok(existing) = root.open(cpath, FileMode::ReadWrite, FileAttribute::empty()) {
            let _ = existing.delete();
        }

        let handle = match root.open(cpath, FileMode::CreateReadWrite, FileAttribute::empty()) {
            Ok(h) => h,
            Err(_) => return false,
        };

        if let Ok(FileType::Regular(mut reg)) = handle.into_type() {
            if reg.write(data).is_ok() {
                let _ = reg.flush();
                return true;
            }
        }
        false
    }

    pub fn read_file(bt: &BootServices, path: &str) -> Option<Vec<u8>> {
        let mut root = open_root_volume(bt)?;
        let mut ucs2 = [0u16; 256];
        let cpath = to_ucs2_buf(path, &mut ucs2)?;

        let handle = root.open(cpath, FileMode::Read, FileAttribute::empty()).ok()?;
        if let Ok(FileType::Regular(mut reg)) = handle.into_type() {
            let mut info_buf = AlignedFileInfoBuf([0u8; 512]);
            let file_size = if let Ok(info) = reg.get_info::<FileInfo>(&mut info_buf.0) {
                (info.file_size() as usize).min(2 * 1024 * 1024)
            } else {
                65536
            };
            let mut out = vec![0u8; file_size];
            if let Ok(n) = reg.read(&mut out) {
                out.truncate(n);
                return Some(out);
            }
        }
        None
    }

    pub fn delete_file(bt: &BootServices, path: &str) -> bool {
        let mut root = match open_root_volume(bt) {
            Some(r) => r,
            None => return false,
        };
        let mut ucs2 = [0u16; 256];
        let cpath = match to_ucs2_buf(path, &mut ucs2) {
            Some(p) => p,
            None => return false,
        };
        if let Ok(handle) = root.open(cpath, FileMode::ReadWrite, FileAttribute::empty()) {
            return handle.delete().is_ok();
        }
        false
    }

    /// Lists entries in `dir_path` as `(filename, size_bytes, is_dir)`
    pub fn list_dir(bt: &BootServices, dir_path: &str) -> Vec<(String, u64, bool)> {
        let mut entries = Vec::new();
        let mut root = match open_root_volume(bt) {
            Some(r) => r,
            None => return entries,
        };

        let mut dir_handle: Directory = if dir_path == "\\" || dir_path.is_empty() {
            root
        } else {
            let mut ucs2 = [0u16; 256];
            let cpath = match to_ucs2_buf(dir_path, &mut ucs2) {
                Some(p) => p,
                None => return entries,
            };
            match root.open(cpath, FileMode::Read, FileAttribute::empty()) {
                Ok(h) => match h.into_type() {
                    Ok(FileType::Dir(d)) => d,
                    _ => return entries,
                },
                Err(_) => return entries,
            }
        };

        let _ = dir_handle.reset_entry_readout();
        let mut info_buf = AlignedFileInfoBuf([0u8; 512]);
        while let Ok(Some(info)) = dir_handle.read_entry(&mut info_buf.0) {
            let name = info.file_name().to_string();
            if name == "." || name == ".." {
                continue;
            }
            let is_dir = info.is_directory();
            let size = info.file_size();
            entries.push((name, size, is_dir));
        }
        entries
    }

    /// Creates default `\AURAOS` directory tree & sample files if not yet present on FAT32
    pub fn ensure_default_files(bt: &BootServices) {
        let _ = Self::mkdir(bt, "\\AURAOS");
        let _ = Self::mkdir(bt, "\\AURAOS\\WALLPAPERS");
        let _ = Self::mkdir(bt, "\\AURAOS\\DOCS");

        if Self::read_file(bt, "\\AURAOS\\README.TXT").is_none() {
            let readme = b"AuraOS v2.0 Bare-Metal UEFI Operating System\n- Persistent FAT32 Storage via SimpleFileSystem\n- Real PCIe NIC + HTTP/HTTPS Stack\n- 10 Built-in OS MCP Tools for Autonomous AI Agent\n- F12 Keyboard Mouse Emulation (0=Down,2=Up,1=Left,3=Right,7=LClick,9=RClick,4=Drag)\n";
            let _ = Self::write_file(bt, "\\AURAOS\\README.TXT", readme);
        }
    }

    pub fn load_installed_system(bt: &BootServices) -> Option<InstalledSystemConfig> {
        let bytes = Self::read_file(bt, "\\AURAOS\\INSTALLED.CFG")?;
        let text = core::str::from_utf8(&bytes).ok()?;
        InstalledSystemConfig::from_config_text(text)
    }

    pub fn save_installed_system(bt: &BootServices, cfg: &InstalledSystemConfig) -> bool {
        let _ = Self::mkdir(bt, "\\AURAOS");
        let text = cfg.to_config_text();
        let ok_cfg = Self::write_file(bt, "\\AURAOS\\INSTALLED.CFG", text.as_bytes());
        let boot_cfg = b"BOOT_TARGET=DISK_INSTALLED\nSKIP_LIVE_INSTALLER=1\n";
        let _ = Self::write_file(bt, "\\AURAOS\\BOOT.CFG", boot_cfg);
        ok_cfg
    }
}
