//! Dense and sparse rewrite timings, including unified diff rendering.

use std::{hint::black_box, time::Instant};
use vibescript_tools::diff::unified_diff;

fn main() {
    if !std::env::args().any(|arg| arg == "--bench") {
        return;
    }
    let dense = "items = [1]\nputs items.size\n".repeat(2_500);
    let sparse: String = (0..50_000)
        .map(|i| {
            if i % 5_555 == 0 {
                "puts [1].size\n"
            } else {
                "puts 1\n"
            }
        })
        .collect();
    for (name, old, iterations) in [("dense", dense, 1), ("sparse", sparse, 100)] {
        let new = old.replace(".size", ".length");
        let expected = unified_diff("bench.vibe", &old, &new);
        let mut samples = Vec::new();
        for _ in 0..9 {
            let start = Instant::now();
            for _ in 0..iterations {
                black_box(unified_diff("bench.vibe", black_box(&old), black_box(&new)));
            }
            samples.push(start.elapsed().as_secs_f64() / f64::from(iterations));
        }
        samples.sort_by(f64::total_cmp);
        println!(
            "{name}: median {:.3} ms ({} diff bytes)",
            samples.get(4).copied().unwrap_or_default() * 1_000.0,
            expected.len()
        );
    }
}
