//! 檔案特徵碼：OpenSubtitles 的 moviehash（線上搜尋字幕用它找「跟這個檔案完全相符」的字幕；之後書籤也用它找回搬家的影片）。
//!
//! 官方的算法：檔案大小，加上開頭 64 KiB 與結尾 64 KiB 每 8 個位元組（little-endian 的 u64）的和，溢位就繞回。
//! 只讀 128 KiB，網路磁碟上也很快；但仍是讀使用者的檔案，介面執行緒不要直接呼叫。
//!
//! 小於 128 KiB 的檔案回傳 None：開頭和結尾的兩段會重疊，各家的實作算法不一致；真的影片不會這麼小。

use std::io::{self, Read, Seek, SeekFrom};
use std::path::Path;

/// 讀開頭、結尾各多少位元組
const CHUNK: u64 = 64 * 1024;
/// 這個大小以上才算特徵碼
pub const MIN_SIZE: u64 = 2 * CHUNK;

/// 檔案的特徵碼
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct FileHash {
    /// 檔案大小（位元組）
    pub size: u64,
    /// moviehash
    pub hash: u64,
}

impl FileHash {
    /// OpenSubtitles 用的寫法：16 個小寫的十六進位數字
    pub fn hex(&self) -> String {
        format!("{:016x}", self.hash)
    }
}

/// 檔案的 moviehash；小於 [`MIN_SIZE`] 時 None
pub fn moviehash(path: &Path) -> io::Result<Option<FileHash>> {
    let mut file = std::fs::File::open(path)?;
    let size = file.metadata()?.len();
    moviehash_of(&mut file, size)
}

/// 從 `reader`（總共 `size` 個位元組）算 moviehash；小於 [`MIN_SIZE`] 時 None
pub fn moviehash_of(reader: &mut (impl Read + Seek), size: u64) -> io::Result<Option<FileHash>> {
    if size < MIN_SIZE {
        return Ok(None);
    }
    let mut hash = size;
    let mut buf = vec![0u8; CHUNK as usize];
    for start in [0, size - CHUNK] {
        reader.seek(SeekFrom::Start(start))?;
        reader.read_exact(&mut buf)?;
        for word in buf.as_chunks::<8>().0 {
            hash = hash.wrapping_add(u64::from_le_bytes(*word));
        }
    }
    Ok(Some(FileHash { size, hash }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    fn hash_of(bytes: &[u8]) -> Option<String> {
        moviehash_of(&mut Cursor::new(bytes), bytes.len() as u64)
            .unwrap()
            .map(|h| h.hex())
    }

    #[test]
    fn matches_the_reference_vectors() {
        // 另外用 Python 照官方算法算出來的值（scratchpad/l3b2/subs_rare_probe）
        let a: Vec<u8> = (0..204_800u32).map(|i| (i % 251) as u8).collect();
        assert_eq!(hash_of(&a).as_deref(), Some("a65c16d28845ff55"));
        // 結尾那一段沒有對齊 8 個位元組（131075 = 128 KiB + 3）
        let mut b: Vec<u8> = (0..512).flat_map(|_| 0..=255u8).collect();
        b.extend_from_slice(&[0xff; 3]);
        assert_eq!(b.len(), 131_075);
        assert_eq!(hash_of(&b).as_deref(), Some("fdbf7f3fffc16003"));
    }

    #[test]
    fn small_files_have_no_hash() {
        assert_eq!(hash_of(&vec![7u8; 100 * 1024]), None);
        assert_eq!(hash_of(&vec![7u8; MIN_SIZE as usize - 1]), None);
        assert!(hash_of(&vec![7u8; MIN_SIZE as usize]).is_some());
        assert_eq!(hash_of(&[]), None);
    }

    #[test]
    fn reads_a_real_file() {
        let path = std::env::temp_dir().join(format!("vitascope-filehash-{}.bin", std::process::id()));
        let a: Vec<u8> = (0..204_800u32).map(|i| (i % 251) as u8).collect();
        std::fs::write(&path, &a).unwrap();
        let h = moviehash(&path).unwrap().unwrap();
        assert_eq!((h.size, h.hex().as_str()), (204_800, "a65c16d28845ff55"));
        std::fs::remove_file(&path).unwrap();
        assert!(moviehash(&path).is_err(), "檔案不存在是錯誤，不是 None");
    }
}
