use ext4_rs::{BlockDevice, Errno, Ext4, InodeFileType, BLOCK_SIZE};
use std::env;
use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::sync::{Arc, Mutex};

struct FileBlockDevice {
    file: Mutex<File>,
}

impl FileBlockDevice {
    fn open(path: &str) -> Self {
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .open(path)
            .expect("open ext4 test image");
        Self {
            file: Mutex::new(file),
        }
    }
}

impl BlockDevice for FileBlockDevice {
    fn read_offset(&self, offset: usize) -> Vec<u8> {
        let mut data = vec![0; BLOCK_SIZE];
        let mut file = self.file.lock().unwrap();
        file.seek(SeekFrom::Start(offset as u64)).unwrap();
        file.read_exact(&mut data).unwrap();
        data
    }

    fn write_offset(&self, offset: usize, data: &[u8]) {
        let mut file = self.file.lock().unwrap();
        file.seek(SeekFrom::Start(offset as u64)).unwrap();
        file.write_all(data).unwrap();
    }
}

fn create_regular_file(ext4: &Ext4, name: &str) -> u32 {
    let mut parent = 2;
    let mut name_offset = 0;
    ext4.generic_open(
        name,
        &mut parent,
        true,
        InodeFileType::S_IFREG.bits() | 0o600,
        &mut name_offset,
    )
    .unwrap_or_else(|error| panic!("create {name}: {error:?}"))
}

fn block_pattern(tag: u8, logical_block: usize) -> Vec<u8> {
    let mut data = vec![tag; BLOCK_SIZE];
    data[..8].copy_from_slice(&(logical_block as u64).to_le_bytes());
    data
}

fn write_block(ext4: &Ext4, inode: u32, tag: u8, logical_block: usize) {
    let data = block_pattern(tag, logical_block);
    let written = ext4
        .write_at(inode, logical_block * BLOCK_SIZE, &data)
        .unwrap_or_else(|error| panic!("write inode={inode} lblock={logical_block}: {error:?}"));
    assert_eq!(written, BLOCK_SIZE);
}

fn verify_block(ext4: &Ext4, inode: u32, tag: u8, logical_block: usize) {
    let expected = block_pattern(tag, logical_block);
    let mut actual = vec![0; BLOCK_SIZE];
    let read = ext4
        .read_at(inode, logical_block * BLOCK_SIZE, &mut actual)
        .unwrap_or_else(|error| panic!("read inode={inode} lblock={logical_block}: {error:?}"));
    assert_eq!(read, BLOCK_SIZE);
    assert_eq!(actual, expected, "data mismatch at logical block {logical_block}");
}

fn main() {
    let image = env::args().nth(1).expect("usage: extent_stress IMAGE");
    let device: Arc<dyn BlockDevice> = Arc::new(FileBlockDevice::open(&image));
    let ext4 = Ext4::open(device);

    let inode_a = create_regular_file(&ext4, "extent-a");
    let inode_b = create_regular_file(&ext4, "extent-b");

    // An external extent node contains 340 entries and the inode root contains
    // four indexes.  More than 1,360 one-block extents therefore forces a leaf
    // split followed by root growth to depth 2.  Descending logical order also
    // exercises insertion into the middle/beginning of full leaves.
    const FRAGMENTED_BLOCKS: usize = 1_500;
    const HIGH_BLOCK: usize = FRAGMENTED_BLOCKS + 64;
    write_block(&ext4, inode_a, b'A', HIGH_BLOCK);
    write_block(&ext4, inode_b, b'B', HIGH_BLOCK);
    for logical_block in (1..=FRAGMENTED_BLOCKS).rev() {
        // Interleaving two files makes their physical blocks non-contiguous, so
        // adjacent logical mappings cannot be merged into one extent.
        write_block(&ext4, inode_a, b'A', logical_block);
        write_block(&ext4, inode_b, b'B', logical_block);
    }

    for logical_block in [1, 169, 170, 339, 340, 341, 679, 680, 1_019, 1_020, 1_359, 1_360, 1_500, HIGH_BLOCK] {
        verify_block(&ext4, inode_a, b'A', logical_block);
        verify_block(&ext4, inode_b, b'B', logical_block);
    }

    // Removing a depth-2 tree must recursively release data, leaf and index
    // blocks; otherwise short-lived compiler outputs would still leak space.
    ext4.file_remove("extent-a")
        .unwrap_or_else(|error| panic!("remove depth-2 extent tree: {error:?}"));

    // Fill the rest of the small image and verify that the allocator's concrete
    // ENOSPC survives the write path instead of being flattened to EIO.
    let fill_inode = create_regular_file(&ext4, "fill-to-enospc");
    let chunk = vec![0x5a; 1024 * 1024];
    let mut offset = 0usize;
    loop {
        match ext4.write_at(fill_inode, offset, &chunk) {
            Ok(0) => panic!("zero-length progress before ENOSPC"),
            Ok(written) => offset += written,
            Err(error) => {
                assert_eq!(error.error(), Errno::ENOSPC, "unexpected terminal error");
                break;
            }
        }
    }

    let mkdir_error = ext4
        .ext4_dir_mk("must-fail-enospc")
        .expect_err("mkdir unexpectedly succeeded on a full filesystem");
    assert_eq!(mkdir_error.error(), Errno::ENOSPC);

    println!(
        "extent stress passed: inode_a={inode_a} inode_b={inode_b} ENOSPC_after={offset}"
    );
}
