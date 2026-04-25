//! Minimal SCO BFS (Boot File System) image writer.
//!
//! BFS is a flat filesystem (no subdirectories, no indirect blocks): each
//! file occupies a contiguous run of 512-byte blocks. The on-disk layout is:
//!
//! ```text
//! block 0:        superblock
//! blocks 1..=63:  inode table (8 inodes per block, inodes 2..=505)
//! blocks 64..:    file data + root directory entries
//! ```
//!
//! Reference: Linux kernel `fs/bfs/` and SCO documentation.

use std::fs::{self, File};
use std::io::{Read, Write};
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

const BSIZE: usize = 512;
const MAGIC: u32 = 0x1bad_face;
const ROOT_INO: u32 = 2;
const LAST_INO: u32 = 505;
const INODES_PER_BLOCK: u32 = 8;
const INODE_SIZE: usize = 64;
const DIRENT_SIZE: usize = 16;
const NAMELEN: usize = 14;

const VREG: u32 = 1;
const VDIR: u32 = 2;

/// `S_IFREG` from Linux `<sys/stat.h>` — encoded into the BFS inode mode field.
const S_IFREG: u32 = 0o100_000;
/// `S_IFDIR` from Linux `<sys/stat.h>`.
const S_IFDIR: u32 = 0o040_000;

/// Stats reported after writing.
pub struct Stats {
    pub total_blocks: u32,
    pub entries: u32,
}

#[derive(Clone, Copy)]
struct Inode {
    ino: u16,
    sblock: u32,
    eblock: u32,
    eoffset: u32,
    vtype: u32,
    mode: u32,
    uid: u32,
    gid: u32,
    nlink: u32,
    atime: u32,
    mtime: u32,
    ctime: u32,
}

impl Inode {
    fn new(ino: u32, vtype: u32, mode: u32, now: u32) -> Self {
        Self {
            ino: ino as u16,
            sblock: 0,
            eblock: 0,
            eoffset: u32::MAX,
            vtype,
            mode,
            uid: 0,
            gid: 0,
            nlink: 1,
            atime: now,
            mtime: now,
            ctime: now,
        }
    }

    fn write_into(&self, buf: &mut [u8]) {
        buf[0..2].copy_from_slice(&self.ino.to_le_bytes());
        // bytes 2..4: i_unused
        buf[4..8].copy_from_slice(&self.sblock.to_le_bytes());
        buf[8..12].copy_from_slice(&self.eblock.to_le_bytes());
        buf[12..16].copy_from_slice(&self.eoffset.to_le_bytes());
        buf[16..20].copy_from_slice(&self.vtype.to_le_bytes());
        buf[20..24].copy_from_slice(&self.mode.to_le_bytes());
        buf[24..28].copy_from_slice(&self.uid.to_le_bytes());
        buf[28..32].copy_from_slice(&self.gid.to_le_bytes());
        buf[32..36].copy_from_slice(&self.nlink.to_le_bytes());
        buf[36..40].copy_from_slice(&self.atime.to_le_bytes());
        buf[40..44].copy_from_slice(&self.mtime.to_le_bytes());
        buf[44..48].copy_from_slice(&self.ctime.to_le_bytes());
        // bytes 48..64: i_padding
    }
}

#[derive(Clone, Copy)]
struct Dirent {
    ino: u16,
    name: [u8; NAMELEN],
}

impl Dirent {
    fn new(ino: u16, name: &str) -> Self {
        let mut n = [0u8; NAMELEN];
        let bytes = name.as_bytes();
        let len = bytes.len().min(NAMELEN);
        n[..len].copy_from_slice(&bytes[..len]);
        Self { ino, name: n }
    }

    fn write_into(&self, buf: &mut [u8]) {
        buf[0..2].copy_from_slice(&self.ino.to_le_bytes());
        buf[2..16].copy_from_slice(&self.name);
    }

    /// Match the C version's collision rule: equal first `min(NAMELEN, len)`
    /// bytes, and either both are 14 bytes (no null required) or the existing
    /// entry is null-terminated at `len`.
    fn matches_name(&self, name: &str) -> bool {
        let bytes = name.as_bytes();
        let len = bytes.len().min(NAMELEN);
        if self.name[..len] != bytes[..len] {
            return false;
        }
        len == NAMELEN || self.name[len] == 0
    }
}

pub struct Bfs {
    image: Vec<u8>,
    total_blocks: u32,
    next_data_block: u32,
    next_inode: u32,
    /// Inode slots indexed by `ino - ROOT_INO`. `None` means "never used".
    inodes: Vec<Option<Inode>>,
    entries: Vec<Dirent>,
    now: u32,
}

impl Bfs {
    pub fn new(size_mb: u32) -> anyhow::Result<Self> {
        let image_size = u64::from(size_mb) * 1024 * 1024;
        if image_size > u64::from(u32::MAX) {
            anyhow::bail!("image size exceeds 4 GB");
        }
        let total_blocks = (image_size / BSIZE as u64) as u32;

        let inode_count = LAST_INO - ROOT_INO + 1;
        let inode_blocks = inode_count.div_ceil(INODES_PER_BLOCK);
        let data_start = 1 + inode_blocks;

        if total_blocks <= data_start + 1 {
            anyhow::bail!("filesystem too small");
        }

        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs() as u32)
            .unwrap_or(0);

        let inode_slots = inode_count as usize;
        let mut bfs = Self {
            image: vec![0u8; image_size as usize],
            total_blocks,
            next_data_block: data_start,
            next_inode: ROOT_INO + 1,
            inodes: vec![None; inode_slots],
            entries: Vec::new(),
            now,
        };

        let mut root = Inode::new(ROOT_INO, VDIR, S_IFDIR | 0o755, now);
        root.nlink = 2;
        bfs.inodes[0] = Some(root);

        bfs.entries.push(Dirent::new(ROOT_INO as u16, "."));
        bfs.entries.push(Dirent::new(ROOT_INO as u16, ".."));

        Ok(bfs)
    }

    pub fn populate_flat(&mut self, src: &Path) -> anyhow::Result<()> {
        for entry in fs::read_dir(src)? {
            let entry = entry?;
            let path = entry.path();
            let meta = fs::symlink_metadata(&path)?;
            let raw_name = entry.file_name();
            let name = raw_name
                .to_str()
                .ok_or_else(|| anyhow::anyhow!("non-UTF8 filename: {:?}", raw_name))?;

            if meta.is_file() {
                self.add_file(&path, name)?;
            } else if meta.is_dir() {
                self.populate_flat(&path)?;
            }
            // symlinks, devices, etc. are skipped (matches the C version)
        }
        Ok(())
    }

    fn add_file(&mut self, path: &Path, name: &str) -> anyhow::Result<()> {
        if name.len() > NAMELEN {
            eprintln!("mkfs.bfs: warning: name truncated to {NAMELEN} bytes: {name}");
        }

        for existing in &self.entries {
            if existing.matches_name(name) {
                anyhow::bail!("duplicate {NAMELEN}-byte directory name: {name}");
            }
        }

        if self.next_inode > LAST_INO {
            anyhow::bail!("no free inodes");
        }
        let ino = self.next_inode;
        self.next_inode += 1;

        let mut data = Vec::new();
        File::open(path)?.read_to_end(&mut data)?;
        let size = u32::try_from(data.len())
            .map_err(|_| anyhow::anyhow!("file exceeds 4 GB: {}", path.display()))?;

        let (perm, uid, gid) = read_perms(path)?;
        let mut inode = Inode::new(ino, VREG, S_IFREG | perm, self.now);
        inode.uid = uid;
        inode.gid = gid;

        if size == 0 {
            inode.sblock = 0;
            inode.eblock = 0;
            inode.eoffset = u32::MAX;
        } else {
            let block_count = size.div_ceil(BSIZE as u32);
            let start = self.reserve_blocks(block_count)?;
            inode.sblock = start;
            inode.eblock = start + block_count - 1;
            inode.eoffset = start * BSIZE as u32 + size - 1;
            let off = (start as usize) * BSIZE;
            self.image[off..off + data.len()].copy_from_slice(&data);
        }

        self.inodes[(ino - ROOT_INO) as usize] = Some(inode);
        self.entries.push(Dirent::new(ino as u16, name));
        Ok(())
    }

    fn reserve_blocks(&mut self, count: u32) -> anyhow::Result<u32> {
        if count == 0 {
            return Ok(0);
        }
        let start = self.next_data_block;
        if start + count > self.total_blocks {
            anyhow::bail!("no free blocks");
        }
        self.next_data_block += count;
        Ok(start)
    }

    pub fn write_to(mut self, path: &Path) -> anyhow::Result<Stats> {
        self.write_root_dir()?;
        self.write_inode_table();
        self.write_superblock();

        let mut f = File::create(path)?;
        f.write_all(&self.image)?;
        f.sync_all()?;

        Ok(Stats {
            total_blocks: self.total_blocks,
            entries: self.entries.len() as u32,
        })
    }

    fn write_root_dir(&mut self) -> anyhow::Result<()> {
        let entry_count = self.entries.len() as u32;
        let size = entry_count * DIRENT_SIZE as u32;
        let block_count = if size == 0 {
            0
        } else {
            size.div_ceil(BSIZE as u32)
        };
        let start = self.reserve_blocks(block_count)?;

        let root = self.inodes[0].as_mut().expect("root inode missing");
        root.sblock = start;
        root.eblock = if block_count == 0 {
            0
        } else {
            start + block_count - 1
        };
        root.eoffset = if size == 0 {
            u32::MAX
        } else {
            start * BSIZE as u32 + size - 1
        };

        let mut buf_off = (start as usize) * BSIZE;
        for entry in &self.entries {
            entry.write_into(&mut self.image[buf_off..buf_off + DIRENT_SIZE]);
            buf_off += DIRENT_SIZE;
        }
        Ok(())
    }

    fn write_inode_table(&mut self) {
        // Inode N lives at byte offset (N - ROOT_INO) * INODE_SIZE + BSIZE
        // (i.e. immediately after the superblock).
        for (idx, slot) in self.inodes.iter().enumerate() {
            if let Some(inode) = slot {
                let off = idx * INODE_SIZE + BSIZE;
                inode.write_into(&mut self.image[off..off + INODE_SIZE]);
            }
        }
    }

    fn write_superblock(&mut self) {
        let inode_blocks = (LAST_INO - ROOT_INO + 1).div_ceil(INODES_PER_BLOCK);
        let data_start_byte = (1 + inode_blocks) * BSIZE as u32;
        let s_end = self.total_blocks * BSIZE as u32 - 1;

        let sb = &mut self.image[..40];
        sb[0..4].copy_from_slice(&MAGIC.to_le_bytes());
        sb[4..8].copy_from_slice(&data_start_byte.to_le_bytes());
        sb[8..12].copy_from_slice(&s_end.to_le_bytes());
        sb[12..16].copy_from_slice(&u32::MAX.to_le_bytes()); // s_from
        sb[16..20].copy_from_slice(&u32::MAX.to_le_bytes()); // s_to
        sb[20..24].copy_from_slice(&(-1i32).to_le_bytes()); // s_bfrom
        sb[24..28].copy_from_slice(&(-1i32).to_le_bytes()); // s_bto
        sb[28..34].copy_from_slice(b"bfs\0\0\0");
        sb[34..40].copy_from_slice(b"basic\0");
    }
}

#[cfg(unix)]
fn read_perms(path: &Path) -> anyhow::Result<(u32, u32, u32)> {
    use std::os::unix::fs::MetadataExt;
    let m = fs::metadata(path)?;
    Ok((m.mode() & 0o777, m.uid(), m.gid()))
}

#[cfg(not(unix))]
fn read_perms(_path: &Path) -> anyhow::Result<(u32, u32, u32)> {
    Ok((0o644, 0, 0))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::TempDir;

    fn read_u32_le(buf: &[u8], off: usize) -> u32 {
        u32::from_le_bytes(buf[off..off + 4].try_into().unwrap())
    }

    #[test]
    fn empty_image_has_correct_superblock() {
        let dir = TempDir::new().unwrap();
        let img = dir.path().join("test.bfs");
        let bfs = Bfs::new(1).unwrap();
        bfs.write_to(&img).unwrap();

        let bytes = fs::read(&img).unwrap();
        assert_eq!(bytes.len(), 1024 * 1024);
        assert_eq!(read_u32_le(&bytes, 0), MAGIC);
        // s_start = data area start in bytes
        let inode_blocks = (LAST_INO - ROOT_INO + 1).div_ceil(INODES_PER_BLOCK);
        let expected_start = (1 + inode_blocks) * BSIZE as u32;
        assert_eq!(read_u32_le(&bytes, 4), expected_start);
        // s_end = last byte of last block
        assert_eq!(read_u32_le(&bytes, 8), bytes.len() as u32 - 1);
        // fsname / volume
        assert_eq!(&bytes[28..31], b"bfs");
        assert_eq!(&bytes[34..39], b"basic");
    }

    #[test]
    fn populate_writes_files_and_root_dirent() {
        let src = TempDir::new().unwrap();
        let mut f = File::create(src.path().join("hello.txt")).unwrap();
        f.write_all(b"hi there\n").unwrap();
        drop(f);

        let dst = TempDir::new().unwrap();
        let img_path = dst.path().join("out.bfs");
        let mut bfs = Bfs::new(1).unwrap();
        bfs.populate_flat(src.path()).unwrap();
        let stats = bfs.write_to(&img_path).unwrap();
        // ".", "..", "hello.txt"
        assert_eq!(stats.entries, 3);

        let bytes = fs::read(&img_path).unwrap();
        // Root dir starts at the data area; it contains 3 dirents = 48 bytes.
        let root_inode_off = BSIZE; // inode 2 at byte 512
        let root_sblock = read_u32_le(&bytes, root_inode_off + 4);
        let dir_off = (root_sblock as usize) * BSIZE;
        // Third dirent has the file's name in bytes 2..16.
        let third_name = &bytes[dir_off + 2 * DIRENT_SIZE + 2..dir_off + 3 * DIRENT_SIZE];
        assert!(third_name.starts_with(b"hello.txt"));
    }

    #[test]
    fn duplicate_truncated_name_is_rejected() {
        let src = TempDir::new().unwrap();
        File::create(src.path().join("aaaaaaaaaaaaaa01")).unwrap();
        File::create(src.path().join("aaaaaaaaaaaaaa02")).unwrap();
        let mut bfs = Bfs::new(1).unwrap();
        let err = bfs.populate_flat(src.path()).unwrap_err();
        assert!(err.to_string().contains("duplicate"));
    }
}
