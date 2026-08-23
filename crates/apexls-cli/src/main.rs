//! Local debug tool: parse a file and dump its tree.

fn main() {
    let path = std::env::args().nth(1);
    match path {
        Some(path) => println!("apexls-cli: would parse {path} (parser not yet implemented)"),
        None => println!("usage: apexls-cli <file.cls>"),
    }
}
