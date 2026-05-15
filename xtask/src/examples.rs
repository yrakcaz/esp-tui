use anyhow::Context as _;

/// Builds the requested example(s) for the given target, or for all targets
/// when `target` is `None`.
///
/// # Arguments
///
/// * `filter` - Optional example selector; `"c"`, `"rust"`, or `None` for both.
/// * `target` - Optional target triple; must be one of [`crate::agent::TARGETS`].
///   When `None`, iterates over all [`crate::agent::TARGETS`].
///
/// # Errors
///
/// Returns an error if a required tool or environment variable is missing, or
/// if any build step fails.
pub(crate) fn build(
    filter: Option<&str>,
    target: Option<&str>,
) -> anyhow::Result<()> {
    let targets: &[&str] = match target {
        None => crate::agent::TARGETS,
        Some(t) => std::slice::from_ref(
            crate::agent::TARGETS.iter().find(|&&s| s == t).unwrap(),
        ),
    };
    if matches!(filter, None | Some("c")) {
        ensure_idf_tools()?;
    }
    for t in targets {
        match filter {
            None => {
                build_rust(t)?;
                build_c(t)?;
            }
            Some("rust") => build_rust(t)?,
            Some("c") => build_c(t)?,
            Some(other) => {
                anyhow::bail!("unknown example {other:?}; valid options: c, rust")
            }
        }
    }
    Ok(())
}

fn build_rust(target: &str) -> anyhow::Result<()> {
    println!("building Rust example for {target}...");
    crate::agent::build(Some(target))?;
    let esp_env = crate::agent::load_esp_env()?;
    let example_dir = crate::agent::workspace_root().join("examples").join("rust");
    anyhow::ensure!(
        std::process::Command::new("cargo")
            .args([
                "+esp",
                "build",
                "--target",
                target,
                "-Z",
                "build-std=std,panic_abort",
            ])
            .current_dir(&example_dir)
            .envs(esp_env.iter().map(|(k, v)| (k.as_str(), v.as_str())))
            .status()
            .context("cargo not found")?
            .success(),
        "Rust example build failed for {target}"
    );
    println!("  -> examples/rust [{target}] built");
    Ok(())
}

fn ensure_idf_tools() -> anyhow::Result<()> {
    let idf_path = resolve_idf_path()?;
    anyhow::ensure!(
        std::process::Command::new("python3")
            .args([format!("{idf_path}/tools/idf_tools.py").as_str(), "install"])
            .status()
            .context("failed to run idf_tools.py install")?
            .success(),
        "idf_tools.py install failed"
    );
    Ok(())
}

fn build_c(target: &str) -> anyhow::Result<()> {
    println!("building C example for {target}...");
    crate::agent::build(Some(target))?;
    let chip = chip_for_target(target)?;
    let idf_path = resolve_idf_path()?;
    let idf_py = std::path::Path::new(&idf_path).join("tools").join("idf.py");
    anyhow::ensure!(
        idf_py.exists(),
        "idf.py not found at {}; check IDF_PATH",
        idf_py.display()
    );
    let example_dir = crate::agent::workspace_root().join("examples").join("c");
    let build_dir = format!("build/{chip}");
    for step in [
        format!("-B {build_dir} set-target {chip}"),
        format!("-B {build_dir} build"),
    ] {
        let script = format!(". {idf_path}/export.sh 1>/dev/null && idf.py {step}");
        anyhow::ensure!(
            std::process::Command::new("bash")
                .args(["-c", &script])
                .current_dir(&example_dir)
                .status()
                .with_context(|| format!("failed to run idf.py {step}"))?
                .success(),
            "idf.py {step} failed"
        );
    }
    println!("  -> examples/c [{target}] built");
    Ok(())
}

fn chip_for_target(target: &str) -> anyhow::Result<&'static str> {
    match target {
        "xtensa-esp32-espidf" => Ok("esp32"),
        "xtensa-esp32s2-espidf" => Ok("esp32s2"),
        "xtensa-esp32s3-espidf" => Ok("esp32s3"),
        "riscv32imc-esp-espidf" => Ok("esp32c3"),
        "riscv32imac-esp-espidf" => Ok("esp32c6"),
        _ => anyhow::bail!("no chip name mapping for target {target:?}"),
    }
}

fn resolve_idf_path() -> anyhow::Result<String> {
    if let Ok(p) = std::env::var("IDF_PATH") {
        return Ok(p);
    }
    let home = std::env::var("HOME").context("HOME not set")?;
    let candidate = std::path::Path::new(&home)
        .join(".espressif")
        .join("esp-idf")
        .join("v5.3.1");
    anyhow::ensure!(
        candidate.exists(),
        "IDF_PATH not set and ~/.espressif/esp-idf/v5.3.1 not found; \
         set IDF_PATH or run `cargo xtask build examples rust` first to install it"
    );
    Ok(candidate.to_string_lossy().into_owned())
}
