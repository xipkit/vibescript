/// Makes an engine invariant failure crash a harness just as a panic would,
/// while leaving ordinary source errors available to the caller.
pub fn check<T>(result: vibescript::Result<T>) -> vibescript::Result<T> {
    if let Err(error) = &result {
        assert_ne!(
            error.kind,
            vibescript::ErrorKind::Internal,
            "internal engine error: {error}"
        );
    }
    result
}

#[cfg(test)]
mod tests {
    use super::check;
    use vibescript::{Error, ErrorKind};

    #[test]
    fn source_errors_are_not_crashes() {
        assert_eq!(check(Ok(42)).unwrap(), 42);
        let error = Error::new(ErrorKind::Syntax, "invalid source");
        assert_eq!(check::<()>(Err(error)).unwrap_err().kind, ErrorKind::Syntax);
    }

    #[test]
    #[should_panic(expected = "internal engine error: forced invariant failure")]
    fn internal_errors_crash() {
        let _ = check::<()>(Err(Error::new(
            ErrorKind::Internal,
            "forced invariant failure",
        )));
    }
}
