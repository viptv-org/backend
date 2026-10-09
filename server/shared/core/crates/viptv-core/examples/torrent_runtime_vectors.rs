#[path = "../tests/common/torrent_runtime_vectors.rs"]
mod vectors;
fn main() {
    let root = vectors::corpus();
    let output: Vec<_> = root["cases"]
        .as_array()
        .unwrap()
        .iter()
        .map(|case| vectors::run(&root, case))
        .collect();
    println!("{}", serde_json::to_string(&output).unwrap());
}
