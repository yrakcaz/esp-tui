use anyhow::Context as _;

fn main() -> anyhow::Result<()> {
    match std::env::args().nth(1).as_deref() {
        Some("build-agent") => build_agent(std::env::args().nth(2).as_deref()),
        Some(cmd) => anyhow::bail!("unknown task: {cmd}"),
        None => anyhow::bail!("usage: cargo xtask <task>"),
    }
}

const TARGETS: &[&str] = &[
    "xtensa-esp32-espidf",
    "xtensa-esp32s2-espidf",
    "xtensa-esp32s3-espidf",
    "riscv32imc-unknown-none-elf",
    "riscv32imac-unknown-none-elf",
];

fn build_agent(target_filter: Option<&str>) -> anyhow::Result<()> {
    let targets: &[&str] = match target_filter {
        None => TARGETS,
        Some(t) => {
            anyhow::ensure!(
                TARGETS.contains(&t),
                "unknown target {t:?}; valid targets: {}",
                TARGETS.join(", ")
            );
            std::slice::from_ref(TARGETS.iter().find(|&&s| s == t).unwrap())
        }
    };

    let workspace_root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("xtask manifest must have a parent directory");

    let esp_env = load_esp_env()?;

    for target in targets {
        println!("building esp-agent for {target}...");
        let mut args: Vec<&str> = Vec::new();
        if let Some(tc) = toolchain_for(target) {
            args.push(tc);
        }
        args.extend(["build", "-p", "esp-agent", "--release", "--target", target]);
        if target.starts_with("xtensa-") {
            args.push("-Z");
            args.push("build-std=core");
        }
        let mut cmd = std::process::Command::new("cargo");
        cmd.args(&args).current_dir(workspace_root);
        if target.starts_with("xtensa-") {
            cmd.envs(esp_env.iter().map(|(k, v)| (k.as_str(), v.as_str())));
        }
        anyhow::ensure!(cmd.status()?.success(), "build failed for {target}");

        let lib = workspace_root
            .join("target")
            .join(target)
            .join("release")
            .join("libesp_agent.a");
        verify_symbols(&lib)?;
        weaken_panic_symbol(&lib, target, &esp_env)?;
        println!("  -> target/{target}/release/libesp_agent.a");
    }
    Ok(())
}

/// Parses `~/export-esp.sh` written by `espup install` and returns the
/// environment variables it sets, with `$PATH` references resolved against
/// the current process environment.
///
/// # Returns
///
/// A list of `(name, value)` pairs ready to pass to [`Command::envs`].
///
/// # Errors
///
/// Returns an error if `~/export-esp.sh` is absent (toolchain not installed),
/// cannot be read, or does not contain a `PATH` entry (format unexpected).
fn load_esp_env() -> anyhow::Result<Vec<(String, String)>> {
    let home = std::env::var("HOME").context("HOME not set")?;
    let script = std::path::Path::new(&home).join("export-esp.sh");
    anyhow::ensure!(
        script.exists(),
        "~/export-esp.sh not found; install the Xtensa toolchain with `espup install`"
    );
    let content = std::fs::read_to_string(&script)
        .with_context(|| format!("failed to read {}", script.display()))?;

    let vars: Vec<(String, String)> = content
        .lines()
        .filter_map(|line| line.strip_prefix("export "))
        .filter_map(|rest| rest.split_once('='))
        .map(|(key, raw)| {
            let val = raw.trim_matches('"');
            let resolved = if key == "PATH" {
                let added =
                    val.split_once(":$PATH").map_or(val, |(prefix, _)| prefix);
                let current = std::env::var("PATH").unwrap_or_default();
                format!("{added}:{current}")
            } else {
                val.to_owned()
            };
            (key.to_owned(), resolved)
        })
        .collect();

    anyhow::ensure!(
        vars.iter().any(|(k, _)| k == "PATH"),
        "~/export-esp.sh did not set PATH; espup may have changed its output format"
    );
    Ok(vars)
}

fn toolchain_for(target: &str) -> Option<&'static str> {
    target.starts_with("xtensa-").then_some("+esp")
}

fn llvm_objcopy_path() -> anyhow::Result<std::path::PathBuf> {
    let out = std::process::Command::new("rustc")
        .args(["--print", "sysroot"])
        .output()
        .context("rustc not found")?;
    anyhow::ensure!(out.status.success(), "rustc --print sysroot failed");
    let sysroot = String::from_utf8(out.stdout)
        .context("rustc sysroot path is not valid UTF-8")?;
    let lib_rustlib = std::path::Path::new(sysroot.trim())
        .join("lib")
        .join("rustlib");
    std::fs::read_dir(&lib_rustlib)
        .with_context(|| format!("cannot read {}", lib_rustlib.display()))?
        .flatten()
        .map(|e| e.path().join("bin").join("llvm-objcopy"))
        .find(|p| p.exists())
        .ok_or_else(|| {
            anyhow::anyhow!(
                "llvm-objcopy not found; \
                 run: rustup component add llvm-tools-preview"
            )
        })
}

/// Finds the symbol in `lib` whose name ends with `rust_begin_unwind`.
///
/// The Rust compiler emits a Rust-mangled name (e.g.
/// `_RNvCs..._17rust_begin_unwind`) for the internal shim it generates for
/// `#[panic_handler]` functions. We match by suffix so the lookup works
/// for both the mangled form and any future unmangled form.
///
/// # Arguments
///
/// * `lib` - Path to the `.a` archive to search.
///
/// # Returns
///
/// The exact symbol name as it appears in the archive.
///
/// # Errors
///
/// Returns an error if `nm` is not found, fails, or the symbol is absent.
fn find_rust_begin_unwind(lib: &std::path::Path) -> anyhow::Result<String> {
    let out = std::process::Command::new("nm")
        .arg("--defined-only")
        .arg(lib)
        .output()
        .context("nm not found")?;
    anyhow::ensure!(out.status.success(), "nm failed on {}", lib.display());
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter_map(|line| line.split_whitespace().last().map(str::to_owned))
        .find(|sym| sym.ends_with("rust_begin_unwind"))
        .ok_or_else(|| {
            anyhow::anyhow!(
                "rust_begin_unwind not found in {}; \
                 ensure the crate defines a #[panic_handler]",
                lib.display()
            )
        })
}

/// Makes `rust_begin_unwind` a weak symbol in the archive so that Rust
/// projects linking the `.a` can override it with their own panic handler
/// (e.g. from std) without a duplicate-symbol linker error. C/C++ projects
/// are unaffected: the weak symbol is used when no other definition exists.
///
/// # Arguments
///
/// * `lib` - Path to the built `.a` archive.
/// * `target` - Target triple; selects the correct `objcopy` binary.
/// * `esp_env` - Xtensa toolchain environment from [`load_esp_env`].
///
/// # Errors
///
/// Returns an error if the `objcopy` binary is not found, exits non-zero,
/// or the symbol is not weak after the operation.
fn weaken_panic_symbol(
    lib: &std::path::Path,
    target: &str,
    esp_env: &[(String, String)],
) -> anyhow::Result<()> {
    let sym = find_rust_begin_unwind(lib)?;
    let (bin, use_esp_env) = if target.starts_with("xtensa-") {
        (std::path::PathBuf::from("xtensa-esp-elf-objcopy"), true)
    } else {
        (llvm_objcopy_path()?, false)
    };
    let mut cmd = std::process::Command::new(&bin);
    cmd.arg(format!("--weaken-symbol={sym}")).arg(lib);
    if use_esp_env {
        cmd.envs(esp_env.iter().map(|(k, v)| (k.as_str(), v.as_str())));
    }
    anyhow::ensure!(
        cmd.status()
            .with_context(|| format!("{} not found", bin.display()))?
            .success(),
        "objcopy failed to weaken {sym} in {}",
        lib.display()
    );

    let out = std::process::Command::new("nm")
        .arg("--defined-only")
        .arg(lib)
        .output()
        .context("nm not found")?;
    anyhow::ensure!(out.status.success(), "nm failed on {}", lib.display());
    let text = String::from_utf8_lossy(&out.stdout);
    let weakened = text.lines().any(|line| {
        let mut parts = line.split_whitespace();
        let last = parts.next_back();
        let typ = parts.next_back();
        matches!((typ, last), (Some("W"), Some(s)) if s == sym)
    });
    anyhow::ensure!(
        weakened,
        "rust_begin_unwind is not weak in {}; \
         objcopy may not support this object format",
        lib.display()
    );
    println!("  rust_begin_unwind weakened");
    Ok(())
}

fn verify_symbols(path: &std::path::Path) -> anyhow::Result<()> {
    let out = std::process::Command::new("nm")
        .arg("--defined-only")
        .arg(path)
        .output()
        .context("nm not found; install binutils")?;
    anyhow::ensure!(out.status.success(), "nm failed on {}", path.display());
    let text = String::from_utf8_lossy(&out.stdout);
    let syms = ["esp_agent_configure", "esp_agent_ctor"];
    for sym in syms {
        anyhow::ensure!(
            text.contains(sym),
            "symbol {sym:?} missing from {}; ABI may be broken",
            path.display(),
        );
    }
    println!("  symbols verified: {}", syms.join(", "));
    Ok(())
}
