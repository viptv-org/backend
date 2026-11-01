//! Execute the canonical transient-transport corpus natively.
#[path = "../tests/common/native_torrent_vectors.rs"]
mod vectors;
fn main() {
    println!(
        "{}",
        serde_json::to_string(&vectors::run_vectors()).unwrap()
    );
}
