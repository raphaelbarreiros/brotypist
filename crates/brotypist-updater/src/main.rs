//! The exact shared trust/transaction source, linked without the application or llama.cpp.
//! Unit coverage lives in the application library; this binary has a separate relocation smoke.
#[path = "../../brotypist/src/paths.rs"]
pub mod paths;
#[path = "../../brotypist/src/update_install.rs"]
pub mod update_install;
#[path = "../../brotypist/src/update_keys.rs"]
pub mod update_keys;
#[path = "../../brotypist/src/updates.rs"]
pub mod updates;

fn main() {
    let arguments: Vec<String> = std::env::args().skip(1).collect();
    if arguments == ["--version"] {
        println!("BroTypist updater {}", env!("CARGO_PKG_VERSION"));
        return;
    }
    let result = update_install::helper_main(&arguments);
    match result {
        Ok(code) => std::process::exit(code),
        Err(error) => {
            eprintln!("BroTypist updater: {error}");
            std::process::exit(1);
        }
    }
}
