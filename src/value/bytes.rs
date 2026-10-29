use crate::{
    CallContext, Result,
    budget::{Buffer, Charge},
};
use std::{
    mem::ManuallyDrop,
    ops::Deref,
    ptr::NonNull,
    sync::{Arc, OnceLock},
};

/// One strong Arc reference, with the low address bit selecting its layout.
/// Keeping this handle one word preserves the size of every VM value and frame.
pub(crate) struct Bytes(NonNull<()>);
const _: () = assert!(size_of::<Bytes>() == size_of::<usize>());

enum Storage<'a> {
    Owned(&'a Owned),
    Shared(&'a Shared),
}

// SAFETY: the pointer owns an Arc of Owned or Shared, both of which are Send
// and Sync. Bytes only exposes shared references and atomic Arc operations.
unsafe impl Send for Bytes {}
// SAFETY: as above, all access is immutable or synchronized by Arc/OnceLock.
unsafe impl Sync for Bytes {}

#[derive(Debug)]
struct Owned {
    data: Data,
    charge: Option<Charge>,
}

#[derive(Debug)]
struct Shared {
    data: Arc<Vec<u8>>,
    charge: Option<Charge>,
}

impl Bytes {
    #[inline]
    fn owned(value: Owned) -> Self {
        const { assert!(align_of::<Owned>() >= 2) };
        Self(NonNull::new(Arc::into_raw(Arc::new(value)).cast_mut().cast()).unwrap())
    }

    #[inline]
    fn shared_value(value: Shared) -> Self {
        const { assert!(align_of::<Shared>() >= 2) };
        let pointer = NonNull::new(Arc::into_raw(Arc::new(value)).cast_mut().cast()).unwrap();
        Self(pointer.map_addr(|address| address | 1))
    }

    #[inline]
    fn is_shared(&self) -> bool {
        self.0.addr().get() & 1 != 0
    }

    #[inline]
    fn pointer(&self) -> *mut () {
        self.0.as_ptr().map_addr(|address| address & !1)
    }

    #[inline]
    fn storage(&self) -> Storage<'_> {
        // SAFETY: constructors tag only Shared pointers and preserve provenance.
        // This handle keeps the correctly typed Arc allocation alive for self's
        // borrow, and neither layout can change after construction.
        unsafe {
            if self.is_shared() {
                Storage::Shared(&*self.pointer().cast::<Shared>())
            } else {
                Storage::Owned(&*self.pointer().cast::<Owned>())
            }
        }
    }

    #[inline]
    pub(crate) fn ptr_eq(&self, other: &Self) -> bool {
        self.0 == other.0
    }

    #[inline]
    pub(crate) fn strong_count(&self) -> usize {
        // SAFETY: the pointer was produced by Arc::into_raw for this layout.
        // ManuallyDrop borrows its strong reference without consuming it.
        unsafe {
            if self.is_shared() {
                Arc::strong_count(&ManuallyDrop::new(Arc::from_raw(
                    self.pointer().cast::<Shared>(),
                )))
            } else {
                Arc::strong_count(&ManuallyDrop::new(Arc::from_raw(
                    self.pointer().cast::<Owned>(),
                )))
            }
        }
    }

    pub(crate) fn header_bytes() -> usize {
        // Reserve promotion up front: a host can share a result after its
        // creating call has ended, without access to that call's limits.
        size_of::<Owned>() + size_of::<Vec<u8>>() + 4 * size_of::<usize>()
    }

    pub(crate) fn shared_header_bytes() -> usize {
        size_of::<Shared>() + size_of::<Vec<u8>>() + 4 * size_of::<usize>()
    }

    pub(super) fn new(ctx: &mut CallContext, buffer: Buffer<u8>) -> Result<Self> {
        let header = ctx.reserve(Self::header_bytes())?;
        let (data, storage) = buffer.into_parts();
        Ok(Self::from_parts(data, storage, header))
    }

    #[inline]
    pub(crate) fn from_parts(
        data: Vec<u8>,
        mut storage: Option<Charge>,
        header: Option<Charge>,
    ) -> Self {
        Charge::merge(&mut storage, header);
        Self::owned(Owned {
            data: Data::new(data),
            charge: storage,
        })
    }

    pub(crate) fn shared_from_parts(
        data: Vec<u8>,
        mut storage: Option<Charge>,
        header: Option<Charge>,
    ) -> Self {
        Charge::merge(&mut storage, header);
        Self::shared_value(Shared {
            data: Arc::new(data),
            charge: storage,
        })
    }

    pub(super) fn untracked(data: Vec<u8>) -> Self {
        Self::shared_from_parts(data, None, None)
    }

    #[inline]
    pub(crate) fn data(&self) -> &Vec<u8> {
        match self.storage() {
            Storage::Owned(bytes) => &bytes.data,
            Storage::Shared(bytes) => &bytes.data,
        }
    }

    pub(crate) fn shared(&self) -> Arc<Vec<u8>> {
        match self.storage() {
            Storage::Owned(bytes) => bytes.data.shared(),
            Storage::Shared(bytes) => bytes.data.clone(),
        }
    }

    #[inline]
    pub(super) fn charge(&self) -> &Option<Charge> {
        match self.storage() {
            Storage::Owned(bytes) => &bytes.charge,
            Storage::Shared(bytes) => &bytes.charge,
        }
    }

    pub(super) fn compiler_constant(&self) -> Self {
        if self.charge().is_none() {
            return self.clone();
        }
        Self::shared_value(Shared {
            data: self.shared(),
            charge: None,
        })
    }

    /// Shares the backing allocation while keeping each call's charges independent.
    #[inline]
    pub(crate) fn import(ctx: &mut CallContext, bytes: &Self) -> Result<Self> {
        if ctx.owns(bytes.charge()) {
            return Ok(bytes.clone());
        }
        let mut storage = ctx.reserve(bytes.data().capacity())?;
        let header = ctx.reserve(Self::shared_header_bytes())?;
        Charge::merge(&mut storage, header);
        Ok(Self::shared_value(Shared {
            data: bytes.shared(),
            charge: storage,
        }))
    }
}

impl Clone for Bytes {
    #[inline]
    fn clone(&self) -> Self {
        // SAFETY: self keeps the tagged, correctly typed Arc allocation alive.
        // Incrementing its count gives the returned handle its own reference.
        unsafe {
            if self.is_shared() {
                Arc::increment_strong_count(self.pointer().cast::<Shared>());
            } else {
                Arc::increment_strong_count(self.pointer().cast::<Owned>());
            }
        }
        Self(self.0)
    }
}

impl Drop for Bytes {
    #[inline]
    fn drop(&mut self) {
        // SAFETY: each handle owns exactly one strong reference, created by a
        // constructor or Clone. Restore its original type before consuming it.
        unsafe {
            if self.is_shared() {
                drop(Arc::from_raw(self.pointer().cast::<Shared>()));
            } else {
                drop(Arc::from_raw(self.pointer().cast::<Owned>()));
            }
        }
    }
}

impl std::fmt::Debug for Bytes {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.data().fmt(formatter)
    }
}

/// Immutable bytes that acquire a separate shared owner only when they cross budgets.
#[derive(Debug)]
struct Data {
    original: ManuallyDrop<Vec<u8>>,
    shared: OnceLock<Arc<Vec<u8>>>,
}

impl Data {
    fn new(bytes: Vec<u8>) -> Self {
        Self {
            original: ManuallyDrop::new(bytes),
            shared: OnceLock::new(),
        }
    }

    fn shared(&self) -> Arc<Vec<u8>> {
        self.shared
            .get_or_init(|| {
                let mut shared = Arc::new(Vec::new());
                let data = Arc::get_mut(&mut shared).unwrap();
                // SAFETY: the vector is immutable. OnceLock transfers ownership
                // exactly once, and Drop only frees original if this never ran.
                // Allocate first, so nothing can unwind after taking ownership.
                *data = unsafe { std::ptr::read(&*self.original) };
                shared
            })
            .clone()
    }
}

impl Deref for Data {
    type Target = Vec<u8>;

    fn deref(&self) -> &Self::Target {
        &self.original
    }
}

impl Drop for Data {
    #[inline]
    fn drop(&mut self) {
        if self.shared.get().is_none() {
            // SAFETY: no shared owner was created, so original still owns its
            // buffer. Exclusive access rules out concurrent promotion or reads.
            unsafe { ManuallyDrop::drop(&mut self.original) };
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn both_handle_layouts_preserve_identity_and_reference_counts() {
        for original in [
            Bytes::from_parts(vec![42; 1024], None, None),
            Bytes::untracked(vec![42; 1024]),
        ] {
            let address = original.data().as_ptr();
            assert_eq!(original.strong_count(), 1);
            let alias = original.clone();
            assert!(original.ptr_eq(&alias));
            assert_eq!(original.strong_count(), 2);
            let shared = original.shared();
            assert_eq!(shared.as_ptr(), address);
            drop(original);
            assert_eq!(alias.strong_count(), 1);
            drop(alias);
            assert_eq!(shared.as_slice(), &[42; 1024]);
        }
    }

    #[test]
    fn failed_import_releases_its_reservation_without_promoting_storage() {
        for length in [0, 7, 4096] {
            let original = Bytes::from_parts(vec![42; length], None, None);
            let mut options = crate::CallOptions::default();
            options.limits.memory_bytes =
                Some(original.data().capacity() + Bytes::shared_header_bytes() - 1);
            let mut ctx = CallContext::new(options);
            assert_eq!(
                Bytes::import(&mut ctx, &original).unwrap_err().kind,
                crate::ErrorKind::Memory
            );
            assert_eq!(ctx.stats().retained_memory_bytes, 0);
            let Storage::Owned(bytes) = original.storage() else {
                panic!("runtime storage");
            };
            assert!(bytes.data.shared.get().is_none());
            assert_eq!(original.data().as_slice(), vec![42; length]);
        }
    }

    #[test]
    #[cfg(not(target_os = "wasi"))]
    fn concurrent_imports_keep_each_budget_independent() {
        let original = Bytes::from_parts(vec![42; 8192], None, None);
        let address = original.data().as_ptr();
        let imports = std::thread::scope(|scope| {
            let threads: Vec<_> = (0..8)
                .map(|_| {
                    scope.spawn(|| {
                        let mut ctx = CallContext::new(Default::default());
                        let memory = Arc::downgrade(&ctx.identity());
                        let imported = Bytes::import(&mut ctx, &original).unwrap();
                        let alias = Bytes::import(&mut ctx, &imported).unwrap();
                        assert!(alias.ptr_eq(&imported));
                        (alias, memory)
                    })
                })
                .collect();
            threads
                .into_iter()
                .map(|thread| thread.join().unwrap())
                .collect::<Vec<_>>()
        });
        drop(original);
        for (imported, memory) in imports {
            assert!(memory.upgrade().is_some());
            assert_eq!(imported.data().as_ptr(), address);
            assert_eq!(imported.data().as_slice(), &[42; 8192]);
            drop(imported);
            assert!(memory.upgrade().is_none());
        }
    }

    #[test]
    #[cfg(not(target_os = "wasi"))]
    fn concurrent_sharing_preserves_storage_after_the_original_drops() {
        let data = Data::new(vec![42; 8192]);
        let address = data.as_ptr();
        let copies = std::thread::scope(|scope| {
            let threads: Vec<_> = (0..8).map(|_| scope.spawn(|| data.shared())).collect();
            threads
                .into_iter()
                .map(|thread| thread.join().unwrap())
                .collect::<Vec<_>>()
        });
        drop(data);
        for copy in copies {
            assert_eq!(copy.as_ptr(), address);
            assert_eq!(copy.as_slice(), &[42; 8192]);
        }
    }

    #[test]
    fn sharing_empty_and_spare_capacity_buffers_keeps_the_original_alive() {
        for length in [0, 3, 8192] {
            let mut buffer = Vec::with_capacity(length + 64);
            buffer.resize(length, 42);
            let data = Data::new(buffer);
            let address = data.as_ptr();
            let shared = data.shared();
            let copy = data.shared();
            assert_eq!(copy.capacity(), length + 64);
            assert_eq!(copy.as_ptr(), address);
            drop(shared);
            drop(copy);
            assert_eq!(data.as_ptr(), address);
            assert_eq!(data.as_slice(), vec![42; length]);
        }
    }
}
