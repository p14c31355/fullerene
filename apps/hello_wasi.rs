fn main() {
    println!("Hello from WASI on Fullerene!");
    println!("Args:");
    for (i, arg) in std::env::args().enumerate() {
        println!("  {}: {}", i, arg);
    }
    println!("All systems running on Fullerene!");
}
