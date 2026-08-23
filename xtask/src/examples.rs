use anyhow::Context as _;

const ESP_IDF_VERSION: &str = "v5.3.1";

/// Builds the requested example(s) for the given target, or for all targets
/// when `target` is `None`.
///
/// # Arguments
///
/// * `filter` - Optional example selector; `"c"`, `"rust"`, `"panic"`, or
///   `None` for all three.
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
    let esp_env = if matches!(filter, None | Some("rust")) {
        Some(crate::agent::load_esp_env()?)
    } else {
        None
    };
    // C/panic-only: IDF must already be installed; run idf_tools.py install once
    // upfront. All/rust: IDF is installed by esp-idf-sys during the first Rust
    // build, so defer the idf_tools.py call to after that build (tracked by
    // idf_tools_done below).
    if matches!(filter, Some("c" | "panic")) {
        ensure_idf_tools()?;
    }
    let mut idf_tools_done = false;
    for t in crate::agent::filter_targets(target)? {
        crate::agent::build(Some(t))?;
        match filter {
            None => {
                build_rust(t, esp_env.as_deref().unwrap())?;
                if !idf_tools_done {
                    ensure_idf_tools()?;
                    idf_tools_done = true;
                }
                build_c(t)?;
                build_panic(t)?;
            }
            Some("rust") => build_rust(t, esp_env.as_deref().unwrap())?,
            Some("c") => build_c(t)?,
            Some("panic") => build_panic(t)?,
            Some(other) => {
                anyhow::bail!(
                    "unknown example {other:?}; valid options: c, rust, panic"
                )
            }
        }
    }
    Ok(())
}

fn build_rust(target: &str, esp_env: &[(String, String)]) -> anyhow::Result<()> {
    println!("building Rust example for {target}...");
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

fn build_idf_project(target: &str, dir_name: &str) -> anyhow::Result<()> {
    println!("building {dir_name} example for {target}...");
    let chip = chip_for_target(target)?;
    let idf_path = resolve_idf_path()?;
    let idf_py = std::path::Path::new(&idf_path).join("tools").join("idf.py");
    anyhow::ensure!(
        idf_py.exists(),
        "idf.py not found at {}; check IDF_PATH",
        idf_py.display()
    );
    let example_dir = crate::agent::workspace_root()
        .join("examples")
        .join(dir_name);
    let build_dir = format!("build/{chip}");
    for step in [
        format!("-B {build_dir} set-target {chip}"),
        format!("-B {build_dir} build"),
    ] {
        let script =
            format!(". '{idf_path}/export.sh' 1>/dev/null && idf.py {step}");
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
    println!("  -> examples/{dir_name} [{target}] built");
    Ok(())
}

fn build_c(target: &str) -> anyhow::Result<()> {
    build_idf_project(target, "c")
}

fn build_panic(target: &str) -> anyhow::Result<()> {
    build_idf_project(target, "panic")
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
    std::env::var("IDF_PATH").ok().map_or_else(
        || {
            let home = std::env::var("HOME").context("HOME not set")?;
            let candidate = std::path::Path::new(&home)
                .join(".espressif")
                .join("esp-idf")
                .join(ESP_IDF_VERSION);
            anyhow::ensure!(
                candidate.exists(),
                "IDF_PATH not set and ~/.espressif/esp-idf/{ESP_IDF_VERSION} not found; \
                 set IDF_PATH or run `cargo xtask build examples rust` first to install it"
            );
            Ok(candidate.to_string_lossy().into_owned())
        },
        Ok,
    )
}
