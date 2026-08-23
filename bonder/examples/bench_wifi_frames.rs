//! Repeatable host benchmark for 802.11 management-frame construction.
//!
//! Run with `cargo run -p bonder --release --example bench_wifi_frames`.
//! The example remains reusable for measuring allocation-sensitive Wi-Fi
//! changes on the target toolchain.

use bonder::wifi::{Ssid, build_assoc_request_with_security, build_auth_frame, build_deauth};
use std::hint::black_box;
use std::time::Instant;

const ITERATIONS: usize = 200_000;
const AP: [u8; 6] = [0xf0, 0xf8, 0x4a, 0xe8, 0x22, 0x18];
const CLIENT: [u8; 6] = [0x02, 0x00, 0x00, 0x00, 0x00, 0x01];

fn measure(mut build: impl FnMut() -> Vec<u8>) -> (u128, usize) {
    let start = Instant::now();
    let mut bytes = 0;
    for _ in 0..ITERATIONS {
        bytes += black_box(build()).len();
    }
    (start.elapsed().as_nanos(), bytes)
}

fn report(name: &str, elapsed_ns: u128, bytes: usize) {
    let ns_per_frame = elapsed_ns as f64 / ITERATIONS as f64;
    let mib_per_second = bytes as f64 / (elapsed_ns as f64 / 1.0e9) / (1024.0 * 1024.0);
    println!("{name}: {ns_per_frame:.1} ns/frame, {mib_per_second:.1} MiB/s");
}

fn main() {
    let ssid = Ssid::new(b"fullerene-lab");
    let (elapsed, bytes) = measure(|| build_auth_frame(AP, CLIENT, 1));
    report("authentication", elapsed, bytes);

    let (elapsed, bytes) = measure(|| build_assoc_request_with_security(AP, CLIENT, &ssid, true));
    report("WPA2 association", elapsed, bytes);

    let (elapsed, bytes) = measure(|| build_deauth(AP, CLIENT, 3));
    report("deauthentication", elapsed, bytes);
}
