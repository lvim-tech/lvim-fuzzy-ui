# lvim-fuzzy-ui

The parts of the lvim terminal programs that are not the program itself, shared by
[lvim-fuzzy](https://github.com/lvim-tech/lvim-fuzzy-cli) (the finder, `lfz`) and
[lvim-fuzzy-commander](https://github.com/lvim-tech/lvim-fuzzy-commander) (the file manager, `lvc`):

- `theme`, `lvim`, `termcolors` — the lvim-colorscheme themes (`~/.config/lvim-fuzzy/themes`) and the
  terminal's own colours, resolved into the lvim look;
- `canvas` — the cell buffer a frame is drawn into, written as the diff against the previous one;
- `icons` — file icons;
- `syntax`, `filepreview` — the built-in file preview (syntect, coloured from the theme's palette);
- `nvim`, `pool` — a real Neovim as the preview, and `lvim-fuzzy-cli pool`, which keeps one warm;
- `util` — shell word splitting.

A library crate (no binary); the programs depend on it by path.

## License

BSD-3-Clause
