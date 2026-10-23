//! Bare-Metal Security Engine, User Permission Tiers (`Admin`/`StandardUser`), AI Authority Levels & PBKDF2-SHA256 (`src/security.rs`)

#![allow(dead_code, non_snake_case)]

extern crate alloc;
use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use uefi::table::boot::BootServices;

// ============================================================================
// 1. USER PERMISSION TIERS (ADMIN VS STANDARD USER)
// ============================================================================

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum UserRole {
    Admin,
    StandardUser,
}

impl UserRole {
    pub fn label_en(self) -> &'static str {
        match self {
            UserRole::Admin => "Admin (Root)",
            UserRole::StandardUser => "Standard User",
        }
    }

    pub fn label_zh(self) -> &'static str {
        match self {
            UserRole::Admin => "管理员 (Admin)",
            UserRole::StandardUser => "普通用户 (User)",
        }
    }

    pub fn can_modify_system(self) -> bool {
        matches!(self, UserRole::Admin)
    }
}

// ============================================================================
// 2. AI PERMISSION TIERS (USER-SELECTABLE AI AUTHORITY L0..L3)
// ============================================================================

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
#[repr(u8)]
pub enum AiPermissionLevel {
    L0Disabled = 0,
    L1ReadOnly = 1,
    L2DesktopControl = 2,
    L3FullRoot = 3,
}

impl AiPermissionLevel {
    pub fn from_u8(val: u8) -> Self {
        match val % 4 {
            0 => AiPermissionLevel::L0Disabled,
            1 => AiPermissionLevel::L1ReadOnly,
            2 => AiPermissionLevel::L2DesktopControl,
            _ => AiPermissionLevel::L3FullRoot,
        }
    }

    pub fn next(self) -> Self {
        Self::from_u8((self as u8).wrapping_add(1))
    }

    pub fn label_en(self) -> &'static str {
        match self {
            AiPermissionLevel::L0Disabled => "L0 Disabled",
            AiPermissionLevel::L1ReadOnly => "L1 Read-Only",
            AiPermissionLevel::L2DesktopControl => "L2 Desktop+Files",
            AiPermissionLevel::L3FullRoot => "L3 Full Root",
        }
    }

    pub fn label_zh(self) -> &'static str {
        match self {
            AiPermissionLevel::L0Disabled => "L0 禁用AI工具",
            AiPermissionLevel::L1ReadOnly => "L1 只读巡检",
            AiPermissionLevel::L2DesktopControl => "L2 桌面与文件",
            AiPermissionLevel::L3FullRoot => "L3 最高Root权限",
        }
    }
}

// ============================================================================
// 3. FIPS-180-4 SHA-256, HMAC-SHA256 & PBKDF2-HMAC-SHA256 ENGINE
// ============================================================================

const K256: [u32; 64] = [
    0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4, 0xab1c5ed5,
    0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe, 0x9bdc06a7, 0xc19bf174,
    0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f, 0x4a7484aa, 0x5cb0a9dc, 0x76f988da,
    0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7, 0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967,
    0x27b70a85, 0x2e1b2138, 0x4d2c6dfc, 0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85,
    0xa2bfe8a1, 0xa81a664b, 0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070,
    0x19a4c116, 0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
    0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7, 0xc67178f2,
];

pub fn sha256(data: &[u8]) -> [u8; 32] {
    let mut h: [u32; 8] = [
        0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a,
        0x510e527f, 0x9b05688c, 0x1f83d9ab, 0x5be0cd19,
    ];

    let bit_len = (data.len() as u64).wrapping_mul(8);
    let mut padded = Vec::with_capacity(data.len() + 72);
    padded.extend_from_slice(data);
    padded.push(0x80);
    while (padded.len() % 64) != 56 {
        padded.push(0x00);
    }
    padded.extend_from_slice(&bit_len.to_be_bytes());

    for chunk in padded.chunks_exact(64) {
        let mut w = [0u32; 64];
        for i in 0..16 {
            w[i] = u32::from_be_bytes([
                chunk[i * 4],
                chunk[i * 4 + 1],
                chunk[i * 4 + 2],
                chunk[i * 4 + 3],
            ]);
        }
        for i in 16..64 {
            let s0 = w[i - 15].rotate_right(7) ^ w[i - 15].rotate_right(18) ^ (w[i - 15] >> 3);
            let s1 = w[i - 2].rotate_right(17) ^ w[i - 2].rotate_right(19) ^ (w[i - 2] >> 10);
            w[i] = w[i - 16]
                .wrapping_add(s0)
                .wrapping_add(w[i - 7])
                .wrapping_add(s1);
        }

        let mut a = h[0];
        let mut b = h[1];
        let mut c = h[2];
        let mut d = h[3];
        let mut e = h[4];
        let mut f = h[5];
        let mut g = h[6];
        let mut hh = h[7];

        for i in 0..64 {
            let s1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
            let ch = (e & f) ^ ((!e) & g);
            let temp1 = hh
                .wrapping_add(s1)
                .wrapping_add(ch)
                .wrapping_add(K256[i])
                .wrapping_add(w[i]);
            let s0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
            let maj = (a & b) ^ (a & c) ^ (b & c);
            let temp2 = s0.wrapping_add(maj);

            hh = g;
            g = f;
            f = e;
            e = d.wrapping_add(temp1);
            d = c;
            c = b;
            b = a;
            a = temp1.wrapping_add(temp2);
        }

        h[0] = h[0].wrapping_add(a);
        h[1] = h[1].wrapping_add(b);
        h[2] = h[2].wrapping_add(c);
        h[3] = h[3].wrapping_add(d);
        h[4] = h[4].wrapping_add(e);
        h[5] = h[5].wrapping_add(f);
        h[6] = h[6].wrapping_add(g);
        h[7] = h[7].wrapping_add(hh);
    }

    let mut out = [0u8; 32];
    for (i, word) in h.iter().enumerate() {
        let bytes = word.to_be_bytes();
        out[i * 4..i * 4 + 4].copy_from_slice(&bytes);
    }
    out
}

pub fn hmac_sha256(key: &[u8], message: &[u8]) -> [u8; 32] {
    let mut k_block = [0u8; 64];
    if key.len() > 64 {
        let hashed = sha256(key);
        k_block[..32].copy_from_slice(&hashed);
    } else {
        k_block[..key.len()].copy_from_slice(key);
    }

    let mut o_key_pad = [0x5cu8; 64];
    let mut i_key_pad = [0x36u8; 64];
    for i in 0..64 {
        o_key_pad[i] ^= k_block[i];
        i_key_pad[i] ^= k_block[i];
    }

    let mut inner_input = Vec::with_capacity(64 + message.len());
    inner_input.extend_from_slice(&i_key_pad);
    inner_input.extend_from_slice(message);
    let inner_hash = sha256(&inner_input);

    let mut outer_input = Vec::with_capacity(96);
    outer_input.extend_from_slice(&o_key_pad);
    outer_input.extend_from_slice(&inner_hash);
    sha256(&outer_input)
}

pub struct CryptoEngine;

impl CryptoEngine {
    pub fn DeriveKeyPbkdf2(password: &[u8], salt: &[u8], iterations: u32) -> [u8; 32] {
        let rounds = iterations.max(1);
        let mut salt_block = Vec::with_capacity(salt.len() + 4);
        salt_block.extend_from_slice(salt);
        salt_block.extend_from_slice(&1u32.to_be_bytes());

        let mut u = hmac_sha256(password, &salt_block);
        let mut out = u;

        for _ in 1..rounds {
            u = hmac_sha256(password, &u);
            for j in 0..32 {
                out[j] ^= u[j];
            }
        }
        out
    }
}

// ============================================================================
// 4. USER AUTHENTICATION MANAGER (ADMIN + STANDARD USER)
// ============================================================================

#[derive(Clone)]
pub struct UserAccount {
    pub username: String,
    pub role: UserRole,
    pub salt: [u8; 16],
    pub password_hash: [u8; 32],
}

pub struct UserAuthManager {
    pub users: [UserAccount; 2],
    pub active_idx: usize,
}

impl UserAuthManager {
    pub fn new_default() -> Self {
        let admin_salt = *b"AuraOS_AdminSalt";
        let user_salt = *b"AuraOS_User_Salt";
        let admin_hash = CryptoEngine::DeriveKeyPbkdf2(b"1234", &admin_salt, 600);
        let user_hash = CryptoEngine::DeriveKeyPbkdf2(b"0000", &user_salt, 600);

        Self {
            users: [
                UserAccount {
                    username: String::from("admin"),
                    role: UserRole::Admin,
                    salt: admin_salt,
                    password_hash: admin_hash,
                },
                UserAccount {
                    username: String::from("user"),
                    role: UserRole::StandardUser,
                    salt: user_salt,
                    password_hash: user_hash,
                },
            ],
            active_idx: 0,
        }
    }

    pub fn from_installed(
        admin_username: &str,
        admin_salt: [u8; 16],
        admin_hash: [u8; 32],
        user_username: &str,
        user_salt: [u8; 16],
        user_hash: [u8; 32],
    ) -> Self {
        Self {
            users: [
                UserAccount {
                    username: admin_username.to_string(),
                    role: UserRole::Admin,
                    salt: admin_salt,
                    password_hash: admin_hash,
                },
                UserAccount {
                    username: user_username.to_string(),
                    role: UserRole::StandardUser,
                    salt: user_salt,
                    password_hash: user_hash,
                },
            ],
            active_idx: 0,
        }
    }

    pub fn current_role(&self) -> UserRole {
        self.users[self.active_idx % 2].role
    }

    pub fn current_username(&self) -> &str {
        &self.users[self.active_idx % 2].username
    }

    pub fn switch_role_quick(&mut self) {
        self.active_idx = (self.active_idx + 1) % 2;
    }

    pub fn authenticate(&mut self, username: &str, password: &str) -> bool {
        for (idx, acc) in self.users.iter().enumerate() {
            if acc.username.eq_ignore_ascii_case(username) {
                let candidate =
                    CryptoEngine::DeriveKeyPbkdf2(password.as_bytes(), &acc.salt, 600);
                let mut diff = 0u8;
                for i in 0..32 {
                    diff |= candidate[i] ^ acc.password_hash[i];
                }
                if diff == 0 {
                    self.active_idx = idx;
                    return true;
                }
            }
        }
        false
    }
}

// ============================================================================
// 5. HARDWARE TSC SECURITY AUDIT CHAIN
// ============================================================================

pub struct SecurityAuditChain {
    pub head_hash: [u8; 32],
    pub records: Vec<String>,
}

impl SecurityAuditChain {
    pub fn new() -> Self {
        Self {
            head_hash: [0u8; 32],
            records: Vec::new(),
        }
    }

    pub fn AppendRecord(&mut self, _bt: &BootServices, event: &str) {
        let mut msg = Vec::with_capacity(32 + event.len());
        msg.extend_from_slice(&self.head_hash);
        msg.extend_from_slice(event.as_bytes());
        self.head_hash = sha256(&msg);
        self.records.push(format!(
            "[{:02x}{:02x}{:02x}{:02x}] {}",
            self.head_hash[0], self.head_hash[1], self.head_hash[2], self.head_hash[3], event
        ));
        if self.records.len() > 16 {
            self.records.remove(0);
        }
    }
}
