use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};
use vibescript::{CallOptions, CancellationToken, Engine, ErrorKind, Value};

#[test]
fn unary_plus_preserves_raw_bytes_and_accounted_backing_storage() {
    let engine = Engine::new();
    let identity = engine
        .compile("def run(value: string) -> string\nvalue\nend")
        .unwrap();
    let unary = engine
        .compile("def run(value: string) -> string\n+value\nend")
        .unwrap();
    for length in [0, 8, 1 << 20] {
        let mut bytes = vec![b'x'; length];
        bytes.extend_from_slice(&[0, 0xff, 0xc0, 0x80]);
        let input = Value::bytes(bytes);
        let expected = input.as_bytes().unwrap();
        let plain = identity
            .call("run", std::slice::from_ref(&input), CallOptions::default())
            .unwrap();
        let output = unary
            .call("run", std::slice::from_ref(&input), CallOptions::default())
            .unwrap();
        assert_eq!(output.value.as_bytes(), Some(expected));
        assert_eq!(output.value.as_bytes().unwrap().as_ptr(), expected.as_ptr());
        assert_eq!(output.stats.steps, plain.stats.steps + 1);
        assert_eq!(
            output.stats.peak_memory_bytes,
            plain.stats.peak_memory_bytes
        );
        assert_eq!(
            output.stats.retained_memory_bytes,
            plain.stats.retained_memory_bytes
        );
        let mut options = CallOptions::default();
        options.limits.memory_bytes = Some(output.stats.peak_memory_bytes);
        options.limits.steps = Some(output.stats.steps);
        unary
            .call("run", std::slice::from_ref(&input), options.clone())
            .unwrap();
        options.limits.steps = Some(output.stats.steps - 1);
        assert_eq!(
            unary
                .call("run", std::slice::from_ref(&input), options.clone())
                .unwrap_err()
                .kind,
            ErrorKind::Steps
        );
        options.limits.steps = Some(output.stats.steps);
        options.limits.memory_bytes = Some(output.stats.peak_memory_bytes - 1);
        assert_eq!(
            unary.call("run", &[input], options).unwrap_err().kind,
            ErrorKind::Memory
        );
    }
}

#[test]
fn cancellation_during_the_operand_prevents_the_consumer_call() {
    let token = CancellationToken::new();
    let cancellation = token.clone();
    let calls = Arc::new(AtomicUsize::new(0));
    let captured = calls.clone();
    let mut engine = Engine::new();
    engine.register("operand", move |_, _| {
        cancellation.cancel();
        Ok(Value::bytes(vec![b'x'; 4096]))
    });
    engine.register("consume", move |_, _| {
        captured.fetch_add(1, Ordering::SeqCst);
        Ok(Value::nil())
    });
    let error = engine
        .compile("consume(+operand().as(string))")
        .unwrap()
        .run(CallOptions {
            cancellation: token,
            ..CallOptions::default()
        })
        .unwrap_err();
    assert_eq!(error.kind, ErrorKind::Cancelled);
    assert_eq!(calls.load(Ordering::SeqCst), 0);
}
