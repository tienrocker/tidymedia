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

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_file(name: &str, content: &[u8]) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join("tidymedia-test-hash");
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join(format!("{}_{name}", std::process::id()));
        std::fs::write(&p, content).unwrap();
        p
    }

    #[test]
    fn quick64_same_content_same_hash() {
        let a = temp_file("qa.bin", &vec![7u8; 20_000]);
        let b = temp_file("qb.bin", &vec![7u8; 20_000]);
        assert_eq!(quick64(&a).unwrap(), quick64(&b).unwrap());
        std::fs::remove_file(a).ok();
        std::fs::remove_file(b).ok();
    }

    #[test]
    fn quick64_differs_on_edge_change_and_size() {
        let mut base = vec![1u8; 20_000];
        let a = temp_file("qe_a.bin", &base);
        base[0] = 99;
        let b = temp_file("qe_b.bin", &base);
        let c = temp_file("qe_c.bin", &vec![1u8; 20_001]); // khác size
        assert_ne!(quick64(&a).unwrap(), quick64(&b).unwrap());
        assert_ne!(quick64(&a).unwrap(), quick64(&c).unwrap());
        for p in [a, b, c] {
            std::fs::remove_file(p).ok();
        }
    }

    #[test]
    fn quick64_small_and_mid_sizes_ok() {
        // < 4KB và 4-8KB không panic (nhánh tail chồng lấn head)
        let a = temp_file("qs.bin", &[5u8; 100]);
        let b = temp_file("qm.bin", &[5u8; 6000]);
        assert!(quick64(&a).is_ok());
        assert!(quick64(&b).is_ok());
        std::fs::remove_file(a).ok();
        std::fs::remove_file(b).ok();
    }

    #[test]
    fn full_blake3_matches_reference() {
        let p = temp_file("full.bin", b"hello world");
        assert_eq!(
            full_blake3(&p).unwrap(),
            *blake3::hash(b"hello world").as_bytes()
        );
        std::fs::remove_file(p).ok();
    }
}
