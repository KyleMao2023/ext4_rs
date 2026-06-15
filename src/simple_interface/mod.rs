use core::panic::RefUnwindSafe;

use crate::prelude::*;

use crate::ext4_defs::*;
use crate::return_errno;
use crate::return_errno_with_message;
use crate::utils::path_check;

// export some definitions
pub use crate::ext4_defs::Ext4;
pub use crate::ext4_defs::BLOCK_SIZE;
pub use crate::ext4_defs::{BlockDevice, BlockWrite};
pub use crate::ext4_defs::InodeFileType;


/// simple interface for ext4
impl Ext4 {

    /// Parse the file access flags (such as "r", "w", "a", etc.) and convert them to system constants.
    ///
    /// This method parses common file access flags into their corresponding bitwise constants defined in `libc`.
    ///
    /// # Arguments
    /// * `flags` - The string representation of the file access flags (e.g., "r", "w", "a", "r+", etc.).
    ///
    /// # Returns
    /// * `Result<i32>` - The corresponding bitwise flag constants (e.g., `O_RDONLY`, `O_WRONLY`, etc.), or an error if the flags are invalid.
    fn ext4_parse_flags(&self, flags: &str) -> Result<i32> {
        match flags {
            "r" | "rb" => Ok(O_RDONLY),
            "w" | "wb" => Ok(O_WRONLY | O_CREAT | O_TRUNC),
            "a" | "ab" => Ok(O_WRONLY | O_CREAT | O_APPEND),
            "r+" | "rb+" | "r+b" => Ok(O_RDWR),
            "w+" | "wb+" | "w+b" => Ok(O_RDWR | O_CREAT | O_TRUNC),
            "a+" | "ab+" | "a+b" => Ok(O_RDWR | O_CREAT | O_APPEND),
            _ => Err(Ext4Error::new(Errno::EINVAL)),
        }
    }

    /// Open a file at the specified path and return the corresponding inode number.
    ///
    /// Open a file by searching for the given path starting from the root directory (`ROOT_INODE`).
    /// If the file does not exist and the `O_CREAT` flag is specified, the file will be created.
    ///
    /// # Arguments
    /// * `path` - The path of the file to open.
    /// * `flags` - The access flags (e.g., "r", "w", "a", etc.).
    ///
    /// # Returns
    /// * `Result<u32>` - Returns the inode number of the opened file if successful.
    pub fn ext4_file_open(
        &self,
        path: &str,
        flags: &str,
    ) -> Result<u32> {
        let mut parent_inode_num = ROOT_INODE;
        let filetype = InodeFileType::S_IFREG;

        let iflags = self.ext4_parse_flags(flags).unwrap();

        let filetype = InodeFileType::S_IFDIR;

        let mut create = false;
        if iflags & O_CREAT != 0 {
            create = true;
        }

        self.generic_open(path, &mut parent_inode_num, create, filetype.bits(), &mut 0)
    }

    /// Create a new directory at the specified path.
    /// 
    /// Checks if the directory already exists by searching from the root directory (`ROOT_INODE`).
    /// If the directory does not exist, it creates the directory under the root directory and returns its inode number.
    /// 
    /// # Arguments
    /// * `path` - The path where the directory will be created.
    /// 
    /// # Returns
    /// * `Result<u32>` - The inode number of the newly created directory if successful, 
    ///   or an error (`Errno::EEXIST`) if the directory already exists.
    pub fn ext4_dir_mk(&self, path: &str) -> Result<u32> {
        let mut search_result = Ext4DirSearchResult::new(Ext4DirEntry::default());
        let r = self.dir_find_entry(ROOT_INODE, path, &mut search_result);
        if r.is_ok() {
            return_errno!(Errno::EEXIST);
        }
        let mut parent_inode_num = ROOT_INODE;
        let filetype = InodeFileType::S_IFDIR;

        self.generic_open(path, &mut parent_inode_num, true, filetype.bits(), &mut 0)
    }


    /// Open a directory at the specified path and return the corresponding inode number.
    ///
    /// Opens a directory by searching for the given path starting from the root directory (`ROOT_INODE`).
    ///
    /// # Arguments
    /// * `path` - The path of the directory to open.
    ///
    /// # Returns
    /// * `Result<u32>` - Returns the inode number of the opened directory if successful.
    pub fn ext4_dir_open(
        &self,
        path: &str,
    ) -> Result<u32> {
        let mut parent_inode_num = ROOT_INODE;
        let filetype = InodeFileType::S_IFDIR;
        self.generic_open(path, &mut parent_inode_num, false, filetype.bits(), &mut 0)
    }

    /// Get dir entries of a inode
    ///
    /// Params:
    /// inode: u32 - inode number of the directory
    /// assert!(inode.is_dir());
    ///
    /// Returns:
    /// `Vec<Ext4DirEntry>` - list of directory entries
    pub fn ext4_dir_get_entries(&self, inode: u32) -> Vec<Ext4DirEntry> {
        let mut entries = self.dir_get_entries(inode);
        entries
    }

    /// Look up one directory entry by name and return its inode number and
    /// ext4 dirent type.
    pub fn ext4_dir_lookup(&self, parent_inode: u32, name: &str) -> Option<(u32, u8)> {
        let mut search_result = Ext4DirSearchResult::new(Ext4DirEntry::default());
        self.dir_find_entry(parent_inode, name, &mut search_result)
            .ok()
            .map(|_| {
                let de = search_result.dentry;
                (de.inode, de.get_de_type())
            })
    }

    /// Fill `buf` with `linux_dirent64` records starting at the logical
    /// directory byte position `offset`.
    pub fn ext4_dir_getdents64(&self, inode: u32, offset: usize, buf: &mut [u8]) -> usize {
        let inode_ref = self.get_inode_ref(inode);
        if !inode_ref.inode.is_dir() {
            return 0;
        }

        let inode_size = inode_ref.inode.size() as usize;
        if offset >= inode_size {
            return 0;
        }

        let total_blocks = inode_size.div_ceil(BLOCK_SIZE);
        let tail_size = core::mem::size_of::<Ext4DirEntryTail>();
        let mut written = 0usize;
        let mut iblock = offset / BLOCK_SIZE;
        let mut entry_off = offset % BLOCK_SIZE;

        while iblock < total_blocks {
            let search_path = self.find_extent(&inode_ref, iblock as u32);
            let Ok(path) = search_path else {
                break;
            };
            let pblock = path.path.last().unwrap().pblock;
            let ext4block = Block::load(&self.block_device, pblock as usize * BLOCK_SIZE);
            let block_base = iblock * BLOCK_SIZE;

            if entry_off >= BLOCK_SIZE.saturating_sub(tail_size) {
                iblock += 1;
                entry_off = 0;
                continue;
            }

            while entry_off < BLOCK_SIZE - tail_size {
                let stream_pos = block_base + entry_off;
                if stream_pos >= inode_size {
                    return written;
                }

                let de: Ext4DirEntry = ext4block.read_offset_as(entry_off);
                let entry_len = de.entry_len() as usize;
                if entry_len == 0 {
                    return written;
                }
                let next_stream_pos = stream_pos + entry_len;

                if de.unused() {
                    entry_off += entry_len;
                    continue;
                }

                let name_len = de.get_name_len();
                let name_bytes = &de.name[..name_len];
                let reclen = (19 + name_len + 1 + 7) & !7usize;
                if written + reclen > buf.len() {
                    return written;
                }

                buf[written..written + 8].copy_from_slice(&(de.inode as u64).to_le_bytes());
                buf[written + 8..written + 16]
                    .copy_from_slice(&(next_stream_pos as i64).to_le_bytes());
                buf[written + 16..written + 18].copy_from_slice(&(reclen as u16).to_le_bytes());
                buf[written + 18] = match de.get_de_type() {
                    2 => 4,
                    7 => 10,
                    3 => 2,
                    4 => 6,
                    5 => 1,
                    6 => 12,
                    1 => 8,
                    _ => 0,
                };
                buf[written + 19..written + 19 + name_len].copy_from_slice(name_bytes);
                buf[written + 19 + name_len] = 0;
                for b in &mut buf[written + 19 + name_len + 1..written + reclen] {
                    *b = 0;
                }
                written += reclen;
                entry_off += entry_len;
            }

            iblock += 1;
            entry_off = 0;
        }

        written
    }

    /// Read data from a file starting from a given offset.
    ///
    /// Reads data from the file starting at the specified inode (`ino`), with a given offset and size.
    ///
    /// # Arguments
    /// * `ino` - The inode number of the file to read from.
    /// * `size` - The number of bytes to read.
    /// * `offset` - The offset from where to start reading.
    ///
    /// # Returns
    /// * `Result<Vec<u8>>` - The data read from the file.
    pub fn ext4_file_read(
        &self,
        ino: u64,
        size: u32,
        offset: i64,
    ) -> Result<Vec<u8>> {
        let mut data = vec![0u8; size as usize];
        let read_size = self.read_at(ino as u32, offset as usize, &mut data)?;
        let r = data[..read_size].to_vec();
        Ok(r)
    }

    /// Write data to a file starting at a given offset.
    ///
    /// Writes data to the file starting at the specified inode (`ino`) and offset.
    ///
    /// # Arguments
    /// * `ino` - The inode number of the file to write to.
    /// * `offset` - The offset in the file where the data will be written.
    /// * `data` - The data to write to the file.
    ///
    /// # Returns
    /// * `Result<usize>` - The number of bytes written to the file.
    pub fn ext4_file_write(
        &self,
        ino: u64,
        offset: i64,
        data: &[u8],
    ) -> Result<usize> {
        let write_size = self.write_at(ino as u32, offset as usize, data)?;
        Ok(write_size)
    }

}
