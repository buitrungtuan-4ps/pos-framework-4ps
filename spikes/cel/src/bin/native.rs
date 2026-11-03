//! Native timings, and what the crate refuses.
use std::sync::Arc;
use std::time::Instant;

use cel_spike::*;

fn per_call(n: u32, f: impl Fn() -> u32) -> f64 {
    f(); // warm
    let t = Instant::now();
    let r = f();
    std::hint::black_box(r);
    t.elapsed().as_nanos() as f64 / n as f64
}

fn main() {
    setup();
    let n = 100_000;
    println!("| guard | compile µs | eval µs (ctx reused) | ctx+eval µs | result |");
    println!("|---|---|---|---|---|");
    for (i, src) in EXPRESSIONS.iter().enumerate() {
        let i = i as u32;
        let c = per_call(n / 10, || compile_n(i, n / 10)) / 1000.0;
        let e = per_call(n, || eval_n(i, n)) / 1000.0;
        let f = per_call(n, || eval_fresh_n(i, n)) / 1000.0;
        let r = eval_n(i, 1) == 1;
        println!("| `{src}` | {c:.2} | {e:.3} | {f:.3} | {r} |");
    }

    // What the environment refuses or the crate does not support.
    let env = Arc::new(env());
    let ctx = context(Arc::clone(&env), order());
    let probes = [
        "order.items.all(i, i.qty > 0)",
        "order.items.exists(i, i.qty > 2)",
        "order.items.map(i, i.qty)",
        "order.items.filter(i, i.qty > 1)",
        "has(order.member_id)",
        "order.member_id == 'x'",
        "order.total / 0",
        "order.total + 9223372036854775807",
        "order.table.matches('^T[0-9]+$')",
        "timestamp('2026-11-03T10:00:00Z')",
        "duration('1h')",
        "double(order.total) * 1.1",
        "order.total * 1.1",
        "'a' + 1",
        "order.total > 1 ? 'big' : 'small'",
        "[1,2,3][5]",
    ];
    println!("\n| probe | result |\n|---|---|");
    for p in probes {
        let r = match compile(&env, p) {
            Err(e) => format!("compile error: {}", e.lines().next().unwrap_or("")),
            Ok(prog) => format!("{:?}", prog.execute(&ctx)),
        };
        println!("| `{p}` | {} |", r.replace('|', "\\|"));
    }
    let long = format!("{}true", "true && ".repeat(100));
    println!("| {}-char source | {:?} |", long.len(), compile(&env, &long).err());
    let deep = format!("{}1{}", "(".repeat(200), ")".repeat(200));
    println!("| 200-deep parens | {:?} |", env.compile(&deep).err().map(|e| e.to_string().chars().take(80).collect::<String>()));
}
