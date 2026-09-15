use super::*;
use crate::{CallOptions, Limits};

#[test]
fn excessive_literal_patterns_stop_before_allocating_normalized_output() {
    let mut ctx = CallContext::new(CallOptions::default());
    let pattern = vec![b'x'; 4 * LIMIT];
    let error = format(&mut ctx, &pattern, &[]).unwrap_err();
    assert_eq!(error.kind, ErrorKind::OutputLimit);
    assert_eq!(ctx.stats().retained_memory_bytes, 0);
    assert!(ctx.stats().peak_memory_bytes < 1024);
    assert_eq!(
        format(&mut ctx, b"ok", &[]).unwrap().as_bytes(),
        Some(&b"ok"[..])
    );
}

#[test]
fn precision_stops_shared_composites_before_expanding_large_payloads() {
    let mut value = Value::bytes(vec![b'x'; LIMIT + 1]);
    for _ in 0..24 {
        value = Value::array(vec![value.clone(), value]);
    }
    for (pattern, expected) in [
        (b"%.1s", &b"["[..]),
        (b"%.1v", &b"["[..]),
        (b"%.1q", &b"\"[\""[..]),
    ] {
        let mut ctx = CallContext::new(CallOptions {
            limits: Limits {
                steps: Some(5000),
                memory_bytes: Some(4096),
                ..Limits::default()
            },
            ..CallOptions::default()
        });
        let result = format(&mut ctx, pattern, std::slice::from_ref(&value)).unwrap();
        assert_eq!(result.as_bytes(), Some(expected));
        drop(result);
        assert_eq!(ctx.stats().retained_memory_bytes, 0);
    }
}

#[test]
fn padding_reservations_latch_memory_exhaustion_and_release_scratch() {
    let mut ctx = CallContext::new(CallOptions {
        limits: Limits {
            memory_bytes: Some(4096),
            ..Limits::default()
        },
        ..CallOptions::default()
    });
    let error = format(&mut ctx, b"%1000000s", &[Value::bytes(b"")]).unwrap_err();
    assert_eq!(error.kind, ErrorKind::Memory);
    assert_eq!(ctx.stats().retained_memory_bytes, 0);
    assert!(ctx.stats().peak_memory_bytes < 4096);
    assert_eq!(ctx.charge(0).unwrap_err().kind, ErrorKind::Memory);
}

#[test]
fn malformed_placeholder_expansion_checks_actual_size_before_allocation() {
    let mut ctx = CallContext::new(CallOptions::default());
    let error = format(&mut ctx, b"%1048576k", &[Value::int(7)]).unwrap_err();
    assert_eq!(error.kind, ErrorKind::OutputLimit);
    assert_eq!(ctx.stats().retained_memory_bytes, 0);
    assert!(ctx.stats().peak_memory_bytes < 4096);
    assert_eq!(
        format(&mut ctx, b"%d", &[Value::int(7)])
            .unwrap()
            .as_bytes(),
        Some(&b"7"[..])
    );
}
