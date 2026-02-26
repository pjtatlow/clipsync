use std::path::Path;
use std::process::Command;

fn main() {
    // Re-run if the server module source changes or if the generated bindings are missing
    println!("cargo:rerun-if-changed=server/src/lib.rs");
    println!("cargo:rerun-if-changed=server/Cargo.toml");
    println!("cargo:rerun-if-changed=src/module_bindings/mod.rs");

    let out_dir = Path::new("src/module_bindings");

    // Skip generation if bindings already exist and we're not forced to rebuild.
    if out_dir.join("mod.rs").exists() {
        return;
    }

    let spacetime = which("spacetime").unwrap_or_else(|| {
        panic!(
            "spacetime CLI not found in PATH. Install it with:\n  \
             curl -sSf https://install.spacetimedb.com | bash"
        );
    });

    let status = Command::new(spacetime)
        .args([
            "generate",
            "--lang",
            "rust",
            "--out-dir",
            out_dir.to_str().unwrap(),
            "--module-path",
            "./server",
        ])
        .status()
        .expect("failed to run spacetime generate");

    if !status.success() {
        panic!("spacetime generate failed with exit code: {}", status);
    }
}

fn which(binary: &str) -> Option<String> {
    // Check common locations first, then fall back to PATH
    let home = std::env::var("HOME").unwrap_or_default();
    let candidates = [
        format!("{home}/.local/bin/{binary}"),
        format!("{home}/.cargo/bin/{binary}"),
        format!("/usr/local/bin/{binary}"),
        format!("/usr/bin/{binary}"),
    ];

    for candidate in &candidates {
        if Path::new(candidate).exists() {
            return Some(candidate.clone());
        }
    }

    // Fall back to `which` command
    Command::new("which")
        .arg(binary)
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
}
