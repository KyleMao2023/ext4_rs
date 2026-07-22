use crate::prelude::*;
use crate::utils::*;

use super::*;

bitflags! {
    #[derive(PartialEq, Eq)]
    pub struct DirEntryType: u8 {
        const EXT4_DE_UNKNOWN = 0;
        const EXT4_DE_REG_FILE = 1;
        const EXT4_DE_DIR = 2;
        const EXT4_DE_CHRDEV = 3;
        const EXT4_DE_BLKDEV = 4;
        const EXT4_DE_FIFO = 5;
        const EXT4_DE_SOCK = 6;
        const EXT4_DE_SYMLINK = 7;
    }
}

/// Directory entry for Ext4
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct Ext4DirEntry {
    pub inode: u32,               // Inode number this entry points to
    pub entry_len: u16,           // Distance to the next directory entry
    pub name_len: u8,             // Lower 8 bits of name length
    pub inner: Ext4DirEnInternal, // Union member
    pub name: [u8; 255],          // File name
}

/// Internal directory entry structure.
#[repr(C)]
#[derive(Clone, Copy)]
pub union Ext4DirEnInternal {
    pub name_length_high: u8, // Higher 8 bits of name length
    pub inode_type: u8,       // Type of the referenced inode (in rev >= 0.5)
}

/// Fake directory entry structure. Used for directory entry iteration.
#[repr(C)]
pub struct Ext4FakeDirEntry {
    inode: u32,
    entry_length: u16,
    name_length: u8,
    inode_type: u8,
}

pub const EXT4_DIR_ENTRY_HEADER_SIZE: usize = core::mem::size_of::<Ext4FakeDirEntry>();

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct Ext4DirEntryTail {
    pub reserved_zero1: u32,
    pub rec_len: u16,
    pub reserved_zero2: u8,
    pub reserved_ft: u8,
    pub checksum: u32, // crc32c(uuid+inum+dirblock)
}

pub struct Ext4DirSearchResult {
    pub dentry: Ext4DirEntry,
    pub pblock_id: usize,       // disk block id
    pub blocks_scanned: usize,  // directory blocks scanned during lookup
    pub dirents_scanned: usize, // directory entries examined during lookup
    pub offset: usize,          // offset in block
    pub prev_offset: usize,     //prev direntry offset
}

impl Ext4DirSearchResult {
    pub fn new(dentry: Ext4DirEntry) -> Self {
        Self {
            dentry,
            pblock_id: 0,
            blocks_scanned: 0,
            dirents_scanned: 0,
            offset: 0,
            prev_offset: 0,
        }
    }
}

impl Debug for Ext4DirEnInternal {
    fn fmt(&self, f: &mut core::fmt::Formatter) -> core::fmt::Result {
        unsafe {
            write!(
                f,
                "Ext4DirEnInternal {{ name_length_high: {:?} }}",
                self.name_length_high
            )
        }
    }
}

impl Default for Ext4DirEnInternal {
    fn default() -> Self {
        Self {
            name_length_high: 0,
        }
    }
}

impl Default for Ext4DirEntry {
    fn default() -> Self {
        Self {
            inode: 0,
            entry_len: 0,
            name_len: 0,
            inner: Ext4DirEnInternal::default(),
            name: [0; 255],
        }
    }
}

impl TryFrom<&[u8]> for Ext4DirEntry {
    type Error = Ext4Error;

    fn try_from(data: &[u8]) -> Result<Self> {
        if data.len() < EXT4_DIR_ENTRY_HEADER_SIZE {
            return Err(Ext4Error::with_message(
                Errno::EIO,
                "Short ext4 directory entry header",
            ));
        }

        let entry_len = u16::from_le_bytes([data[4], data[5]]) as usize;
        let name_len = data[6] as usize;
        if entry_len < EXT4_DIR_ENTRY_HEADER_SIZE
            || entry_len % 4 != 0
            || entry_len > data.len()
            || name_len > entry_len - EXT4_DIR_ENTRY_HEADER_SIZE
        {
            return Err(Ext4Error::with_message(
                Errno::EIO,
                "Invalid ext4 directory entry",
            ));
        }

        let mut entry = Self::default();
        entry.inode = u32::from_le_bytes([data[0], data[1], data[2], data[3]]);
        entry.entry_len = entry_len as u16;
        entry.name_len = name_len as u8;
        entry.inner.inode_type = data[7];
        entry.name[..name_len].copy_from_slice(
            &data[EXT4_DIR_ENTRY_HEADER_SIZE..EXT4_DIR_ENTRY_HEADER_SIZE + name_len],
        );
        Ok(entry)
    }
}

/// Directory entry implementation.
impl Ext4DirEntry {
    /// Parse one variable-length on-disk directory record.  `data_end` is the
    /// end of the directory payload and excludes the checksum tail.
    pub fn from_slice_at(data: &[u8], offset: usize, data_end: usize) -> Result<Self> {
        if data_end > data.len() || offset > data_end {
            return Err(Ext4Error::with_message(
                Errno::EIO,
                "Invalid ext4 directory block bounds",
            ));
        }
        Self::try_from(&data[offset..data_end])
    }

    /// Check if the directory entry is unused.
    pub fn unused(&self) -> bool {
        self.inode == 0
    }

    /// Set the directory entry as unused.
    pub fn set_unused(&mut self) {
        self.inode = 0
    }

    /// Check name
    pub fn compare_name(&self, name: &str) -> bool {
        if self.name_len as usize == name.len() {
            return &self.name[..name.len()] == name.as_bytes();
        }
        false
    }

    /// Entry length
    pub fn entry_len(&self) -> u16 {
        self.entry_len
    }

    /// Dir type
    pub fn get_de_type(&self) -> u8 {
        unsafe { self.inner.inode_type }
    }

    /// Get name to string
    pub fn get_name(&self) -> String {
        let name_len = self.name_len as usize;
        let name = &self.name[..name_len];
        let name = core::str::from_utf8(name).unwrap();
        name.to_string()
    }

    /// Get name len
    pub fn get_name_len(&self) -> usize {
        self.name_len as usize
    }

    /// Calculate the actual length of a directory entry (excluding padding bytes)
    pub fn actual_len(&self) -> usize {
        size_of::<Ext4FakeDirEntry>() + self.name_len as usize
    }

    /// Calculate the aligned length of a directory entry (including padding bytes)
    pub fn align_len(&self) -> usize {
        let mut len = self.actual_len();
        len = (len + 3) & !3;
        len
    }

    pub fn write_entry(
        &mut self,
        entry_len: u16,
        inode: u32,
        name: &str,
        de_type: &DirEntryType,
    ) -> Result<()> {
        if name.len() > self.name.len()
            || EXT4_DIR_ENTRY_HEADER_SIZE + name.len() > entry_len as usize
        {
            return Err(Ext4Error::with_message(
                Errno::ENAMETOOLONG,
                "Invalid ext4 directory entry name",
            ));
        }
        self.inode = inode;
        self.entry_len = entry_len;
        self.name_len = name.len() as u8;
        self.inner.inode_type = de_type.bits();
        self.name.fill(0);
        self.name[..name.len()].copy_from_slice(name.as_bytes());
        Ok(())
    }
}

/// The size of a block without its tail
const BLOCK_DATA_SIZE: usize = BLOCK_SIZE - core::mem::size_of::<Ext4DirEntryTail>();

impl Ext4DirEntry {
    /// Get the checksum of the directory entry.
    #[allow(unused)]
    pub fn ext4_dir_get_csum(
        s: &Ext4Superblock,
        dir_inode: u32,
        blk_data: &[u8],
        ino_gen: u32,
    ) -> u32 {
        let mut csum = 0;

        let uuid = s.uuid;

        csum = ext4_crc32c(EXT4_CRC32_INIT, &uuid, uuid.len() as u32);
        csum = ext4_crc32c(csum, &dir_inode.to_le_bytes(), 4);
        csum = ext4_crc32c(csum, &ino_gen.to_le_bytes(), 4);
        let mut data = [0u8; BLOCK_DATA_SIZE];
        unsafe {
            core::ptr::copy_nonoverlapping(blk_data.as_ptr(), data.as_mut_ptr(), BLOCK_DATA_SIZE);
        }

        csum = ext4_crc32c(csum, &data[..], BLOCK_DATA_SIZE.try_into().unwrap());
        csum
    }

    /// Write de to block
    pub fn write_de_to_blk(&self, dst_blk: &mut Block, offset: usize) {
        self.copy_to_slice(&mut dst_blk.data, offset)
            .expect("invalid ext4 directory entry write");
    }

    /// Serialize only the fixed header and the actual name.  The in-memory
    /// structure contains a 255-byte name buffer, but an ext4 directory block
    /// stores variable-length records and must never be accessed as that full
    /// structure.
    pub fn copy_to_slice(&self, array: &mut [u8], offset: usize) -> Result<()> {
        let name_len = self.name_len as usize;
        let record_len = self.entry_len as usize;
        let actual_len = EXT4_DIR_ENTRY_HEADER_SIZE + name_len;
        let Some(record_end) = offset.checked_add(record_len) else {
            return Err(Ext4Error::with_message(
                Errno::EIO,
                "Ext4 directory entry write overflow",
            ));
        };
        if record_len < EXT4_DIR_ENTRY_HEADER_SIZE
            || record_len % 4 != 0
            || actual_len > record_len
            || record_end > array.len()
        {
            return Err(Ext4Error::with_message(
                Errno::EIO,
                "Invalid ext4 directory entry write",
            ));
        }
        array[offset..offset + 4].copy_from_slice(&self.inode.to_le_bytes());
        array[offset + 4..offset + 6].copy_from_slice(&self.entry_len.to_le_bytes());
        array[offset + 6] = self.name_len;
        array[offset + 7] = self.get_de_type();
        array[offset + EXT4_DIR_ENTRY_HEADER_SIZE..offset + actual_len]
            .copy_from_slice(&self.name[..name_len]);
        Ok(())
    }

    pub fn set_inode_in_slice(array: &mut [u8], offset: usize, inode: u32) -> Result<()> {
        let Some(end) = offset.checked_add(4) else {
            return Err(Ext4Error::with_message(
                Errno::EIO,
                "Invalid directory offset",
            ));
        };
        let Some(field) = array.get_mut(offset..end) else {
            return Err(Ext4Error::with_message(
                Errno::EIO,
                "Invalid directory offset",
            ));
        };
        field.copy_from_slice(&inode.to_le_bytes());
        Ok(())
    }

    pub fn set_entry_len_in_slice(array: &mut [u8], offset: usize, entry_len: u16) -> Result<()> {
        let Some(start) = offset.checked_add(4) else {
            return Err(Ext4Error::with_message(
                Errno::EIO,
                "Invalid directory offset",
            ));
        };
        let Some(end) = start.checked_add(2) else {
            return Err(Ext4Error::with_message(
                Errno::EIO,
                "Invalid directory offset",
            ));
        };
        let Some(field) = array.get_mut(start..end) else {
            return Err(Ext4Error::with_message(
                Errno::EIO,
                "Invalid directory offset",
            ));
        };
        field.copy_from_slice(&entry_len.to_le_bytes());
        Ok(())
    }
}

impl Ext4DirEntryTail {
    pub fn new() -> Self {
        Self {
            reserved_zero1: 0,
            rec_len: size_of::<Ext4DirEntryTail>() as u16,
            reserved_zero2: 0,
            reserved_ft: 0xDE,
            checksum: 0,
        }
    }
    pub fn tail_set_csum(
        &mut self,
        s: &Ext4Superblock,
        dir_inode: u32,
        blk_data: &[u8],
        ino_gen: u32,
    ) {
        let csum = Ext4DirEntry::ext4_dir_get_csum(s, dir_inode, blk_data, ino_gen);
        self.checksum = csum;
    }

    pub fn copy_to_slice(&self, array: &mut [u8]) {
        unsafe {
            let offset = BLOCK_SIZE - core::mem::size_of::<Ext4DirEntryTail>();
            let de_ptr = self as *const Ext4DirEntryTail as *const u8;
            let array_ptr = array as *mut [u8] as *mut u8;
            let count = core::mem::size_of::<Ext4DirEntryTail>();
            core::ptr::copy_nonoverlapping(de_ptr, array_ptr.add(offset), count);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_variable_record_near_block_end() {
        let mut block = vec![0u8; BLOCK_SIZE];
        let offset = 0xf04;
        let record_len = 40u16;
        let name = b"1234567890123456789012345678901";
        block[offset..offset + 4].copy_from_slice(&42u32.to_le_bytes());
        block[offset + 4..offset + 6].copy_from_slice(&record_len.to_le_bytes());
        block[offset + 6] = name.len() as u8;
        block[offset + 7] = DirEntryType::EXT4_DE_REG_FILE.bits();
        block
            [offset + EXT4_DIR_ENTRY_HEADER_SIZE..offset + EXT4_DIR_ENTRY_HEADER_SIZE + name.len()]
            .copy_from_slice(name);

        let entry =
            Ext4DirEntry::from_slice_at(&block, offset, BLOCK_SIZE - size_of::<Ext4DirEntryTail>())
                .unwrap();
        assert_eq!(entry.inode, 42);
        assert_eq!(entry.entry_len(), record_len);
        assert_eq!(&entry.name[..name.len()], name);
    }

    #[test]
    fn rejects_record_crossing_directory_payload_end() {
        let mut block = vec![0u8; BLOCK_SIZE];
        let data_end = BLOCK_SIZE - size_of::<Ext4DirEntryTail>();
        let offset = data_end - EXT4_DIR_ENTRY_HEADER_SIZE;
        block[offset + 4..offset + 6].copy_from_slice(&40u16.to_le_bytes());

        assert!(Ext4DirEntry::from_slice_at(&block, offset, data_end).is_err());
    }

    #[test]
    fn serializes_only_header_and_actual_name() {
        let mut block = vec![0xa5u8; BLOCK_SIZE];
        let mut entry = Ext4DirEntry::default();
        entry
            .write_entry(40, 7, "short", &DirEntryType::EXT4_DE_REG_FILE)
            .unwrap();
        entry.copy_to_slice(&mut block, 0xf04).unwrap();

        assert_eq!(block[0xf04 + EXT4_DIR_ENTRY_HEADER_SIZE + 5], 0xa5);
    }
}
