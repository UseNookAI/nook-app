//! Where a JDK is, for the worker's Gradle verifications. An app started from the Start menu
//! carries no JAVA_HOME, so the worker looks in the places a JDK is likely to be named: the
//! environment, the repository's `nook.json` (`"javaHome"`) and `gradle.properties`
//! (`org.gradle.java.home`), the user's and the machine's registered environment, the usual install
//! folders, and PATH. The first of these with a `bin\javac` is the one.
//!
//! Ports `worker/JdkLocator.java`.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// The JDK for verifications in the repository at `repo_root`, if one can be found.
///
/// Blocking (it reads files and asks `reg.exe`): from async code use [`find_async`].
pub fn find(repo_root: &Path) -> Option<PathBuf> {
    let sources = Sources {
        env: &|name| std::env::var(name).ok(),
        user_home: user_home(),
        registry: cfg!(windows),
    };
    candidates(&sources, repo_root)
        .into_iter()
        .find(|c| has_javac(c))
}

/// [`find`] off the async threads.
pub async fn find_async(repo_root: PathBuf) -> Option<PathBuf> {
    tokio::task::spawn_blocking(move || find(&repo_root))
        .await
        .ok()
        .flatten()
}

/// Where the candidates come from; tests pass their own environment and no registry.
struct Sources<'a> {
    env: &'a dyn Fn(&str) -> Option<String>,
    user_home: Option<PathBuf>,
    registry: bool,
}

fn candidates(src: &Sources, repo_root: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    add(&mut out, (src.env)("JAVA_HOME"));
    add(&mut out, from_nook_json(repo_root));
    add(
        &mut out,
        from_gradle_properties(repo_root, src.user_home.as_deref()),
    );
    if src.registry {
        add(&mut out, registry_env("HKCU\\Environment"));
        add(
            &mut out,
            registry_env("HKLM\\SYSTEM\\CurrentControlSet\\Control\\Session Manager\\Environment"),
        );
    }
    let env_dir = |name: &str| {
        (src.env)(name)
            .filter(|v| !v.trim().is_empty())
            .map(PathBuf::from)
    };
    let bases = [
        env_dir("ProgramFiles"),
        env_dir("ProgramFiles(x86)"),
        env_dir("LOCALAPPDATA").map(|d| d.join("Programs")),
        src.user_home.as_ref().map(|h| h.join(".jdks")),
        src.user_home.as_ref().map(|h| h.join("scoop").join("apps")),
    ];
    for base in bases.into_iter().flatten() {
        for vendor in [
            "Java",
            "Eclipse Adoptium",
            "Microsoft",
            "Zulu",
            "Amazon Corretto",
            "BellSoft",
            "OpenJDK",
            "",
        ] {
            let dir = if vendor.is_empty() {
                base.clone()
            } else {
                base.join(vendor)
            };
            let Ok(entries) = std::fs::read_dir(&dir) else {
                continue;
            };
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_dir()
                    && entry
                        .file_name()
                        .to_string_lossy()
                        .to_lowercase()
                        .contains("jdk")
                {
                    out.push(path);
                }
            }
        }
    }
    add(&mut out, from_path((src.env)("PATH")));
    out
}

fn add(list: &mut Vec<PathBuf>, value: Option<String>) {
    if let Some(v) = value.filter(|v| !v.trim().is_empty()) {
        list.push(PathBuf::from(v.trim()));
    }
}

fn has_javac(dir: &Path) -> bool {
    dir.join("bin").join(exe("javac")).is_file()
}

fn exe(name: &str) -> String {
    if cfg!(windows) {
        format!("{name}.exe")
    } else {
        name.to_string()
    }
}

fn user_home() -> Option<PathBuf> {
    std::env::var_os("USERPROFILE")
        .or_else(|| std::env::var_os("HOME"))
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
}

fn from_nook_json(root: &Path) -> Option<String> {
    let cfg = root.join("nook.json");
    if !cfg.is_file() {
        return None;
    }
    let text = std::fs::read_to_string(cfg).ok()?;
    let n: serde_json::Value = serde_json::from_str(crate::settings::strip_bom_str(&text)).ok()?;
    n.get("javaHome")?.as_str().map(String::from)
}

fn from_gradle_properties(root: &Path, user_home: Option<&Path>) -> Option<String> {
    let files = [
        Some(root.join("gradle.properties")),
        user_home.map(|h| h.join(".gradle").join("gradle.properties")),
    ];
    for f in files.into_iter().flatten() {
        if !f.is_file() {
            continue;
        }
        let Ok(bytes) = std::fs::read(&f) else {
            continue;
        };
        // Properties.load(InputStream) reads ISO-8859-1.
        let text: String = bytes.iter().map(|b| char::from(*b)).collect();
        if let Some(home) = properties(&text)
            .remove("org.gradle.java.home")
            .filter(|h| !h.trim().is_empty())
        {
            return Some(home);
        }
    }
    None
}

/// The JAVA_HOME a person set in Windows' environment settings, which an app from the Start menu
/// may not carry.
fn registry_env(key: &str) -> Option<String> {
    let out = crate::process::std_command("reg")
        .args(["query", key, "/v", "JAVA_HOME"])
        .output()
        .ok()?;
    let text = String::from_utf8_lossy(&out.stdout);
    text.lines().find_map(|line| {
        let l = line.trim();
        if !l.starts_with("JAVA_HOME") {
            return None;
        }
        // "JAVA_HOME    REG_SZ    C:\Program Files\Java\jdk-21": the third field, spaces and all.
        let mut rest = l;
        for _ in 0..2 {
            let t = rest.trim_start();
            let end = t.find(|c: char| c.is_ascii_whitespace())?;
            rest = &t[end..];
        }
        let value = rest.trim();
        (!value.is_empty()).then(|| value.to_string())
    })
}

fn from_path(path: Option<String>) -> Option<String> {
    let path = path?;
    for dir in std::env::split_paths(&path) {
        if dir.join(exe("javac")).is_file() {
            return dir.parent().map(|p| p.to_string_lossy().into_owned());
        }
    }
    None
}

/// A `.properties` file as `java.util.Properties.load` reads it: comments (# and !), `key=value`,
/// `key: value` and `key value`, lines continued with a backslash, and backslash escapes.
fn properties(text: &str) -> HashMap<String, String> {
    let mut map = HashMap::new();
    for line in logical_lines(text) {
        let chars: Vec<char> = line.chars().collect();
        let ws = |c: char| matches!(c, ' ' | '\t' | '\u{0C}');
        let (mut key_len, mut value_start, mut has_sep, mut preceding_backslash) =
            (0, chars.len(), false, false);
        while key_len < chars.len() {
            let c = chars[key_len];
            if (c == '=' || c == ':') && !preceding_backslash {
                value_start = key_len + 1;
                has_sep = true;
                break;
            } else if ws(c) && !preceding_backslash {
                value_start = key_len + 1;
                break;
            }
            preceding_backslash = c == '\\' && !preceding_backslash;
            key_len += 1;
        }
        while value_start < chars.len() {
            let c = chars[value_start];
            if !ws(c) {
                if !has_sep && (c == '=' || c == ':') {
                    has_sep = true;
                } else {
                    break;
                }
            }
            value_start += 1;
        }
        map.insert(
            unescape(&chars[..key_len]),
            unescape(&chars[value_start.min(chars.len())..]),
        );
    }
    map
}

/// Natural lines joined where one ends in an odd number of backslashes, comments and blank lines
/// dropped, each line's leading whitespace skipped.
fn logical_lines(text: &str) -> Vec<String> {
    let normalized = text.replace("\r\n", "\n").replace('\r', "\n");
    let mut out = Vec::new();
    let mut current: Option<String> = None;
    for natural in normalized.split('\n') {
        let trimmed = natural.trim_start_matches([' ', '\t', '\u{0C}']);
        let line = match current.as_mut() {
            Some(line) => line,
            None => {
                if trimmed.is_empty() || trimmed.starts_with('#') || trimmed.starts_with('!') {
                    continue;
                }
                current.insert(String::new())
            }
        };
        let backslashes = trimmed.chars().rev().take_while(|c| *c == '\\').count();
        if backslashes % 2 == 1 {
            line.push_str(&trimmed[..trimmed.len() - 1]);
        } else {
            line.push_str(trimmed);
            out.extend(current.take());
        }
    }
    out.extend(current);
    out
}

fn unescape(chars: &[char]) -> String {
    let mut out = String::with_capacity(chars.len());
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        i += 1;
        if c != '\\' || i >= chars.len() {
            if c != '\\' {
                out.push(c);
            }
            continue;
        }
        let e = chars[i];
        i += 1;
        match e {
            'u' => {
                let hex: String = chars[i..chars.len().min(i + 4)].iter().collect();
                if let Some(ch) = u32::from_str_radix(&hex, 16)
                    .ok()
                    .filter(|_| hex.len() == 4)
                    .and_then(char::from_u32)
                {
                    out.push(ch);
                    i += 4;
                }
            }
            't' => out.push('\t'),
            'r' => out.push('\r'),
            'n' => out.push('\n'),
            'f' => out.push('\u{0C}'),
            other => out.push(other),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn jdk(dir: &Path) -> PathBuf {
        std::fs::create_dir_all(dir.join("bin")).unwrap();
        std::fs::write(dir.join("bin").join(exe("javac")), "").unwrap();
        dir.to_path_buf()
    }

    fn first(
        env: &HashMap<&str, String>,
        user_home: Option<&Path>,
        repo: &Path,
    ) -> Option<PathBuf> {
        let lookup = |name: &str| env.get(name).cloned();
        let sources = Sources {
            env: &lookup,
            user_home: user_home.map(Path::to_path_buf),
            registry: false,
        };
        candidates(&sources, repo)
            .into_iter()
            .find(|c| has_javac(c))
    }

    #[test]
    fn nook_json_names_the_jdk_and_java_home_comes_first() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        let named = jdk(&tmp.path().join("jdks").join("named"));
        let json = serde_json::json!({ "javaHome": named.display().to_string() });
        std::fs::write(repo.join("nook.json"), json.to_string()).unwrap();

        assert_eq!(Some(named.clone()), first(&HashMap::new(), None, &repo));
        let env_jdk = jdk(&tmp.path().join("env-jdk"));
        let env = HashMap::from([("JAVA_HOME", env_jdk.display().to_string())]);
        assert_eq!(Some(env_jdk), first(&env, None, &repo));
        let broken = HashMap::from([(
            "JAVA_HOME",
            tmp.path().join("not-a-jdk").display().to_string(),
        )]);
        assert_eq!(
            Some(named),
            first(&broken, None, &repo),
            "a JAVA_HOME without javac is passed over"
        );
    }

    #[test]
    fn gradle_properties_install_folders_and_path_are_looked_in() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        std::fs::create_dir_all(&repo).unwrap();

        let from_props = jdk(&tmp.path().join("props jdk"));
        let escaped = from_props
            .display()
            .to_string()
            .replace('\\', "\\\\")
            .replace(':', "\\:");
        std::fs::write(
            repo.join("gradle.properties"),
            format!("# the build's JDK\norg.gradle.java.home = {escaped}\n"),
        )
        .unwrap();
        assert_eq!(Some(from_props), first(&HashMap::new(), None, &repo));
        std::fs::remove_file(repo.join("gradle.properties")).unwrap();

        let programs = tmp.path().join("Program Files");
        let adoptium = jdk(&programs
            .join("Eclipse Adoptium")
            .join("jdk-21.0.2+13-hotspot"));
        std::fs::create_dir_all(programs.join("Eclipse Adoptium").join("jre-17")).unwrap();
        let env = HashMap::from([("ProgramFiles", programs.display().to_string())]);
        assert_eq!(Some(adoptium), first(&env, None, &repo));

        let home = tmp.path().join("me");
        let jdks = jdk(&home.join(".jdks").join("openjdk-22"));
        assert_eq!(Some(jdks), first(&HashMap::new(), Some(&home), &repo));

        let on_path = jdk(&tmp.path().join("tools").join("java"));
        let path =
            std::env::join_paths([tmp.path().join("elsewhere"), on_path.join("bin")]).unwrap();
        let env = HashMap::from([("PATH", path.to_string_lossy().into_owned())]);
        assert_eq!(Some(on_path), first(&env, None, &repo));

        assert_eq!(None, first(&HashMap::new(), None, &repo));
    }

    #[test]
    fn properties_are_read_as_java_reads_them() {
        let text = "# comment\n! also a comment\n  a = 1\nb:2\nc 3\nd\\=x = 4\ne = one \\\n    two\nf = C:\\\\jdk\\\\21\ng = \\u0041\\t|\nh\n";
        let p = properties(text);
        assert_eq!(Some("1"), p.get("a").map(String::as_str));
        assert_eq!(Some("2"), p.get("b").map(String::as_str));
        assert_eq!(Some("3"), p.get("c").map(String::as_str));
        assert_eq!(Some("4"), p.get("d=x").map(String::as_str));
        assert_eq!(Some("one two"), p.get("e").map(String::as_str));
        assert_eq!(Some("C:\\jdk\\21"), p.get("f").map(String::as_str));
        assert_eq!(Some("A\t|"), p.get("g").map(String::as_str));
        assert_eq!(Some(""), p.get("h").map(String::as_str));
        assert_eq!(
            Some("C:Program FilesJava"),
            properties("k=C:\\Program Files\\Java")
                .get("k")
                .map(String::as_str),
            "an unescaped backslash is dropped, as Java drops it"
        );
    }
}
