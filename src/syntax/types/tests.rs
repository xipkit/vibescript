use super::*;
use crate::{Error, ErrorKind, budget::Charge, compilation::Work};
use std::cell::Cell;

struct FailedInvariant(Cell<bool>);

impl Work for FailedInvariant {
    fn charge(&self, _: usize) -> Result<()> {
        Ok(())
    }

    fn bytes(&self, bytes: usize) -> Result<()> {
        // Inject a one-shot invariant failure while bump reads the type name.
        // A later checkpoint deliberately cannot recover it from a quota latch.
        if bytes == "Invariant".len() && !self.0.replace(true) {
            return Err(crate::checked::violation("injected type-name invariant").into());
        }
        Ok(())
    }

    fn checkpoint(&self) -> Result<()> {
        Ok(())
    }

    fn reserve(&self, _: usize) -> Result<Option<Charge>> {
        Ok(None)
    }

    fn allocation_error(&self, message: &str) -> Error {
        ().allocation_error(message)
    }
}

fn preserves_invariant(source: &str, probe: impl FnOnce(&mut Parser<'_>) -> Result<()>) {
    let mut parser = crate::syntax::parser(source, &()).unwrap();
    parser
        .type_names
        .insert(&(), Name::new(&(), "Invariant").unwrap(), ())
        .unwrap();
    let work = FailedInvariant(Cell::new(false));
    parser.work = &work;
    let error = probe(&mut parser).unwrap_err();
    assert!(
        work.0.get(),
        "the speculative parse did not reach the invariant"
    );
    assert_eq!(error.kind, ErrorKind::Internal);
    assert!(
        error.message.ends_with("injected type-name invariant"),
        "{error}"
    );
}

#[test]
fn argument_preserves_internal_error() {
    preserves_invariant("Invariant)", |p| p.argument_type_literal(false).map(|_| ()));
}

#[test]
fn tuple_argument_preserves_internal_error() {
    preserves_invariant("[Invariant])", |p| {
        p.argument_type_literal(true).map(|_| ())
    });
}

#[test]
fn hash_candidate_preserves_internal_error() {
    preserves_invariant("field: Invariant}", |p| {
        p.hash_type_candidate(false).map(|_| ())
    });
}

#[test]
fn block_union_preserves_internal_error() {
    preserves_invariant("| Invariant|", |p| p.block_type_continues(1).map(|_| ()));
}

#[test]
fn shape_default_preserves_internal_error() {
    preserves_invariant("{field: Invariant}", |p| {
        p.keyword_default(true).map(|_| ())
    });
}

#[test]
fn tuple_default_preserves_internal_error() {
    preserves_invariant("[Invariant]", |p| p.keyword_default(true).map(|_| ()));
}

#[test]
fn typed_local_preserves_internal_error() {
    preserves_invariant("value: Invariant = 1", |p| {
        p.typed_local_ahead().map(|_| ())
    });
}

#[test]
fn class_var_preserves_internal_error() {
    preserves_invariant("@@value: Invariant = 1", |p| {
        p.class_var_ahead().map(|_| ())
    });
}

#[test]
fn full_parse_preserves_internal_error() {
    let source = "f(Invariant)";
    let mut parser = crate::syntax::parser(source, &()).unwrap();
    let work = FailedInvariant(Cell::new(false));
    parser.work = &work;
    let parsing = Parsing::<crate::syntax::recovery::FailFast>::new(parser);
    let error = parsing.run(crate::syntax::Call::Program).err().unwrap();
    assert!(work.0.get());
    assert_eq!(error.kind, ErrorKind::Internal);
    assert!(error.message.ends_with("injected type-name invariant"));
}
