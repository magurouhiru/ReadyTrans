//! 画面の範囲と画像（Windows 以外でもテストできるよう、OS に依存しない部分）。

/// メイン画面上の範囲（物理ピクセル）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Region {
    pub x: i32,
    pub y: i32,
    pub w: u32,
    pub h: u32,
}

/// 1ピクセル4バイト（青・緑・赤・不透明度）の画像。
#[derive(Debug, Clone)]
pub struct Bgra {
    pub w: u32,
    pub h: u32,
    pub data: Vec<u8>,
}
