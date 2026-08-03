//! core-hash: hash 3 tầng cho dedup (M4 dùng đầy đủ; M1 mới có primitive).
//!
//! INVARIANT AN TOÀN: `quick64` CHỈ dùng để lọc ứng viên — mọi quyết định
//! xóa phải dựa trên `full_blake3` khớp nhau.

use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

use anyhow::Result;

const QUICK_CHUNK: usize = 4096;

/// xxh3(4KB đầu ++ 4KB cuối ++ size) — tầng lọc nhanh, đọc tối đa 8KB.
pub fn quick64(path: &Path) -> Result<i64> {
    let mut f = File::open(path)?;
    let size = f.metadata()?.len();
    let mut buf = Vec::with_capacity(QUICK_CHUNK * 2 + 8);

    let mut head = vec![0u8; QUICK_CHUNK.min(size as usize)];
    f.read_exact(&mut head)?;
    buf.extend_from_slice(&head);

    if size as usize > QUICK_CHUNK {
        let tail_len = QUICK_CHUNK.min(size as usize - QUICK_CHUNK);
        let mut tail = vec![0u8; tail_len];
        f.seek(SeekFrom::End(-(tail_len as i64)))?;
        f.read_exact(&mut tail)?;
        buf.extend_from_slice(&tail);
    }
    buf.extend_from_slice(&size.to_le_bytes());
    Ok(xxhash_rust::xxh3::xxh3_64(&buf) as i64)
}

/// BLAKE3 toàn bộ nội dung — căn cứ duy nhất để kết luận trùng lặp.
pub fn full_blake3(path: &Path) -> Result<[u8; 32]> {
    let mut hasher = blake3::Hasher::new();
    hasher.update_mmap_rayon(path)?;
    Ok(*hasher.finalize().as_bytes())
}
