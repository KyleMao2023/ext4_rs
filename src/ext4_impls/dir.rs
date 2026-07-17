use crate::prelude::*;
use crate::return_errno_with_message;

use crate::ext4_defs::*;

impl Ext4 {
    /// Find a directory entry in a directory
    ///
    /// Params:
    /// parent_inode: u32 - inode number of the parent directory
    /// name: &str - name of the entry to find
    /// result: &mut Ext4DirSearchResult - result of the search
    ///
    /// Returns:
    /// `Result<usize>` - status of the search
    pub fn dir_find_entry(
        &self,
        parent_inode: u32,
        name: &str,
        result: &mut Ext4DirSearchResult,
    ) -> Result<usize> {
        result.blocks_scanned = 0;
        result.dirents_scanned = 0;
        // load parent inode
        let parent = self.get_inode_ref(parent_inode);
        assert!(parent.inode.is_dir());

        // start from the first logical block
        let mut iblock = 0;
        // physical block id
        let mut fblock: Ext4Fsblk = 0;

        // calculate total blocks
        let inode_size: u64 = parent.inode.size();
        let total_blocks: u64 = inode_size / BLOCK_SIZE as u64;

        // iterate all blocks
        while iblock < total_blocks {
            let search_path = self.find_extent(&parent, iblock as u32);

            if let Ok(path) = search_path {
                // get the last path
                let path = path.path.last().unwrap();

                // get physical block id
                fblock = path.pblock;
                result.blocks_scanned += 1;

                // load physical block
                let mut ext4block =
                    Block::load(&self.block_device, fblock as usize * BLOCK_SIZE);

                // find entry in block
                let r = self.dir_find_in_block(&ext4block, name, result);

                if r.is_ok() {
                    result.pblock_id = fblock as usize;
                    return Ok(EOK);
                }
            } else {
                return_errno_with_message!(Errno::ENOENT, "dir search fail")
            }
            // go to next block
            iblock += 1
        }

        return_errno_with_message!(Errno::ENOENT, "dir search fail");
    }

    /// Find a directory entry in a block
    ///
    /// Params:
    /// block: &mut Block - block to search in
    /// name: &str - name of the entry to find
    ///
    /// Returns:
    /// result: Ext4DirEntry - result of the search
    pub fn dir_find_in_block(
        &self,
        block: &Block,
        name: &str,
        result: &mut Ext4DirSearchResult,
    ) -> Result<Ext4DirEntry> {
        let mut offset = 0;
        let mut prev_de_offset = 0;

        // start from the first entry
        while offset < BLOCK_SIZE - core::mem::size_of::<Ext4DirEntryTail>() {
            let de: Ext4DirEntry = block.read_offset_as(offset);
            result.dirents_scanned += 1;
            if !de.unused() && de.compare_name(name) {
                result.dentry = de;
                result.offset = offset;
                result.prev_offset = prev_de_offset;
                return Ok(de);
            }

            prev_de_offset = offset;
            // go to next entry
            offset += de.entry_len() as usize;
        }
        return_errno_with_message!(Errno::ENOENT, "dir find in block failed");
    }

    /// Get dir entries of a inode
    ///
    /// Params:
    /// inode: u32 - inode number of the directory
    ///
    /// Returns:
    /// `Vec<Ext4DirEntry>` - list of directory entries
    pub fn dir_get_entries(&self, inode: u32) -> Vec<Ext4DirEntry> {
        let mut entries = Vec::new();

        // load inode
        let inode_ref = self.get_inode_ref(inode);
        assert!(inode_ref.inode.is_dir());

        // calculate total blocks
        let inode_size = inode_ref.inode.size();
        let total_blocks = inode_size / BLOCK_SIZE as u64;

        // start from the first logical block
        let mut iblock = 0;

        // iterate all blocks
        while iblock < total_blocks {
            // get physical block id of a logical block id
            let search_path = self.find_extent(&inode_ref, iblock as u32);

            if let Ok(path) = search_path {
                // get the last path
                let path = path.path.last().unwrap();

                // get physical block id
                let fblock = path.pblock;

                // load physical block
                let ext4block =
                    Block::load(&self.block_device, fblock as usize * BLOCK_SIZE);
                let mut offset = 0;

                // iterate all entries in a block
                while offset < BLOCK_SIZE - core::mem::size_of::<Ext4DirEntryTail>() {
                    let de: Ext4DirEntry = ext4block.read_offset_as(offset);
                    if !de.unused() {
                        entries.push(de);
                    }
                    offset += de.entry_len() as usize;
                }
            }

            // go ot next block
            iblock += 1;
        }
        entries
    }

    pub fn dir_set_csum(&self, dst_blk: &mut Block, dir_inode: u32, ino_gen: u32) {
        let tail_offset = BLOCK_SIZE - size_of::<Ext4DirEntryTail>();
        let mut tail: Ext4DirEntryTail = *dst_blk.read_offset_as_mut(tail_offset);

        tail.tail_set_csum(&self.super_block, dir_inode, &dst_blk.data[..], ino_gen);

        tail.copy_to_slice(&mut dst_blk.data);
    }

    /// Add a new entry to a directory
    ///
    /// Params:
    /// parent: &mut Ext4InodeRef - parent directory inode reference
    /// child: &mut Ext4InodeRef - child inode reference
    /// path: &str - path of the new entry
    ///
    /// Returns:
    /// `Result<usize>` - status of the operation
    pub fn dir_add_entry(
        &self,
        parent: &mut Ext4InodeRef,
        child: &Ext4InodeRef,
        name: &str,
    ) -> Result<usize> {
        // calculate total blocks
        let inode_size: u64 = parent.inode.size();
        let block_size = self.super_block.block_size();
        let total_blocks: u64 = inode_size / block_size as u64;

        let inode_mode = child.inode.mode();
        
        let de_type = if InodeFileType::from_bits_truncate(inode_mode) == InodeFileType::S_IFDIR {
            DirEntryType::EXT4_DE_DIR
        } else if InodeFileType::from_bits_truncate(inode_mode) == InodeFileType::S_IFLNK {
            DirEntryType::EXT4_DE_SYMLINK
        } else {
            DirEntryType::EXT4_DE_REG_FILE
        };

        // iterate all blocks
        let mut iblock = 0;
        while iblock < total_blocks {
            // get physical block id of a logical block id
            let pblock = self.get_pblock_idx(parent, iblock as u32)?;

            // load physical block
            let mut ext4block =
                Block::load(&self.block_device, pblock as usize * BLOCK_SIZE);

            let result = self.try_insert_to_existing_block(&mut ext4block, name, child.inode_num, &de_type);

            if result.is_ok() {
                // set checksum
                self.dir_set_csum(
                    &mut ext4block,
                    parent.inode_num,
                    parent.inode.generation(),
                );
                ext4block.sync_blk_to_disk(&self.block_device);

                return Ok(EOK);
            }

            // go ot next block
            iblock += 1;
        }

        // no space in existing blocks, need to add new block
        let new_block = self.append_inode_pblk(parent)?;

        // load new block
        let mut new_ext4block =
            Block::load(&self.block_device, new_block as usize * BLOCK_SIZE);

        // write new entry to the new block
        // must succeed, as we just allocated the block
        self.insert_to_new_block(&mut new_ext4block, child.inode_num, name, &de_type);

        // set checksum
        self.dir_set_csum(
            &mut new_ext4block,
            parent.inode_num,
            parent.inode.generation(),
        );
        new_ext4block.sync_blk_to_disk(&self.block_device);

        Ok(EOK)
    }

    /// Try to insert a new entry to an existing block
    ///
    /// Params:
    /// block: &mut Block - block to insert the new entry
    /// name: &str - name of the new entry
    /// inode: u32 - inode number of the new entry
    ///
    /// Returns:
    /// `Result<usize>` - status of the operation
    pub fn try_insert_to_existing_block(
        &self,
        block: &mut Block,
        name: &str,
        child_inode: u32,
        de_type: &DirEntryType,
    ) -> Result<usize> {
        // required length aligned to 4 bytes
        let required_len = {
            let mut len = size_of::<Ext4DirEntry>() + name.len();
            if len % 4 != 0 {
                len += 4 - (len % 4);
            }
            len
        };

        let mut offset = 0;
        let data_end = BLOCK_SIZE - size_of::<Ext4DirEntryTail>();

        // Start from the first entry
        while offset < data_end {
            let mut de = Ext4DirEntry::try_from(&block.data[offset..]).unwrap();
            let rec_len = de.entry_len as usize;

            // Every ext4 directory record must contain at least its fixed
            // header, be four-byte aligned, and stay within the data portion
            // of the block.  In particular, reject rec_len == 0 instead of
            // letting a malformed record turn this scan into an infinite
            // loop.
            let Some(next_offset) = offset.checked_add(rec_len) else {
                return_errno_with_message!(Errno::EIO, "Invalid directory entry length");
            };
            if rec_len < size_of::<Ext4FakeDirEntry>()
                || rec_len % 4 != 0
                || next_offset > data_end
            {
                return_errno_with_message!(Errno::EIO, "Invalid directory entry length");
            }

            if de.unused() {
                // An inode number of zero denotes a reusable record.  This
                // occurs legitimately when the first entry in a directory
                // block is removed and therefore cannot be coalesced with a
                // predecessor.  The old code continued without advancing
                // `offset`, which compiled into a literal self-loop.
                if rec_len >= required_len {
                    let mut new_entry = Ext4DirEntry::default();
                    new_entry.write_entry(rec_len as u16, child_inode, name, de_type);
                    new_entry.copy_to_slice(&mut block.data, offset);
                    block.sync_blk_to_disk(&self.block_device);
                    return Ok(EOK);
                }
                offset = next_offset;
                continue;
            }

            let used_len = de.name_len as usize;
            let mut sz = core::mem::size_of::<Ext4FakeDirEntry>() + used_len;
            if used_len % 4 != 0 {
                sz += 4 - used_len % 4;
            }

            if sz > rec_len {
                return_errno_with_message!(Errno::EIO, "Invalid directory entry length");
            }
            let free_space = rec_len - sz;

            // If there is enough free space
            if free_space >= required_len {
                // Create new directory entry
                let mut new_entry = Ext4DirEntry::default();

                // Update existing entry length and copy both entries back to block data
                de.entry_len = sz as u16;

                // should not always be a directory
                // let de_type = DirEntryType::EXT4_DE_DIR;
                new_entry.write_entry(free_space as u16, child_inode, name, de_type);

                // update parent_de and new_de to blk_data
                de.copy_to_slice(&mut block.data, offset);
                new_entry.copy_to_slice(&mut block.data, offset + sz);

                // Sync to disk
                block.sync_blk_to_disk(&self.block_device);

                return Ok(EOK);
            }

            // Move to the next entry
            offset = next_offset;
        }

        return_errno_with_message!(Errno::ENOSPC, "No space in block for new entry");
    }

    /// Insert a new entry to a new block
    ///
    /// Params:
    /// block: &mut Block - block to insert the new entry
    /// name: &str - name of the new entry
    /// inode: u32 - inode number of the new entry
    pub fn insert_to_new_block(
        &self,
        block: &mut Block,
        inode: u32,
        name: &str,
        de_type: &DirEntryType,
    ) {
        // write new entry
        let mut new_entry = Ext4DirEntry::default();
        let el = BLOCK_SIZE - size_of::<Ext4DirEntryTail>();
        new_entry.write_entry(el as u16, inode, name, &de_type);
        new_entry.copy_to_slice(&mut block.data, 0);

        copy_dir_entry_to_array(&new_entry, &mut block.data, 0);

        // init tail for new block
        let tail = Ext4DirEntryTail::new();
        tail.copy_to_slice(&mut block.data);
    }

    pub fn dir_remove_entry(&self, parent: &mut Ext4InodeRef, path: &str) -> Result<usize> {
        // get remove_entry pos in parent and its prev entry
        let mut result = Ext4DirSearchResult::new(Ext4DirEntry::default());

        let r = self.dir_find_entry(parent.inode_num, path, &mut result)?;

        let mut ext4block = Block::load(&self.block_device, result.pblock_id * BLOCK_SIZE);

        // Invalidate entry first
        let de_del: &mut Ext4DirEntry = ext4block.read_offset_as_mut(result.offset);
        de_del.inode = 0;

        // Store entry position in block
        let pos = result.offset;

        // If entry is not the first in block, it must be merged with previous entry
        if pos != 0 {
            let mut offset = 0;

            // Start from the first entry in block
            let mut tmp_de: Ext4DirEntry = ext4block.read_offset_as(offset);
            let mut de_len = tmp_de.entry_len();

            // Find direct predecessor of removed entry
            while (offset + de_len as usize) < pos {
                offset += de_len as usize;
                tmp_de = ext4block.read_offset_as(offset);
                de_len = tmp_de.entry_len();
            }
            
            assert!(de_len as usize + offset == pos, "Invalid predecessor calculation");

            // Add removed entry length to predecessor's length
            let del_len = result.dentry.entry_len();
            let mut tmp_de_mut: &mut Ext4DirEntry = ext4block.read_offset_as_mut(offset);
            tmp_de_mut.entry_len = de_len + del_len;
        }

        self.dir_set_csum(
            &mut ext4block,
            parent.inode_num,
            parent.inode.generation(),
        );
        ext4block.sync_blk_to_disk(&self.block_device);

        Ok(EOK)
    }

    pub fn dir_has_entry(&self, dir_inode: u32) -> bool {
        // load parent inode
        let parent = self.get_inode_ref(dir_inode);
        assert!(parent.inode.is_dir());

        // start from the first logical block
        let mut iblock = 0;
        // physical block id
        let mut fblock: Ext4Fsblk = 0;

        // calculate total blocks
        let inode_size: u64 = parent.inode.size();
        let total_blocks: u64 = inode_size / BLOCK_SIZE as u64;

        // iterate all blocks
        while iblock < total_blocks {
            let search_path = self.find_extent(&parent, iblock as u32);

            if let Ok(path) = search_path {
                // get the last path
                let path = path.path.last().unwrap();

                // get physical block id
                fblock = path.pblock;

                // load physical block
                let ext4block =
                    Block::load(&self.block_device, fblock as usize * BLOCK_SIZE);

                // start from the first entry
                let mut offset = 0;
                while offset < BLOCK_SIZE - core::mem::size_of::<Ext4DirEntryTail>() {
                    let de: Ext4DirEntry = ext4block.read_offset_as(offset);
                    offset += de.entry_len as usize;
                    if de.inode == 0 {
                        continue;
                    }
                    // skip . and ..
                    if de.get_name() == "." || de.get_name() == ".." {
                        continue;
                    }
                    return true;
                }
            }
            // go to next block
            iblock += 1
        }

        false
    }

    pub fn dir_remove(&self, parent: u32, path: &str) -> Result<usize> {
        let mut search_result = Ext4DirSearchResult::new(Ext4DirEntry::default());

        self.dir_find_entry(parent, path, &mut search_result)?;

        let mut parent_inode_ref = self.get_inode_ref(parent);
        let mut child_inode_ref = self.get_inode_ref(search_result.dentry.inode);

        // Use the directory-aware removal path so the parent's link count is
        // decremented when the child directory is removed.
        self.dir_remove_target(&mut parent_inode_ref, &mut child_inode_ref, path)?;

        self.write_back_inode(&mut parent_inode_ref);

        Ok(EOK)
    }

    fn dir_update_dotdot(&self, dir_inode: u32, new_parent_inode: u32) -> Result<usize> {
        let dir_ref = self.get_inode_ref(dir_inode);
        let mut result = Ext4DirSearchResult::new(Ext4DirEntry::default());
        self.dir_find_entry(dir_inode, "..", &mut result)?;

        let mut ext4block = Block::load(&self.block_device, result.pblock_id * BLOCK_SIZE);
        let dotdot: &mut Ext4DirEntry = ext4block.read_offset_as_mut(result.offset);
        dotdot.inode = new_parent_inode;
        self.dir_set_csum(
            &mut ext4block,
            dir_ref.inode_num,
            dir_ref.inode.generation(),
        );
        ext4block.sync_blk_to_disk(&self.block_device);
        Ok(EOK)
    }

    fn dir_remove_target(
        &self,
        parent: &mut Ext4InodeRef,
        target: &mut Ext4InodeRef,
        name: &str,
    ) -> Result<usize> {
        if target.inode.is_dir() {
            if self.dir_has_entry(target.inode_num) {
                return_errno_with_message!(Errno::ENOTEMPTY, "target directory is not empty");
            }
            self.truncate_inode(target, 0)?;
            self.dir_remove_entry(parent, name)?;
            parent
                .inode
                .set_links_count(parent.inode.links_count().saturating_sub(1));
            self.ialloc_free_inode(target.inode_num, true);
            return Ok(EOK);
        }

        self.dir_remove_entry(parent, name)?;
        if target.inode.links_count() > 1 {
            target
                .inode
                .set_links_count(target.inode.links_count() - 1);
            self.write_back_inode(target);
        } else {
            self.truncate_inode(target, 0)?;
            self.ialloc_free_inode(target.inode_num, false);
        }

        Ok(EOK)
    }

    /// Move an existing directory entry to a new parent/name.
    ///
    /// If the destination exists, it is replaced atomically under the same FS
    /// lock. This updates directory link counts and the moved directory's `..`
    /// entry when moving a directory across parents.
    pub fn rename_entry(
        &self,
        old_parent_inode: u32,
        old_name: &str,
        new_parent_inode: u32,
        new_name: &str,
    ) -> Result<usize> {
        let mut search_result = Ext4DirSearchResult::new(Ext4DirEntry::default());
        self.dir_find_entry(old_parent_inode, old_name, &mut search_result)?;

        let child_ino = search_result.dentry.inode;
        let child = self.get_inode_ref(child_ino);

        let mut target_result = Ext4DirSearchResult::new(Ext4DirEntry::default());
        let target_entry = self
            .dir_find_entry(new_parent_inode, new_name, &mut target_result)
            .ok()
            .map(|_| target_result.dentry);

        if let Some(target_entry) = target_entry {
            if target_entry.inode == child_ino {
                return Ok(EOK);
            }

            let mut target = self.get_inode_ref(target_entry.inode);
            if child.inode.is_dir() && !target.inode.is_dir() {
                return_errno_with_message!(Errno::ENOTDIR, "cannot replace non-directory with directory");
            }
            if !child.inode.is_dir() && target.inode.is_dir() {
                return_errno_with_message!(Errno::EISDIR, "cannot replace directory with non-directory");
            }

            if old_parent_inode == new_parent_inode {
                let mut parent = self.get_inode_ref(old_parent_inode);
                self.dir_remove_target(&mut parent, &mut target, new_name)?;
                self.dir_add_entry(&mut parent, &child, new_name)?;
                self.dir_remove_entry(&mut parent, old_name)?;
                self.write_back_inode(&mut parent);
                return Ok(EOK);
            }

            let mut old_parent = self.get_inode_ref(old_parent_inode);
            let mut new_parent = self.get_inode_ref(new_parent_inode);
            self.dir_remove_target(&mut new_parent, &mut target, new_name)?;
            self.dir_add_entry(&mut new_parent, &child, new_name)?;

            if child.inode.is_dir() {
                self.dir_update_dotdot(child_ino, new_parent_inode)?;
                old_parent
                    .inode
                    .set_links_count(old_parent.inode.links_count().saturating_sub(1));
                new_parent
                    .inode
                    .set_links_count(new_parent.inode.links_count() + 1);
            }

            self.dir_remove_entry(&mut old_parent, old_name)?;
            self.write_back_inode(&mut old_parent);
            self.write_back_inode(&mut new_parent);
            return Ok(EOK);
        }

        if old_parent_inode == new_parent_inode {
            let mut parent = self.get_inode_ref(old_parent_inode);
            self.dir_add_entry(&mut parent, &child, new_name)?;
            self.dir_remove_entry(&mut parent, old_name)?;
            self.write_back_inode(&mut parent);
            return Ok(EOK);
        }

        let mut old_parent = self.get_inode_ref(old_parent_inode);
        let mut new_parent = self.get_inode_ref(new_parent_inode);
        self.dir_add_entry(&mut new_parent, &child, new_name)?;

        if child.inode.is_dir() {
            self.dir_update_dotdot(child_ino, new_parent_inode)?;
            old_parent
                .inode
                .set_links_count(old_parent.inode.links_count().saturating_sub(1));
            new_parent
                .inode
                .set_links_count(new_parent.inode.links_count() + 1);
        }

        self.dir_remove_entry(&mut old_parent, old_name)?;
        self.write_back_inode(&mut old_parent);
        self.write_back_inode(&mut new_parent);

        Ok(EOK)
    }
}

pub fn copy_dir_entry_to_array(header: &Ext4DirEntry, array: &mut [u8], offset: usize) {
    unsafe {
        let de_ptr = header as *const Ext4DirEntry as *const u8;
        let array_ptr = array as *mut [u8] as *mut u8;
        let count = core::mem::size_of::<Ext4DirEntry>() / core::mem::size_of::<u8>();
        core::ptr::copy_nonoverlapping(de_ptr, array_ptr.add(offset), count);
    }
}
