//! lvim-fuzzy-ui: the parts of the lvim terminal programs that are not the program — shared by
//! lvim-fuzzy (the finder, `lfz`) and lvim-commander (the file manager, `lvc`).
//!
//! - `theme`, `lvim`, `termcolors`: the lvim-colorscheme themes and the terminal's own colours;
//! - `ansi`: SGR-coloured text (a command's output) into text + style runs;
//! - `canvas`: the cell buffer every frame is drawn into;
//! - `icons`: file icons (Nerd glyphs); `iconimg`: the Material SVG icons as kitty-placeholder images;
//! - `syntax`, `filepreview`: the built-in file preview (syntect colours from the theme);
//! - `nvim`, `pool`: a real Neovim as the preview, and the pool that keeps one warm.

pub mod ansi;
pub mod archive;
pub mod canvas;
pub mod filepreview;
pub mod fsinfo;
pub mod iconimg;
pub mod icons;
pub mod keyboard;
pub mod listview;
pub mod lvim;
pub mod media;
pub mod nvim;
pub mod open;
pub mod pool;
pub mod printas;
pub mod provided;
pub mod syntax;
pub mod input;
pub mod termguard;
pub mod termcolors;
pub mod theme;
pub mod util;
