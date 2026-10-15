//! 假的 GitHub 發佈（放在本機的測試伺服器上，`http.rs`）：影戲下載 yt-dlp、deno 的測試不連到真的 GitHub。
//!
//! 跟 GitHub 一樣的網址：
//! - `/<repo>/releases/latest` 轉到 `/<repo>/releases/tag/<標籤>`
//! - `/<repo>/releases/download/<標籤>/<檔案>`：這個系統要下載的檔案與檢查碼
//!   （yt-dlp 的 `SHA2-256SUMS`；deno 的 `<檔名>.sha256sum`，Windows 版跟真的一樣是 PowerShell 的格式）

use super::http::{Canned, Server};
use vitascope::paths::Os;
use vitascope::ytdl::install::{Tool, ZipSpec, build_zip, sha256_of};

/// 這台電腦要下載的檔名
pub fn asset(tool: Tool) -> &'static str {
    tool.asset(Os::current(), std::env::consts::ARCH)
        .expect("這個系統沒有可以下載的版本")
}

pub fn hex(b: &[u8; 32]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

/// 下載的網址的路徑
pub fn download_path(tool: Tool, tag: &str, file: &str) -> String {
    format!("/{}/releases/download/{tag}/{file}", tool.repo())
}

/// 最新的版本是 `tag`
pub fn set_latest(server: &Server, tool: Tool, tag: &str) {
    let repo = tool.repo();
    server.put(
        &format!("/{repo}/releases/latest"),
        Canned::redirect(&server.url(&format!("/{repo}/releases/tag/{tag}"))),
    );
}

/// 發佈 yt-dlp `tag`：檔案內容 `content`，檢查碼照實際的內容（`sums` 給了就用它）
pub fn publish_ytdl(server: &Server, tag: &str, content: &[u8], sums: Option<String>) {
    let name = asset(Tool::Ytdl);
    let sums = sums.unwrap_or_else(|| {
        // 跟真的一樣列出每個平台的檔案
        format!(
            "{}  yt-dlp\n{}  {name}\n{}  yt-dlp_win.zip\n",
            hex(&sha256_of(b"other")),
            hex(&sha256_of(content)),
            hex(&sha256_of(b"zip"))
        )
    });
    server.put(&download_path(Tool::Ytdl, tag, "SHA2-256SUMS"), Canned::ok(sums));
    server.put(&download_path(Tool::Ytdl, tag, name), Canned::ok(content.to_vec()));
    set_latest(server, Tool::Ytdl, tag);
}

/// 假的 deno 的 zip（裡面有 `deno` / `deno.exe` 與一個授權檔）
pub fn deno_zip(content: &[u8]) -> Vec<u8> {
    let exe = Tool::Deno.file_name(Os::current());
    build_zip(&[
        ZipSpec {
            name: "LICENSE.md",
            data: b"MIT License",
            deflate: true,
            bad_crc: false,
            declared_size: None,
        },
        ZipSpec {
            name: exe,
            data: content,
            deflate: true,
            bad_crc: false,
            declared_size: None,
        },
    ])
}

/// 發佈 deno `tag`（`v2.9.7` 這樣）：zip 與它的 `.sha256sum`
pub fn publish_deno(server: &Server, tag: &str, zip: &[u8]) {
    let name = asset(Tool::Deno);
    let hash = hex(&sha256_of(zip));
    let sums = if cfg!(windows) {
        format!(
            "\r\nAlgorithm       : SHA256\r\nHash            : {}\r\nPath            : D:\\a\\deno\\{name}\r\n\r\n",
            hash.to_uppercase()
        )
    } else {
        format!("{hash}  {name}\n")
    };
    server.put(
        &download_path(Tool::Deno, tag, &format!("{name}.sha256sum")),
        Canned::ok(sums),
    );
    server.put(&download_path(Tool::Deno, tag, name), Canned::ok(zip.to_vec()));
    set_latest(server, Tool::Deno, tag);
}

/// 假的 yt-dlp 的內容：第一行是版本（測試用的尋找讀它當成 `--version` 的結果），後面補到 `size`
pub fn fake_ytdl(version: &str, size: usize) -> Vec<u8> {
    let mut v = format!("{version}\n").into_bytes();
    while v.len() < size {
        v.push(b'#');
    }
    v
}
