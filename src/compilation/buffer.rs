use super::Work;
use crate::{Result, budget::Charge};
use std::{
    fmt,
    ops::{Deref, DerefMut},
};

pub(crate) struct Buffer<T> {
    data: Vec<T>,
    charge: Option<Charge>,
}

impl<T> Default for Buffer<T> {
    fn default() -> Self {
        Self::new()
    }
}

impl<T> Buffer<T> {
    pub fn new() -> Self {
        Self {
            data: Vec::new(),
            charge: None,
        }
    }

    pub fn with_capacity(work: &dyn Work, capacity: usize) -> Result<Self> {
        let mut result = Self::new();
        result.ensure(work, capacity)?;
        Ok(result)
    }

    pub fn into_parts(self) -> (Vec<T>, Option<Charge>) {
        (self.data, self.charge)
    }

    pub fn as_slice(&self) -> &[T] {
        &self.data
    }

    pub fn from_array<const N: usize>(work: &dyn Work, values: [T; N]) -> Result<Self> {
        let mut result = Self::with_capacity(work, N)?;
        for value in values {
            result.push(work, value)?;
        }
        Ok(result)
    }

    pub fn insert(&mut self, work: &dyn Work, index: usize, value: T) -> Result<()> {
        work.bytes(std::mem::size_of_val(&self.data[index..]))?;
        self.push(work, value)?;
        self.data[index..].rotate_right(1);
        Ok(())
    }

    fn ensure(&mut self, work: &dyn Work, capacity: usize) -> Result<()> {
        work.checkpoint()?;
        if capacity <= self.data.capacity() {
            return Ok(());
        }
        let bytes = capacity
            .checked_mul(size_of::<T>())
            .ok_or_else(|| work.allocation_error("compiler allocation size overflow"))?;
        let mut charge = work.reserve(bytes)?;
        work.bytes(std::mem::size_of_val(self.data.as_slice()))?;
        let mut data = Vec::new();
        data.try_reserve_exact(capacity)
            .map_err(|_| work.allocation_error("compiler allocation failed"))?;
        if data.capacity() > capacity {
            let extra = (data.capacity() - capacity)
                .checked_mul(size_of::<T>())
                .ok_or_else(|| work.allocation_error("compiler allocation size overflow"))?;
            Charge::merge(&mut charge, work.reserve(extra)?);
        }
        data.append(&mut self.data);
        self.data = data;
        self.charge = charge;
        Ok(())
    }

    pub fn push(&mut self, work: &dyn Work, value: T) -> Result<()> {
        work.checkpoint()?;
        if self.data.len() == self.data.capacity() {
            let capacity = self
                .data
                .capacity()
                .max(4)
                .checked_mul(2)
                .ok_or_else(|| work.allocation_error("compiler allocation size overflow"))?;
            self.ensure(work, capacity)?;
        }
        self.data.push(value);
        Ok(())
    }

    pub fn pop(&mut self) -> Option<T> {
        self.data.pop()
    }

    pub fn truncate(&mut self, length: usize) {
        self.data.truncate(length);
    }

    pub fn remove(&mut self, index: usize) -> T {
        self.data.remove(index)
    }

    pub fn extend(&mut self, work: &dyn Work, values: Self) -> Result<()> {
        let capacity = self
            .data
            .len()
            .checked_add(values.len())
            .ok_or_else(|| work.allocation_error("compiler allocation size overflow"))?;
        self.ensure(work, capacity)?;
        for value in values {
            work.charge(1)?;
            self.data.push(value);
        }
        Ok(())
    }

    pub fn copy_with(
        &self,
        work: &dyn Work,
        mut copy: impl FnMut(&T) -> Result<T>,
    ) -> Result<Self> {
        let mut result = Self::new();
        result.ensure(work, self.len())?;
        for value in &self.data {
            work.charge(1)?;
            result.data.push(copy(value)?);
        }
        Ok(result)
    }
}

impl<T: Copy> Buffer<T> {
    pub fn from_slice(work: &dyn Work, values: &[T]) -> Result<Self> {
        let mut result = Self::with_capacity(work, values.len())?;
        work.bytes(std::mem::size_of_val(values))?;
        result.data.extend_from_slice(values);
        Ok(result)
    }

    pub fn extend_from_slice(&mut self, work: &dyn Work, values: &[T]) -> Result<()> {
        let capacity = self
            .data
            .len()
            .checked_add(values.len())
            .ok_or_else(|| work.allocation_error("compiler allocation size overflow"))?;
        if capacity > self.data.capacity() {
            let growth = self.data.capacity().checked_mul(2).unwrap_or(capacity);
            self.ensure(work, capacity.max(growth).max(8))?;
        }
        work.bytes(std::mem::size_of_val(values))?;
        self.data.extend_from_slice(values);
        Ok(())
    }
}

impl crate::shapes::TypeWriter for (&dyn Work, &mut Buffer<u8>) {
    fn write(&mut self, bytes: &[u8]) -> Result<()> {
        for chunk in bytes.chunks(4096) {
            self.1.extend_from_slice(self.0, chunk)?;
        }
        Ok(())
    }

    fn node(&mut self) -> Result<()> {
        self.0.charge(1)
    }
}

impl<T> Deref for Buffer<T> {
    type Target = [T];

    fn deref(&self) -> &[T] {
        &self.data
    }
}

impl<T> DerefMut for Buffer<T> {
    fn deref_mut(&mut self) -> &mut [T] {
        &mut self.data
    }
}

impl<T: fmt::Debug> fmt::Debug for Buffer<T> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.data.fmt(formatter)
    }
}

impl<T: PartialEq> PartialEq for Buffer<T> {
    fn eq(&self, other: &Self) -> bool {
        self.data == other.data
    }
}

impl<T: PartialEq> Buffer<T> {
    pub fn dedup(&mut self) {
        self.data.dedup();
    }
}

impl<'a, T> IntoIterator for &'a Buffer<T> {
    type Item = &'a T;
    type IntoIter = std::slice::Iter<'a, T>;

    fn into_iter(self) -> Self::IntoIter {
        self.data.iter()
    }
}

impl<'a, T> IntoIterator for &'a mut Buffer<T> {
    type Item = &'a mut T;
    type IntoIter = std::slice::IterMut<'a, T>;

    fn into_iter(self) -> Self::IntoIter {
        self.data.iter_mut()
    }
}

pub(crate) struct IntoIter<T> {
    values: std::vec::IntoIter<T>,
    _charge: Option<Charge>,
}

impl<T> Iterator for IntoIter<T> {
    type Item = T;

    fn next(&mut self) -> Option<T> {
        self.values.next()
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        self.values.size_hint()
    }
}

impl<T> ExactSizeIterator for IntoIter<T> {}

impl<T> IntoIterator for Buffer<T> {
    type Item = T;
    type IntoIter = IntoIter<T>;

    fn into_iter(self) -> Self::IntoIter {
        IntoIter {
            values: self.data.into_iter(),
            _charge: self.charge,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{CallContext, CallOptions, Error, ErrorKind, compilation::Meter};
    use std::{cell::RefCell, time::Instant};

    #[test]
    fn growth_reserves_old_and_new_storage_before_mutating() {
        for short in [false, true] {
            let mut options = CallOptions::default();
            options.limits.memory_bytes = Some(size_of::<[usize; 24]>() - usize::from(short));
            let mut context = CallContext::new(options);
            let work = Meter(RefCell::new(&mut context));
            let mut buffer = Buffer::new();
            for value in 0..8usize {
                buffer.push(&work, value).unwrap();
            }
            let original = buffer.as_ptr();
            let result = buffer.push(&work, 8);
            if short {
                assert_eq!(result.unwrap_err().kind, ErrorKind::Memory);
                assert_eq!(&*buffer, &[0, 1, 2, 3, 4, 5, 6, 7]);
                assert_eq!(buffer.as_ptr(), original);
                assert_eq!(
                    work.0.borrow().stats().peak_memory_bytes,
                    size_of::<[usize; 8]>()
                );
                assert_eq!(work.checkpoint().unwrap_err().kind, ErrorKind::Memory);
            } else {
                result.unwrap();
                assert_eq!(buffer.len(), 9);
                assert_eq!(
                    work.0.borrow().stats().peak_memory_bytes,
                    size_of::<[usize; 24]>()
                );
                assert_eq!(
                    work.0.borrow().stats().retained_memory_bytes,
                    size_of::<[usize; 16]>()
                );
            }
            drop(buffer);
            assert_eq!(work.0.borrow().stats().retained_memory_bytes, 0);
        }
    }

    #[test]
    fn owned_iteration_keeps_the_allocation_charged_until_drop() {
        let mut context = CallContext::new(CallOptions::default());
        let work = Meter(RefCell::new(&mut context));
        let mut buffer = Buffer::new();
        buffer.push(&work, 1usize).unwrap();
        buffer.push(&work, 2).unwrap();
        let bytes = work.0.borrow().stats().retained_memory_bytes;
        let mut values = buffer.into_iter();
        assert_eq!(values.next(), Some(1));
        assert_eq!(work.0.borrow().stats().retained_memory_bytes, bytes);
        assert_eq!(values.len(), 1);
        drop(values);
        assert_eq!(work.0.borrow().stats().retained_memory_bytes, 0);
    }

    #[test]
    fn failed_nested_copies_release_only_new_allocations() {
        let mut context = CallContext::new(CallOptions::default());
        let work = Meter(RefCell::new(&mut context));
        let mut source = Buffer::new();
        for value in 1..=2u8 {
            let mut row = Buffer::new();
            row.push(&work, value).unwrap();
            source.push(&work, row).unwrap();
        }
        let before = work.0.borrow().stats().retained_memory_bytes;
        let mut copies = 0;
        let result = source.copy_with(&work, |row| {
            copies += 1;
            if copies == 2 {
                return Err(Error::new(ErrorKind::Runtime, "stop copying"));
            }
            row.copy_with(&work, |value| Ok(*value))
        });
        assert_eq!(result.unwrap_err().kind, ErrorKind::Runtime);
        assert_eq!(work.0.borrow().stats().retained_memory_bytes, before);
        assert_eq!(&*source[0], &[1]);
        assert_eq!(&*source[1], &[2]);
        drop(source);
        assert_eq!(work.0.borrow().stats().retained_memory_bytes, 0);
    }

    #[test]
    fn repeated_small_extensions_keep_copy_work_and_capacity_bounded() {
        let mut context = CallContext::new(CallOptions::default());
        let work = Meter(RefCell::new(&mut context));
        let mut bytes = Buffer::new();
        for _ in 0..1024 {
            bytes.extend_from_slice(&work, "日".as_bytes()).unwrap();
        }
        assert_eq!(bytes.len(), 3072);
        assert!(work.0.borrow().stats().steps < 4096);
        assert!(work.0.borrow().stats().peak_memory_bytes <= 3 * bytes.len());
        drop(bytes);
        assert_eq!(work.0.borrow().stats().retained_memory_bytes, 0);
    }

    #[test]
    fn allocation_failures_preserve_stop_priority_and_unlimited_tracking() {
        for kind in [ErrorKind::Memory, ErrorKind::Cancelled, ErrorKind::Deadline] {
            let mut options = CallOptions::default();
            if kind == ErrorKind::Deadline {
                options.deadline = Some(Instant::now());
            }
            let mut context = CallContext::new(options);
            if kind == ErrorKind::Cancelled {
                context.cancellation().cancel();
            }
            let work = Meter(RefCell::new(&mut context));
            let mut buffer = Buffer::<usize>::new();
            assert_eq!(buffer.ensure(&work, usize::MAX).unwrap_err().kind, kind);
            assert_eq!(work.checkpoint().unwrap_err().kind, kind);
            assert_eq!(work.0.borrow().stats().retained_memory_bytes, 0);
        }
        let mut options = CallOptions::default();
        options.limits.memory_bytes = None;
        let mut context = CallContext::new(options);
        let work = Meter(RefCell::new(&mut context));
        let mut buffer = Buffer::new();
        for value in 0..32usize {
            buffer.push(&work, value).unwrap();
        }
        assert!(buffer.charge.is_some());
        assert!(work.0.borrow().stats().peak_memory_bytes >= 32 * size_of::<usize>());
        drop(buffer);
        assert_eq!(work.0.borrow().stats().retained_memory_bytes, 0);
    }
}
