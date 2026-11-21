//! `LedColor`, from `Artemis.Peripherals/Abstractions/Contracts.cs`.
//!
//! A plain 8-bit RGB triple. `Eq` and `Hash` matter: per-key frame building groups keys by
//! identical colour, which is what keeps a full-keyboard frame inside a handful of HID++ packets.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
pub struct LedColor {
    pub r: u8,
    pub g: u8,
    pub b: u8,
}

impl LedColor {
    pub const BLACK: Self = Self { r: 0, g: 0, b: 0 };
    pub const WHITE: Self = Self { r: 255, g: 255, b: 255 };

    pub const fn new(r: u8, g: u8, b: u8) -> Self {
        Self { r, g, b }
    }
}
