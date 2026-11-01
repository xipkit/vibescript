#![cfg(feature = "playground")]

#[path = "../../examples/footprint/allocations.rs"]
mod allocations;

#[global_allocator]
static ALLOCATOR: allocations::Counting = allocations::Counting;

#[test]
fn padded_project_repair_has_bounded_peak_allocations() {
    let mut source = format!("# {}\n", "padding ".repeat(65_536));
    for index in 0..128 {
        source.push_str(&format!("def f{index} -> int\n [1][0] + 1\nend\n"));
    }
    let input = serde_json::to_vec(&serde_json::json!({"op":"fix", "source":source})).unwrap();
    // Warm lazy tables before measuring the request's outstanding allocations.
    vibescript_tools::playground::serve(
        br#"{"op":"fix","source":"1"}"#.as_slice(),
        std::io::sink(),
    )
    .unwrap();
    let before = allocations::snapshot();
    let mut output = Vec::new();
    vibescript_tools::playground::serve(input.as_slice(), &mut output).unwrap();
    let after = allocations::snapshot();
    let response: serde_json::Value = serde_json::from_slice(&output).unwrap();
    let applied = response["applied"].as_u64().unwrap();
    let peak = after[3] - before[2];
    println!(
        "padded repair: input={} bytes, applied={applied}, passes={}, peak_live={peak} bytes, allocated={} bytes",
        input.len(),
        applied + 1,
        after[1] - before[1],
    );
    assert!(peak < 8 << 20, "repair peak live allocation: {peak}");
    assert_eq!(response["ok"], false);
    assert_eq!(response["error"]["kind"], "Memory");
    assert!(applied > 0 && applied < 128);
}
