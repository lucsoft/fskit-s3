//! `FSItem` subclass that carries the absolute path (plus kind/size) FSKit hands
//! back to us on later operations.

use objc2::rc::Retained;
use objc2::runtime::NSObjectProtocol;
use objc2::{define_class, msg_send, AllocAnyThread, DefinedClass};

use crate::sys::{FSItem, FSItemID, FS_ITEM_ID_ROOT_DIRECTORY};

/// Per-item state stored on the ObjC instance.
pub struct ItemIvars {
    pub path: String,
    pub is_dir: bool,
    pub size: u64,
    pub item_id: FSItemID,
}

define_class!(
    #[unsafe(super(FSItem))]
    #[name = "FSKitS3Item"]
    #[ivars = ItemIvars]
    pub struct S3Item;

    unsafe impl NSObjectProtocol for S3Item {}
);

// SAFETY: an `S3Item` is immutable after construction — its ivars (`path`, `is_dir`,
// `size`, `item_id`) are set once in `new` and only ever read — and it is only ever
// messaged for `init`/`retain`/`release` (all thread-safe), never a main-thread-only
// API. So a `Retained<S3Item>` is sound to move and share across the FSKit worker
// threads and to hold in the volume's item cache.
unsafe impl Send for S3Item {}
unsafe impl Sync for S3Item {}

impl S3Item {
    /// Create an item for `path`. The root (`"/"`) uses the reserved root id.
    pub fn new(path: String, is_dir: bool, size: u64) -> Retained<Self> {
        let item_id = item_id_for(&path);
        let this = Self::alloc().set_ivars(ItemIvars {
            path,
            is_dir,
            size,
            item_id,
        });
        unsafe { msg_send![super(this), init] }
    }

    pub fn path(&self) -> &str {
        &self.ivars().path
    }

    pub fn is_dir(&self) -> bool {
        self.ivars().is_dir
    }

    pub fn size(&self) -> u64 {
        self.ivars().size
    }

    pub fn item_id(&self) -> FSItemID {
        self.ivars().item_id
    }
}

/// Stable item id for a path: the reserved id for the root, otherwise an FNV-1a
/// hash with the top bit set so it can never collide with FSKit's reserved
/// low values (invalid=0, parent-of-root=1, root=2).
pub fn item_id_for(path: &str) -> FSItemID {
    if path == "/" {
        return FS_ITEM_ID_ROOT_DIRECTORY;
    }
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in path.bytes() {
        h ^= b as u64;
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    h | (1 << 63)
}
