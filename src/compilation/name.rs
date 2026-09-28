use super::{Buffer, Work};
use crate::{Result, budget::Charge};
use std::{borrow::Borrow, cmp::Ordering, fmt, hash::Hash, ops::Deref, sync::Arc};

struct OwnedName {
    text: String,
    _charge: Option<Charge>,
}

#[derive(Clone, Default)]
pub(crate) struct Name(Option<Arc<OwnedName>>);

impl Name {
    /// Copies a syntax name into storage owned by the compiler budget.
    pub fn new(work: &dyn Work, text: &str) -> Result<Self> {
        Self::join(work, &[text])
    }

    /// Assembles a generated name after reserving its complete backing storage.
    pub fn join(work: &dyn Work, pieces: &[&str]) -> Result<Self> {
        work.checkpoint()?;
        let length = pieces.iter().try_fold(0usize, |length, piece| {
            length
                .checked_add(piece.len())
                .ok_or_else(|| work.allocation_error("compiler name size overflow"))
        })?;
        if length == 0 {
            return Ok(Self::default());
        }
        let mut charge = work.reserve(size_of::<OwnedName>() + 2 * size_of::<usize>())?;
        let mut bytes = Buffer::with_capacity(work, length)?;
        for piece in pieces {
            bytes.extend_from_slice(work, piece.as_bytes())?;
        }
        work.bytes(length)?;
        let (bytes, storage) = bytes.into_parts();
        Charge::merge(&mut charge, storage);
        Ok(Self(Some(Arc::new(OwnedName {
            text: String::from_utf8(bytes).expect("names contain valid UTF-8"),
            _charge: charge,
        }))))
    }

    pub fn as_str(&self) -> &str {
        self.0.as_ref().map_or("", |name| name.text.as_str())
    }

    /// Transfers a name to compiled metadata, which does not own invocation charges.
    pub fn into_string(self) -> String {
        match self.0 {
            Some(name) => match Arc::try_unwrap(name) {
                Ok(name) => name.text,
                Err(name) => name.text.clone(),
            },
            None => String::new(),
        }
    }
}

impl Deref for Name {
    type Target = str;

    fn deref(&self) -> &str {
        self.as_str()
    }
}

impl AsRef<str> for Name {
    fn as_ref(&self) -> &str {
        self.as_str()
    }
}

impl Borrow<str> for Name {
    fn borrow(&self) -> &str {
        self.as_str()
    }
}

impl<T: AsRef<str> + ?Sized> PartialEq<T> for Name {
    fn eq(&self, other: &T) -> bool {
        self.as_str() == other.as_ref()
    }
}

impl Eq for Name {}

impl PartialOrd for Name {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for Name {
    fn cmp(&self, other: &Self) -> Ordering {
        self.as_str().cmp(other.as_str())
    }
}

impl Hash for Name {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        self.as_str().hash(state);
    }
}

impl fmt::Debug for Name {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.as_str().fmt(formatter)
    }
}

impl fmt::Display for Name {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{CallContext, CallOptions, ErrorKind, compilation::Meter};
    use std::{cell::RefCell, time::Instant};

    #[test]
    fn shared_names_release_the_budget_at_the_compiled_boundary() {
        for shared in [false, true] {
            let mut context = CallContext::new(CallOptions::default());
            let memory = Arc::downgrade(&context.identity());
            let name = Name::join(
                &Meter(RefCell::new(&mut context)),
                &["日本語", "::", "member"],
            )
            .unwrap();
            let retained = context.stats().retained_memory_bytes;
            assert!(retained > name.len());
            let alias = shared.then(|| name.clone());
            let compiled = name.into_string();
            assert_eq!(compiled, "日本語::member");
            assert_eq!(
                context.stats().retained_memory_bytes,
                if shared { retained } else { 0 }
            );
            drop(context);
            assert_eq!(memory.upgrade().is_some(), shared);
            drop(alias);
            assert!(memory.upgrade().is_none());
            assert_eq!(compiled, "日本語::member");
        }
    }

    #[test]
    fn generated_names_fail_without_releasing_the_original_name() {
        for shortage in [0, 1] {
            let mut context = CallContext::new(CallOptions::default());
            let work = Meter(RefCell::new(&mut context));
            let original = Name::new(&work, "member").unwrap();
            let before = work.0.borrow().stats().retained_memory_bytes;
            let generated = Name::join(&work, &["Outer::", &original, "="]).unwrap();
            let additional = work.0.borrow().stats().retained_memory_bytes - before;
            drop(generated);
            work.0.borrow_mut().options.limits.memory_bytes = Some(before + additional - shortage);
            let result = Name::join(&work, &["Outer::", &original, "="]);
            if shortage == 0 {
                assert_eq!(result.as_ref().unwrap(), "Outer::member=");
            } else {
                assert_eq!(result.as_ref().unwrap_err().kind, ErrorKind::Memory);
                assert_eq!(work.checkpoint().unwrap_err().kind, ErrorKind::Memory);
            }
            drop(result);
            assert_eq!(original, "member");
            assert_eq!(work.0.borrow().stats().retained_memory_bytes, before);
            drop(original);
            assert_eq!(work.0.borrow().stats().retained_memory_bytes, 0);
        }
    }

    #[test]
    fn even_empty_names_preserve_cancellation_and_deadlines() {
        for deadline in [false, true] {
            let mut context = CallContext::new(CallOptions::default());
            if deadline {
                context.options.deadline = Some(Instant::now());
            } else {
                context.cancellation().cancel();
            }
            let work = Meter(RefCell::new(&mut context));
            let kind = if deadline {
                ErrorKind::Deadline
            } else {
                ErrorKind::Cancelled
            };
            for name in ["", "name"] {
                assert_eq!(Name::new(&work, name).unwrap_err().kind, kind);
                assert_eq!(work.0.borrow().stats().retained_memory_bytes, 0);
            }
        }
    }
}
