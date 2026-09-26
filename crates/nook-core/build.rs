// Stamps the build with its commit and time (the Kotlin build's build-info.properties).
use std::process::Command;

fn main() {
    let commit = std::env::var("NOOK_COMMIT")
        .ok()
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| {
            Command::new("git")
                .args(["rev-parse", "--short", "HEAD"])
                .output()
                .ok()
                .filter(|o| o.status.success())
                .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
                .unwrap_or_else(|| "unknown".into())
        });
    let time = chrono_like_now();
    println!("cargo:rustc-env=NOOK_BUILD_COMMIT={commit}");
    println!("cargo:rustc-env=NOOK_BUILD_TIME={time}");
    println!("cargo:rerun-if-env-changed=NOOK_COMMIT");
    println!("cargo:rerun-if-env-changed=NOOK_VERSION");
    println!("cargo:rerun-if-env-changed=NOOK_RS_UPDATE_BASE");
    if let Ok(v) = std::env::var("NOOK_VERSION") {
        if !v.is_empty() {
            println!("cargo:rustc-env=NOOK_BUILD_VERSION={v}");
        }
    }
    // The commit changes with HEAD; in a worktree .git is a file, so ask git where HEAD lives.
    if let Some(head) = git_path("HEAD") {
        println!("cargo:rerun-if-changed={head}");
    }
    println!("cargo:rerun-if-changed=../../resources");
}

fn chrono_like_now() -> String {
    // Seconds since the epoch; BuildInfo formats it. Avoids a build-dependency.
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    secs.to_string()
}

fn git_path(name: &str) -> Option<String> {
    let out = Command::new("git")
        .args(["rev-parse", "--path-format=absolute", "--git-path", name])
        .output()
        .ok()
        .filter(|o| o.status.success())?;
    let path = String::from_utf8_lossy(&out.stdout).trim().to_string();
    std::path::Path::new(&path).exists().then_some(path)
}
