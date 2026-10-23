//! 4-Bit (16-Level Grayscale) Anti-Aliased OpenType Font Module (`src/aa_font.rs`)
//! Re-exports anti-aliased ASCII and CJK glyph tables and lookup helpers.

pub use crate::cjk_font::{
    lookup_aa_ascii, lookup_aa_cjk, AA_ASCII_H, AA_ASCII_TABLE, AA_ASCII_W, AA_CJK_TABLE,
};
