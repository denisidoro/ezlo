//! Nerd Font glyphs (https://www.nerdfonts.com/cheat-sheet) shown in front of dropdown rows.
//! Constants are named after their Nerd Font class (`nf-<set>-<name>`); all live in the BMP
//! private-use area so they render as single-width cells.

// Candidate kinds.
pub const CMD: char = '\u{f489}'; // nf-oct-terminal
pub const SUB: char = '\u{ea8c}'; // nf-cod-symbol_method
pub const OPT: char = '\u{f024}'; // nf-fa-flag
pub const ARG: char = '\u{ea88}'; // nf-cod-symbol_variable
pub const RECIPE: char = '\u{f427}'; // nf-oct-rocket
pub const HISTORY: char = '\u{f464}'; // nf-oct-history
pub const TRANSLATE: char = '\u{ec10}'; // nf-cod-sparkle

// Filesystem entries.
pub const DIR: char = '\u{f07b}'; // nf-fa-folder
pub const FILE: char = '\u{f016}'; // nf-fa-file_o
pub const DIR_LINK: char = '\u{eaed}'; // nf-cod-file_symlink_directory
pub const FILE_LINK: char = '\u{eaee}'; // nf-cod-file_symlink_file
pub const EXECUTABLE: char = '\u{f471}'; // nf-oct-file_binary

/// File-type glyph from the (case-insensitive) file name / extension; `None` for unknown types.
pub fn for_file_name(name: &str) -> Option<char> {
    let lower = name.to_ascii_lowercase();
    if let Some(icon) = by_name(&lower) {
        return Some(icon);
    }
    let (_, ext) = lower.rsplit_once('.')?;
    by_extension(ext)
}

fn by_name(lower: &str) -> Option<char> {
    Some(match lower {
        "dockerfile" | "containerfile" => '\u{e650}', // nf-seti-docker
        "makefile" | "justfile" => '\u{e673}',        // nf-seti-makefile
        ".gitignore" | ".gitattributes" | ".gitmodules" | ".gitconfig" => '\u{e65d}', // nf-seti-git
        ".zshrc" | ".bashrc" | ".profile" | ".zshenv" => '\u{e691}', // nf-seti-shell
        _ => return None,
    })
}

fn by_extension(ext: &str) -> Option<char> {
    Some(match ext {
        "rs" => '\u{e7a8}',                                    // nf-dev-rust
        "py" | "pyi" => '\u{e606}',                            // nf-seti-python
        "js" | "mjs" | "cjs" | "jsx" => '\u{e60c}',            // nf-seti-javascript
        "ts" | "tsx" | "mts" => '\u{e628}',                    // nf-seti-typescript
        "go" => '\u{e627}',                                    // nf-seti-go
        "lua" => '\u{e620}',                                   // nf-seti-lua
        "rb" => '\u{e605}',                                    // nf-seti-ruby
        "java" => '\u{e738}',                                  // nf-dev-java
        "kt" | "kts" => '\u{e634}',                            // nf-custom-kotlin
        "swift" => '\u{e699}',                                 // nf-seti-swift
        "c" | "h" => '\u{e649}',                               // nf-seti-c
        "cc" | "cpp" | "cxx" | "hpp" | "hh" => '\u{e646}',     // nf-seti-cpp
        "html" | "htm" => '\u{e60e}',                          // nf-seti-html
        "css" | "scss" | "sass" => '\u{e614}',                 // nf-seti-css
        "sh" | "bash" | "zsh" | "fish" => '\u{e691}',          // nf-seti-shell
        "json" | "jsonc" | "json5" => '\u{e60b}',              // nf-seti-json
        "toml" => '\u{e6b2}',                                  // nf-custom-toml
        "yml" | "yaml" => '\u{e6a8}',                          // nf-seti-yml
        "xml" => '\u{e619}',                                   // nf-seti-xml
        "ini" | "cfg" | "conf" | "env" | "spec" | "ezlo" => '\u{e615}', // nf-seti-config
        "lock" => '\u{e672}',                                  // nf-seti-lock
        "md" | "markdown" | "rst" | "org" => '\u{e609}',       // nf-seti-markdown
        "txt" | "log" => '\u{f15c}',                           // nf-fa-file_text
        "csv" | "tsv" => '\u{e64a}',                           // nf-seti-csv
        "db" | "sqlite" | "sqlite3" | "sql" => '\u{e64d}',     // nf-seti-db
        "pdf" => '\u{f1c1}',                                   // nf-fa-file_pdf
        "svg" => '\u{e698}',                                   // nf-seti-svg
        "png" | "jpg" | "jpeg" | "gif" | "webp" | "heic" | "bmp" | "ico" => '\u{f1c5}', // nf-fa-file_image
        "mp4" | "mkv" | "mov" | "avi" | "webm" => '\u{f1c8}', // nf-fa-file_video
        "mp3" | "flac" | "wav" | "ogg" | "m4a" | "opus" => '\u{f1c7}', // nf-fa-file_audio
        "zip" | "tar" | "gz" | "tgz" | "xz" | "bz2" | "7z" | "zst" | "rar" | "deb" | "rpm"
        | "dmg" => '\u{f1c6}', // nf-fa-file_zipper
        _ => return None,
    })
}
