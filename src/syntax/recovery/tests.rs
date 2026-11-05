use super::*;

const SOURCE: &str = "x = )\ny = ]\n";
const INTERNAL: &str = "injected replay invariant failure";

struct Fault {
    charges: AtomicUsize,
    at: usize,
    kind: ErrorKind,
    quota: Option<ErrorKind>,
}

impl Fault {
    fn new(at: usize, kind: ErrorKind, quota: Option<ErrorKind>) -> Self {
        Self {
            charges: AtomicUsize::new(0),
            at,
            kind,
            quota,
        }
    }
}

impl Work for Fault {
    fn charge(&self, _: usize) -> Result<()> {
        if self.charges.fetch_add(1, Ordering::Relaxed) == self.at {
            return Err(Error::new(self.kind, INTERNAL));
        }
        Ok(())
    }

    fn bytes(&self, _: usize) -> Result<()> {
        Ok(())
    }

    fn checkpoint(&self) -> Result<()> {
        if self.charges.load(Ordering::Relaxed) > self.at {
            if let Some(kind) = self.quota {
                return Err(Error::new(kind, "caller quota exhausted"));
            }
        }
        Ok(())
    }

    fn reserve(&self, _: usize) -> Result<Option<crate::budget::Charge>> {
        Ok(None)
    }

    fn allocation_error(&self, message: &str) -> Error {
        ().allocation_error(message)
    }
}

fn first() -> Error {
    let error = parse(SOURCE, &()).err().unwrap();
    assert_eq!(error.kind, ErrorKind::Syntax);
    error
}

fn tokens(reuse: bool) -> Option<Tokens<'static>> {
    if reuse {
        parser(SOURCE, &()).unwrap().tokens.original()
    } else {
        None
    }
}

// Measure construction separately so the replay fault fires inside run(),
// after construction, and only once. A later caller checkpoint succeeds.
fn construction_charges(reuse: bool) -> usize {
    let caller = Fault::new(usize::MAX, ErrorKind::Internal, None);
    let work = RecoveryWork {
        remaining: AtomicUsize::new(usize::MAX),
        caller: &caller,
    };
    let mode = super::super::Mode {
        lexical: true,
        ..Default::default()
    };
    let parser = match tokens(reuse) {
        Some(tokens) => parser_from_tokens(SOURCE, &work, tokens, mode),
        None => parser_in(SOURCE, &work, mode),
    };
    assert!(parser.is_ok());
    caller.charges.load(Ordering::Relaxed)
}

#[test]
fn preserves_internal_from_construction_and_replay() {
    for reuse in [false, true] {
        for at in [0, construction_charges(reuse)] {
            let caller = Fault::new(at, ErrorKind::Internal, None);
            let error = diagnostics_with_tokens(
                SOURCE,
                first(),
                &caller,
                tokens(reuse),
                Default::default(),
            );
            assert_eq!(error.kind, ErrorKind::Internal, "reuse={reuse}, at={at}");
            assert_eq!(error.message, INTERNAL);
            assert_eq!(caller.charges.load(Ordering::Relaxed), at + 1);
            caller.checkpoint().unwrap();
        }
    }
}

#[test]
fn host_syntax_returns_internal_from_replay() {
    let baseline = Fault::new(usize::MAX, ErrorKind::Internal, None);
    let mode = super::super::Mode {
        canonical: true,
        ..Default::default()
    };
    assert_eq!(
        parse_in(SOURCE, &baseline, mode).err().unwrap().kind,
        ErrorKind::Syntax
    );
    let at = baseline.charges.load(Ordering::Relaxed) + construction_charges(true);
    let caller = Fault::new(at, ErrorKind::Internal, None);
    let error = host_syntax(SOURCE, &caller, first());
    assert_eq!(error.kind, ErrorKind::Internal);
    assert_eq!(error.message, INTERNAL);
    assert_eq!(caller.charges.load(Ordering::Relaxed), at + 1);
    caller.checkpoint().unwrap();
}

#[test]
fn caller_quotas_take_precedence_over_replay_internal() {
    for reuse in [false, true] {
        for at in [0, construction_charges(reuse)] {
            for quota in [
                ErrorKind::Steps,
                ErrorKind::Memory,
                ErrorKind::Deadline,
                ErrorKind::Cancelled,
            ] {
                let caller = Fault::new(at, ErrorKind::Internal, Some(quota));
                let error = diagnostics_with_tokens(
                    SOURCE,
                    first(),
                    &caller,
                    tokens(reuse),
                    Default::default(),
                );
                assert_eq!(error.kind, quota, "reuse={reuse}, at={at}");
                assert_eq!(error.message, "caller quota exhausted");
            }
        }
    }
}

#[test]
fn transient_steps_failure_keeps_syntax_diagnostics() {
    for reuse in [false, true] {
        for at in [0, construction_charges(reuse)] {
            let caller = Fault::new(at, ErrorKind::Steps, None);
            let error = diagnostics_with_tokens(
                SOURCE,
                first(),
                &caller,
                tokens(reuse),
                Default::default(),
            );
            assert_eq!(error.kind, ErrorKind::Syntax);
            assert_eq!(error.message, first().message);
            caller.checkpoint().unwrap();
        }
    }
}
