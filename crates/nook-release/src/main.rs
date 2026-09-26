//! `nook-release`: Nook's release signing from the command line (the original's tools/release.py).

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use anyhow::{bail, Context, Result};
use nook_release::{
    build_manifest, keygen, load_key, public_line, verify_files, write_signed, ManifestRequest,
};

const USAGE: &str = "\
Nook releases: the signing key, the signed latest.json manifest, and its check.

Usage:
  nook-release keygen KEYFILE                                  a new Ed25519 key; prints the public key line
  nook-release manifest KEYFILE --channel stable|dev --installer PATH (--url URL | --base URL) --out DIR
                        [--version V] [--commit SHA] [--notes TEXT] [--published ISO]
  nook-release manifest KEYFILE --channel stable|dev --sha256 HEX --size BYTES --version V --url URL --out DIR ...
                                                               for an installer already on the host
  nook-release verify KEYS DIR/<channel>/latest.json           KEYS: a public key (base64url) or release-keys.txt
  nook-release public KEYFILE                                  the key's public half, as release-keys.txt has it

The manifest is written to DIR/<channel>/latest.json with latest.json.sig beside it: one line, the
base64url Ed25519 signature over \"nook-release-v1\" + the exact bytes of latest.json. --base puts the
installer at <base>/builds/<version>-<commit>/<file>, the download host's layout. The version and
the commit default to what the installer's build.json beside it says, when there is one, and the
version then to the installer's name.";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match run(&args) {
        Ok(code) => code,
        Err(e) => {
            eprintln!("nook-release: {e:#}");
            ExitCode::from(2)
        }
    }
}

fn run(args: &[String]) -> Result<ExitCode> {
    let Some(command) = args.first() else {
        println!("{USAGE}");
        return Ok(ExitCode::from(2));
    };
    let rest = &args[1..];
    match command.as_str() {
        "keygen" => {
            let [keyfile] = positional::<1>(rest)?;
            let path = PathBuf::from(keyfile);
            let public = keygen(&path)?;
            println!(
                "private key written to {} (keep it out of every repository)",
                path.display()
            );
            println!("public key line for resources/release-keys.txt:");
            println!("{public}");
            Ok(ExitCode::SUCCESS)
        }
        "public" => {
            let [keyfile] = positional::<1>(rest)?;
            println!("{}", public_line(&load_key(Path::new(&keyfile))?));
            Ok(ExitCode::SUCCESS)
        }
        "verify" => {
            let [keys, manifest] = positional::<2>(rest)?;
            match verify_files(&keys, Path::new(&manifest))? {
                Some(r) => {
                    println!("signature ok");
                    let published = r
                        .published
                        .map(|p| p.format("%Y-%m-%dT%H:%M:%SZ").to_string())
                        .unwrap_or_default();
                    for (k, v) in [
                        ("channel", r.channel.clone()),
                        ("version", r.version.clone()),
                        ("file", r.file.clone()),
                        ("url", r.url.clone()),
                        ("sha256", r.sha256.clone()),
                        ("size", r.size.to_string()),
                        ("commit", r.commit.clone()),
                        ("published", published),
                    ] {
                        println!("{k:<10} {v}");
                    }
                    Ok(ExitCode::SUCCESS)
                }
                None => {
                    println!("signature NOT VALID");
                    Ok(ExitCode::from(1))
                }
            }
        }
        "manifest" => {
            let (keyfile, req, out) = manifest_args(rest)?;
            let key = load_key(Path::new(&keyfile))?;
            let m = build_manifest(&req)?;
            let path = write_signed(&key, &m, &out)?;
            println!("wrote {} and latest.json.sig", path.display());
            print!(
                "{}",
                String::from_utf8_lossy(&nook_release::manifest_bytes(&m))
            );
            Ok(ExitCode::SUCCESS)
        }
        "-h" | "--help" | "help" => {
            println!("{USAGE}");
            Ok(ExitCode::SUCCESS)
        }
        other => bail!("unknown command {other}\n\n{USAGE}"),
    }
}

fn positional<const N: usize>(rest: &[String]) -> Result<[String; N]> {
    <[String; N]>::try_from(rest.to_vec())
        .map_err(|_| anyhow::anyhow!("expected {N} argument(s)\n\n{USAGE}"))
}

fn manifest_args(rest: &[String]) -> Result<(String, ManifestRequest, PathBuf)> {
    let mut keyfile = None;
    let mut out = None;
    let mut req = ManifestRequest::default();
    let mut it = rest.iter();
    while let Some(arg) = it.next() {
        let mut value = || {
            it.next()
                .cloned()
                .with_context(|| format!("{arg} needs a value"))
        };
        match arg.as_str() {
            "--channel" => req.channel = value()?,
            "--installer" => req.installer = Some(PathBuf::from(value()?)),
            "--sha256" => req.sha256 = Some(value()?),
            "--size" => req.size = Some(value()?.parse().context("--size is a number of bytes")?),
            "--url" => req.url = Some(value()?),
            "--base" => req.base = Some(value()?),
            "--out" => out = Some(PathBuf::from(value()?)),
            "--version" => req.version = Some(value()?),
            "--commit" => req.commit = Some(value()?),
            "--notes" => req.notes = Some(value()?),
            "--published" => req.published = Some(value()?),
            flag if flag.starts_with("--") => bail!("unknown option {flag}"),
            positional if keyfile.is_none() => keyfile = Some(positional.to_string()),
            extra => bail!("unexpected argument {extra}"),
        }
    }
    let keyfile = keyfile.context("manifest needs KEYFILE")?;
    if req.channel.is_empty() {
        bail!("--channel stable|dev is required");
    }
    let out = out.context("--out DIR is required")?;
    Ok((keyfile, req, out))
}
