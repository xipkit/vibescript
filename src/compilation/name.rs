use super::{Text, Work};
use crate::Result;
use std::{borrow::Borrow, cmp::Ordering, fmt, hash::Hash, ops::Deref};

#[derive(Clone, Default)]
pub(crate) struct Name(Option<Text>);

impl Name {
    /// Copies a syntax name into storage owned by the compiler budget.
    pub fn new(work: &dyn Work, text: &str) -> Result<Self> {
        Self::join(work, &[text])
    }

    /// Assembles a generated name after reserving its complete backing storage.
    pub fn join(work: &dyn Work, pieces: &[&str]) -> Result<Self> {
        if pieces.iter().all(|piece| piece.is_empty()) {
            work.checkpoint()?;
            return Ok(Self::default());
        }
        Ok(Self(Some(Text::join(work, pieces)?)))
    }

    pub fn as_str(&self) -> &str {
        self.0.as_ref().map_or("", Text::as_str)
    }

    /// Transfers a name to compiled metadata, which does not own invocation charges.
    pub fn into_string(self) -> String {
        self.0.map(Text::into_string).unwrap_or_default()
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
    use std::{sync::Arc, time::Instant};

    #[test]
    fn shared_names_release_the_budget_at_the_compiled_boundary() {
        for shared in [false, true] {
            let mut context = CallContext::new(CallOptions::default());
            let memory = context.identity();
            let name = Name::join(&Meter::new(&mut context), &["日本語", "::", "member"]).unwrap();
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
            assert_eq!(Arc::strong_count(&memory) > 1, shared);
            drop(alias);
            assert_eq!(Arc::strong_count(&memory), 1);
            assert_eq!(compiled, "日本語::member");
        }
    }

    #[test]
    fn generated_names_fail_without_releasing_the_original_name() {
        for shortage in [0, 1] {
            let mut context = CallContext::new(CallOptions::default());
            let work = Meter::new(&mut context);
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
            let work = Meter::new(&mut context);
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
