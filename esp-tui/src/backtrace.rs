use std::path::Path;
use std::sync::LazyLock;

use regex::Regex;

static ADDR_PAIR_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"0x([0-9A-Fa-f]+):0x[0-9A-Fa-f]+")
        .expect("valid backtrace address regex")
});

// ESP-IDF's panic handler always prefixes the dump with this literal token.
// Without this guard, any unrelated `0xHEX:0xHEX` text (e.g. esp-agent's
// partition table telemetry, formatted `label:type:0xoffset:0xsize`) would be
// misread as a crash.
const BACKTRACE_PREFIX: &str = "Backtrace:";

/// One resolved (or unresolved) stack frame from a decoded panic backtrace.
pub(crate) struct Frame {
    pub(crate) address: u64,
    pub(crate) function: Option<String>,
    pub(crate) file: Option<String>,
    pub(crate) line: Option<u32>,
}

/// A decoded panic backtrace, ready to display in the backtrace popup.
pub(crate) struct Report {
    pub(crate) header: Option<String>,
    pub(crate) frames: Vec<Frame>,
    pub(crate) warning: Option<String>,
}

/// Addresses captured from a `Backtrace:` line, pending symbol resolution.
pub(crate) struct Pending {
    pub(crate) header: Option<String>,
    pub(crate) addresses: Vec<u64>,
}

/// Extracts program-counter addresses from a serial line containing an
/// ESP-IDF `Backtrace:` dump (`PC:SP` pairs separated by spaces).
///
/// # Arguments
///
/// * `message` - The log message body to scan.
///
/// # Returns
///
/// The PC address from each `0xPC:0xSP` pair found, in order. Empty when the
/// line contains no backtrace address pairs.
#[must_use]
pub(crate) fn extract_addresses(message: &str) -> Vec<u64> {
    if message.starts_with(BACKTRACE_PREFIX) {
        ADDR_PAIR_RE
            .captures_iter(message)
            .filter_map(|caps| u64::from_str_radix(&caps[1], 16).ok())
            .collect()
    } else {
        Vec::new()
    }
}

/// Resolves a list of addresses against an ELF file's DWARF debug info.
///
/// # Arguments
///
/// * `elf_path` - Path to the ELF firmware image containing debug info.
/// * `addresses` - Program-counter addresses to resolve, in the order they
///   should appear in the result.
///
/// # Returns
///
/// One [`Frame`] per input address, or more than one when an address is an
/// inlined call site: every inlined function at that program counter is
/// yielded, innermost first, followed by the enclosing physical function,
/// matching how a real backtrace shows an inline chain as separate entries.
/// An address with no matching debug info, or whose debug info can't be
/// read, still produces a single `Frame` with `function`, `file`, and `line`
/// set to `None`, rather than discarding the rest of the backtrace.
///
/// # Errors
///
/// Returns an error if the file cannot be read, is not a valid object file,
/// or its debug info cannot be parsed.
pub(crate) fn resolve(
    elf_path: &Path,
    addresses: &[u64],
) -> anyhow::Result<Vec<Frame>> {
    let loader = addr2line::Loader::new(elf_path)
        .map_err(|e| anyhow::anyhow!("failed to load ELF debug info: {e}"))?;
    Ok(addresses
        .iter()
        .flat_map(|&address| resolve_one(&loader, address))
        .collect())
}

/// Resolves one address to every inlined frame at that program counter
/// (see [`resolve`]'s `# Returns`). Degrades to a single raw frame instead
/// of propagating an error when this address's own DWARF entry can't be
/// read, so one malformed compilation unit can't poison frames already
/// resolved for other addresses in the same backtrace. If the walk fails
/// partway through an inline chain, any frames already resolved before the
/// failure are discarded too, in favor of the same raw-frame fallback,
/// rather than showing a partial chain.
fn resolve_one(loader: &addr2line::Loader, address: u64) -> Vec<Frame> {
    match try_resolve_one(loader, address) {
        Ok(frames) if !frames.is_empty() => frames,
        _ => vec![raw_frame(address)],
    }
}

fn try_resolve_one(
    loader: &addr2line::Loader,
    address: u64,
) -> anyhow::Result<Vec<Frame>> {
    let mut iter = loader
        .find_frames(address)
        .map_err(|e| anyhow::anyhow!("failed to resolve frame: {e}"))?;
    let mut frames = Vec::new();
    while let Some(frame) = iter
        .next()
        .map_err(|e| anyhow::anyhow!("failed to resolve frame: {e}"))?
    {
        let function = frame
            .function
            .as_ref()
            .and_then(|f| f.demangle().ok().map(std::borrow::Cow::into_owned));
        let (file, line) = frame.location.as_ref().map_or((None, None), |loc| {
            (loc.file.map(ToOwned::to_owned), loc.line)
        });
        frames.push(Frame {
            address,
            function,
            file,
            line,
        });
    }
    Ok(frames)
}

fn raw_frame(address: u64) -> Frame {
    Frame {
        address,
        function: None,
        file: None,
        line: None,
    }
}

fn raw_frames(addresses: Vec<u64>) -> Vec<Frame> {
    addresses.into_iter().map(raw_frame).collect()
}

/// Builds a decoded backtrace report from captured addresses, resolving
/// against the given ELF file when one is available.
///
/// # Arguments
///
/// * `header` - A preceding `Guru Meditation Error` line, if one was found.
/// * `addresses` - Program-counter addresses extracted from the `Backtrace:`
///   line.
/// * `elf_path` - The currently configured ELF path, if any.
///
/// # Returns
///
/// A [`Report`] with resolved frames when `elf_path` is `Some` and resolution
/// succeeds; otherwise a report with raw, unresolved addresses and a
/// `warning` explaining why.
#[must_use]
pub(crate) fn build_report(
    header: Option<String>,
    addresses: Vec<u64>,
    elf_path: Option<&Path>,
) -> Report {
    let Some(elf_path) = elf_path else {
        return Report {
            header,
            warning: Some("No ELF file loaded; showing raw addresses.".to_owned()),
            frames: raw_frames(addresses),
        };
    };
    match resolve(elf_path, &addresses) {
        Ok(frames) => Report {
            header,
            frames,
            warning: None,
        },
        Err(e) => Report {
            header,
            warning: Some(format!("Failed to resolve symbols: {e}")),
            frames: raw_frames(addresses),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extract_addresses_single_pair() {
        let addrs = extract_addresses("Backtrace:0x400d1fb2:0x3ffb2170");
        assert_eq!(addrs, vec![0x400d_1fb2]);
    }

    #[test]
    fn extract_addresses_multiple_pairs() {
        let addrs = extract_addresses(
            "Backtrace:0x400d1fb2:0x3ffb2170 0x400d2e9d:0x3ffb2190",
        );
        assert_eq!(addrs, vec![0x400d_1fb2, 0x400d_2e9d]);
    }

    #[test]
    fn extract_addresses_empty_for_non_backtrace_line() {
        assert!(extract_addresses("I (1) wifi: Connected").is_empty());
    }

    #[test]
    fn extract_addresses_ignores_malformed_pair() {
        assert!(extract_addresses("Backtrace:0xzz:0x3ffb2170").is_empty());
    }

    // Regression: esp-agent's partition table telemetry is formatted
    // `label:type:0xoffset:0xsize`, which matches the same `0xHEX:0xHEX`
    // shape as a real backtrace pair. Only a line starting with the literal
    // `Backtrace:` prefix should be treated as a crash dump.
    #[test]
    fn extract_addresses_ignores_partition_table_style_offsets() {
        assert!(extract_addresses(
            "parts nvs:d:0x9000:0x6000,factory:0:0x10000:0x100000"
        )
        .is_empty());
    }

    #[test]
    fn extract_addresses_ignores_hex_pairs_not_at_line_start() {
        assert!(
            extract_addresses("some prefix Backtrace:0x400d1fb2:0x3ffb2170")
                .is_empty()
        );
    }

    #[test]
    fn build_report_without_elf_path_has_warning_and_raw_addresses() {
        let report = build_report(None, vec![0x400d_1fb2], None);
        assert!(report.warning.is_some());
        assert_eq!(report.frames.len(), 1);
        assert_eq!(report.frames[0].address, 0x400d_1fb2);
        assert!(report.frames[0].function.is_none());
    }

    #[test]
    fn build_report_preserves_header() {
        let report = build_report(
            Some("Guru Meditation Error: Core 0 panic'ed".to_owned()),
            vec![0x1],
            None,
        );
        assert_eq!(
            report.header.as_deref(),
            Some("Guru Meditation Error: Core 0 panic'ed")
        );
    }

    #[test]
    fn build_report_with_nonexistent_elf_path_has_warning() {
        let report = build_report(
            None,
            vec![0x1],
            Some(Path::new("/nonexistent/path/to.elf")),
        );
        assert!(report.warning.is_some());
        assert_eq!(report.frames.len(), 1);
    }

    #[test]
    fn resolve_errors_on_missing_file() {
        let result = resolve(Path::new("/nonexistent/path/to.elf"), &[0x1]);
        assert!(result.is_err());
    }

    #[test]
    fn resolve_errors_on_non_elf_file() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(tmp.path(), b"not an elf file").unwrap();
        let result = resolve(tmp.path(), &[0x1]);
        assert!(result.is_err());
    }

    // `tests/fixtures/panic_test.elf` is a tiny x86_64 ELF object (compiled
    // from a two-function C file with `-g -O0`, never linked) checked in
    // purely to exercise the real object/gimli/addr2line wiring end to end.
    // Address 0x0 is the entry to `add()`, at line 1 of the source.
    #[test]
    fn resolve_decodes_known_address_from_fixture_elf() {
        let fixture = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/panic_test.elf");
        let frames = resolve(&fixture, &[0x0]).unwrap();
        assert_eq!(frames.len(), 1);
        assert_eq!(frames[0].function.as_deref(), Some("add"));
        assert!(frames[0].file.as_deref().unwrap().ends_with("panic_test.c"));
        assert_eq!(frames[0].line, Some(1));
    }

    // `tests/fixtures/panic_test_inline.elf` is a tiny arm64 Mach-O object
    // (compiled with `-g -O2` from a two-function C file where `inner` is
    // force-inlined into `outer` via `__attribute__((always_inline))`,
    // never linked) checked in purely to exercise addr2line's inline-frame
    // walk end to end. Address 0x0 is the entry to `outer`, inside the
    // range where `inner` is inlined (confirmed via `dwarfdump`:
    // `DW_TAG_inlined_subroutine` at `DW_AT_call_line 9`).
    #[test]
    fn resolve_decodes_all_inlined_frames_from_fixture_elf() {
        let fixture = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/panic_test_inline.elf");
        let frames = resolve(&fixture, &[0x0]).unwrap();
        assert_eq!(frames.len(), 2);
        assert_eq!(frames[0].function.as_deref(), Some("inner"));
        assert_eq!(frames[1].function.as_deref(), Some("outer"));
        // The outer frame's line is the call site of the inlined `inner`.
        assert_eq!(frames[1].line, Some(9));
        assert!(frames[0]
            .file
            .as_deref()
            .unwrap()
            .ends_with("inline_test.c"));
    }
}
