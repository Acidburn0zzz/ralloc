//! Memory blocks.
//!
//! Blocks are the main unit for the memory bookkeeping. A block is a simple construct with a
//! `Pointer` pointer and a size. Occupied (non-free) blocks are represented by a zero-sized block.

use prelude::*;

use core::{ptr, cmp, mem, fmt};

use ptr;

usize_newtype!(pub Size);

/// A contiguous memory block.
///
/// This provides a number of guarantees,
///
/// 1. The buffer is valid for the block's lifetime, but not necessarily initialized.
/// 2. The Block "owns" the inner data.
/// 3. There is no interior mutability. Mutation requires either mutable access or ownership over
///    the block.
/// 4. The buffer is not aliased. That is, it do not overlap with other blocks or is aliased in any
///    way.
///
/// All this is enforced through the type system. These invariants can only be broken through
/// unsafe code.
///
/// Accessing it through an immutable reference does not break these guarantees. That is, you are
/// not able to read/mutate without acquiring a _mutable_ reference.
#[must_use = "`Block` represents some owned memory, not using it will likely result in memory \
              leaks."]
pub struct Block {
    /// The size of this block, in bytes.
    size: Size,
    /// The pointer to the start of this block.
    ptr: Pointer<u8>,
}

impl Block {
    /// Construct a block from its raw parts (pointer and size).
    #[inline]
    pub unsafe fn from_raw_parts(ptr: Pointer<u8>, size: Size) -> Block {
        Block {
            size: size,
            ptr: ptr,
        }
    }

    /// Create an empty block starting at `ptr`.
    #[inline]
    pub fn empty(ptr: Pointer<u8>) -> Block {
        Block {
            size: 0,
            // This won't alias `ptr`, since the block is empty.
            ptr: ptr,
        }
    }

    /// Create an empty block representing the left edge of this block.
    #[inline]
    pub fn empty_left(&self) -> Block {
        Block::empty(self.ptr.clone())
    }

    /// Create an empty block representing the right edge of this block.
    #[inline]
    pub fn empty_right(&self) -> Block {
        Block {
            size: 0,
            ptr: unsafe {
                // LAST AUDIT: 2016-08-21 (Ticki).

                // By the invariants of this type (the end is addressable), this conversion isn't
                // overflowing.
                self.ptr.clone().offset(self.size as isize)
            },
        }
    }

    /// Merge this block with a block to the right.
    ///
    /// This will simply extend the block, adding the size of the block, and then set the size to
    /// zero. The return value is `Ok(())` on success, and `Err(())` on failure (e.g., the blocks
    /// are not adjacent).
    ///
    /// If you merge with a zero sized block, it will succeed, even if they are not adjacent.
    #[inline]
    pub fn merge_right(&mut self, block: &mut Block) -> Result<(), ()> {
        if self.left_to(block) {
            // Since the end of `block` is bounded by the address space, adding them cannot
            // overflow.
            self.size += block.pop().size;
            // We pop it to make sure it isn't aliased.

            Ok(())
        } else { Err(()) }
    }

    /// Is this block empty/free?
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.size == 0
    }

    /// Get the size of the block.
    #[inline]
    pub fn size(&self) -> Size {
        self.size
    }

    /// Is this block aligned to `align`?
    #[inline]
    pub fn aligned_to(&self, align: ptr::Align) -> bool {
        self.ptr.aligned_to(align)
    }

    /// memcpy the block to another pointer.
    ///
    /// # Panics
    ///
    /// This will panic if the target block is smaller than the source.
    #[inline]
    pub fn copy_to(&self, block: &mut Block) {
        log!(INTERNAL, "Copying {:?} to {:?}", *self, *block);

        // Bound check.
        assert!(self.size <= block.size, "Block too small.");

        unsafe {
            // LAST AUDIT: 2016-08-21 (Ticki).

            // From the invariants of `Block`, this copy is well-defined.
            ptr::copy_nonoverlapping(*self.ptr, *block.ptr, self.size);
        }
    }

    /// Volatile zero this memory if the `security` feature is set.
    pub fn sec_zero(&mut self) {
        use core::intrinsics;

        if cfg!(feature = "security") {
            log!(INTERNAL, "Zeroing {:?}", *self);

            unsafe {
                // LAST AUDIT: 2016-08-21 (Ticki).

                // Since the memory of the block is inaccessible (read-wise), zeroing it is fully
                // safe.
                intrinsics::volatile_set_memory(*self.ptr, 0, self.size);
            }
        }
    }

    /// "Pop" this block.
    ///
    /// This marks it as free, and returns the old value.
    #[inline]
    pub fn pop(&mut self) -> Block {
        unborrow!(mem::replace(self, Block::empty(self.ptr.clone())))
    }

    /// Is this block placed left to the given other block?
    #[inline]
    pub fn left_to(&self, to: &Block) -> bool {
        // Warn about potential confusion of `self` and `to` and other similar bugs.
        if cfg!(debug_assertions) && self >= to {
            log!(WARNING, "{:?} is not lower than {:?}. Are you sure this `left_to()` is correctly \
                 used?", self, to);
        }

        // This won't overflow due to the end being bounded by the address space.
        self.size + *self.ptr as usize == *to.ptr as usize
    }

    /// Split the block at some position.
    ///
    /// # Panics
    ///
    /// Panics if `pos` is out of bound.
    #[inline]
    pub fn split(self, pos: Size) -> (Block, Block) {
        assert!(pos <= self.size, "Split {} out of bound (size is {})!", pos, self.size);

        (
            Block {
                size: pos,
                ptr: self.ptr.clone(),
            },
            Block {
                size: self.size - pos,
                ptr: unsafe {
                    // LAST AUDIT: 2016-08-21 (Ticki).

                    // This won't overflow due to the assertion above, ensuring that it is bounded
                    // by the address space. See the `split_at_mut` source from libcore.
                    self.ptr.offset(pos as isize)
                },
            }
        )
    }

    /// Split this block, such that the second block is aligned to `align`.
    ///
    /// Returns an `None` holding the intact block if `align` is out of bounds. If not
    /// out-of-bounds, `self`'s size is set to zero and a tuple of two blocks (a precursor, which
    /// is there to keep the block aligned, and the block itself, respectively).
    #[inline]
    pub fn align(&mut self, align: ptr::Align) -> Result<(Block, Block), ()> {
        log!(INTERNAL, "Padding {:?} to align {}", self, align);

        // FIXME: This functions suffers from external fragmentation. Leaving bigger segments might
        //        increase performance.

        // Calculate the aligner, which defines the smallest size required as precursor to align
        // the block to `align`.
        // TODO: This can be reduced.
        let aligner = (align.into_usize() - *self.ptr as usize % align.into_usize()) % align.into_usize();
        //                                                       ^^^^^^^^^^^^^^^^^^
        // To avoid wasting space on the case where the block is already aligned, we calculate it
        // modulo `align`.

        // Bound check.
        if aligner < self.size {
            // Invalidate the old block.
            let old = self.pop();

            Some((
                Block {
                    size: Size(aligner),
                    ptr: old.ptr.clone(),
                },
                Block {
                    size: old.size - aligner,
                    ptr: unsafe {
                        // LAST AUDIT: 2016-08-21 (Ticki).

                        // The aligner is bounded by the size, which itself is bounded by the
                        // address space. Therefore, this conversion cannot overflow.
                        old.ptr.offset(aligner as isize)
                    },
                }
            ))
        } else {
            log!(INTERNAL, "Unable to align block.");

            None
        }
    }

    /// Mark this block free to the debugger.
    ///
    /// The debugger might do things like memleak and use-after-free checks. This methods informs
    /// the debugger that this block is freed.
    #[inline]
    pub fn mark_free(self) -> Block {
        #[cfg(feature = "debugger")]
        ::shim::debug::mark_free(*self.ptr, self.size);

        self
    }

    /// Mark this block uninitialized to the debugger.
    ///
    /// To detect use-after-free, the allocator need to mark
    #[inline]
    pub fn mark_uninitialized(self) -> Block {
        #[cfg(feature = "debugger")]
        ::shim::debug::mark_unintialized(*self.ptr, self.size);

        self
    }
}

impl From<Block> for Pointer<u8> {
    fn from(from: Block) -> Pointer<u8> {
        from.ptr
    }
}

/// Compare the blocks address.
impl PartialOrd for Block {
    #[inline]
    fn partial_cmp(&self, other: &Block) -> Option<cmp::Ordering> {
        self.ptr.partial_cmp(&other.ptr)
    }
}

/// Compare the blocks address.
impl Ord for Block {
    #[inline]
    fn cmp(&self, other: &Block) -> cmp::Ordering {
        self.ptr.cmp(&other.ptr)
    }
}

impl cmp::PartialEq for Block {
    #[inline]
    fn eq(&self, other: &Block) -> bool {
        *self.ptr == *other.ptr
    }
}

impl cmp::Eq for Block {}

impl fmt::Debug for Block {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "0x{:x}[{}]", *self.ptr as usize, self.size)
    }
}

impl Drop for Block {
    fn drop(&mut self) {
        debug_assert!(self.is_empty(), "Leaking a non-empty block.");
    }
}

#[cfg(test)]
mod test {
    use super::*;

    use brk;

    /// Implementation we will use for testing.
    impl Block {
        /// Create a new block by extending the program break.
        #[cfg(test)]
        pub fn sbrk(size: Size) -> Block {
            Block::from_raw_parts(brk::lock().sbrk(size.try_into()).unwrap(), size)
        }
    }

    #[test]
    fn array() {
        let block = Block::sbrk(Size(26));

        // Test split.
        let (mut lorem, mut rest) = block.split(Size(5));
        assert_eq!(lorem.size(), 5);
        assert_eq!(lorem.size() + rest.size(), Size(26));
        assert!(lorem < rest);
        assert!(lorem.left_to(&rest));

        assert_eq!(lorem, lorem);
        assert!(!rest.is_empty());
        assert!(lorem.align(2).unwrap().1.aligned_to(2));
        assert!(rest.align(15).unwrap().1.aligned_to(15));
        assert_eq!(*Pointer::from(lorem) as usize + 5, *Pointer::from(rest) as usize);
    }

    #[test]
    fn merge() {
        let block = Block::sbrk(26);

        let (mut lorem, mut rest) = block.split(Size(5));
        lorem.merge_right(&mut rest).unwrap();

        let mut tmp = rest.split(0).0;
        assert!(tmp.is_empty());
        lorem.split(2).0.merge_right(&mut tmp).unwrap();
    }

    #[test]
    #[should_panic]
    fn oob() {
        // Test OOB.
        Block::sbrk(5).split(6);
    }

    #[test]
    fn mutate() {
        let mut arr = [0u8, 2, 0, 0, 255, 255];

        let block = unsafe {
            Block::from_raw_parts(Pointer::new(&mut arr[0]), Size(6))
        };

        let (a, mut b) = block.split(Size(2));
        a.copy_to(&mut b);
        assert_eq!(a.size(), Size(2));

        assert_eq!(arr, [0, 2, 0, 2, 255, 255]);
    }

    #[test]
    fn empty_lr() {
        let block = Block::sbrk(Size(26));

        assert!(block.empty_left().is_empty());
        assert!(block.empty_right().is_empty());
        assert_eq!(*Pointer::from(block.empty_left()), arr.as_ptr());
        assert_eq!(block.empty_right(), block.split(arr.len()).1);
    }

    #[test]
    fn empty() {
        let mut x = 3;
        assert!(Block::empty(Pointer::from(&mut x)).is_emty());
        assert_eq!(Block::empty(Pointer::from(&mut x)).size(), Size(0));
    }
}
