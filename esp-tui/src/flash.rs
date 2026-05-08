use std::path::Path;

use anyhow::Context;
use espflash::connection::reset::{ResetAfterOperation, ResetBeforeOperation};
use espflash::flasher::{
    FlashDataBuilder, FlashSettings, Flasher, ProgressCallbacks,
};
use serialport::{FlowControl, UsbPortInfo};
use tokio::sync::mpsc::UnboundedSender;

use crate::event::Message;

/// The current flash operation state.
pub(crate) enum State {
    /// No flash operation in progress.
    Idle,
    /// A flash operation is in progress.
    Flashing {
        addr: u32,
        current: usize,
        total: usize,
    },
    /// An erase operation is in progress.
    Erasing,
    /// Flash or erase completed; waiting for the device to reconnect.
    Reconnecting,
}

/// Information about the connected ESP32 device.
pub(crate) struct DeviceInfo {
    chip: String,
    flash_size: String,
    mac_address: String,
    partitions: Vec<PartitionEntry>,
}

impl DeviceInfo {
    /// Constructs a `DeviceInfo` from its components.
    ///
    /// # Arguments
    ///
    /// * `chip` - Chip string (e.g. `"ESP32-S3 (rev v0.1)"`).
    /// * `flash_size` - Flash size string (e.g. `"4MB"`).
    /// * `mac_address` - MAC address string (e.g. `"AA:BB:CC:DD:EE:FF"`).
    /// * `partitions` - Partition entries read from the device.
    ///
    /// # Returns
    ///
    /// A [`DeviceInfo`] with the given fields.
    #[must_use]
    pub(crate) fn new(
        chip: impl Into<String>,
        flash_size: impl Into<String>,
        mac_address: impl Into<String>,
        partitions: Vec<PartitionEntry>,
    ) -> Self {
        Self {
            chip: chip.into(),
            flash_size: flash_size.into(),
            mac_address: mac_address.into(),
            partitions,
        }
    }

    /// Returns the chip type and revision string.
    ///
    /// # Returns
    ///
    /// A string like `"ESP32-S3 (rev v0.1)"`.
    #[must_use]
    pub(crate) fn chip(&self) -> &str {
        &self.chip
    }

    /// Returns the flash size string.
    ///
    /// # Returns
    ///
    /// A string like `"4MB"`.
    #[must_use]
    pub(crate) fn flash_size(&self) -> &str {
        &self.flash_size
    }

    /// Returns the MAC address string.
    ///
    /// # Returns
    ///
    /// A string like `"AA:BB:CC:DD:EE:FF"`.
    #[must_use]
    pub(crate) fn mac_address(&self) -> &str {
        &self.mac_address
    }

    /// Returns the partition entries read from the device.
    ///
    /// # Returns
    ///
    /// A slice of [`PartitionEntry`] values, or an empty slice if the
    /// partition table could not be read.
    #[must_use]
    pub(crate) fn partitions(&self) -> &[PartitionEntry] {
        &self.partitions
    }
}

/// A single entry in the device partition table.
pub(crate) struct PartitionEntry {
    name: String,
    partition_type: String,
    subtype: String,
    offset: u32,
    size: u32,
}

impl PartitionEntry {
    /// Returns the partition name.
    ///
    /// # Returns
    ///
    /// The partition name as a string slice (e.g. `"nvs"` or `"factory"`).
    #[must_use]
    pub(crate) fn name(&self) -> &str {
        &self.name
    }

    /// Returns the partition type string.
    ///
    /// # Returns
    ///
    /// A string like `"app"` or `"data"`.
    #[must_use]
    pub(crate) fn partition_type(&self) -> &str {
        &self.partition_type
    }

    /// Returns the partition subtype string.
    ///
    /// # Returns
    ///
    /// A string like `"factory"` or `"nvs"`.
    #[must_use]
    pub(crate) fn subtype(&self) -> &str {
        &self.subtype
    }

    /// Returns the partition byte offset in flash.
    ///
    /// # Returns
    ///
    /// The byte offset from the start of flash (e.g. `0x010000`).
    #[must_use]
    pub(crate) fn offset(&self) -> u32 {
        self.offset
    }

    /// Returns the partition size in bytes.
    ///
    /// # Returns
    ///
    /// The partition size in bytes (e.g. `1048576` for 1 MB).
    #[must_use]
    pub(crate) fn size(&self) -> u32 {
        self.size
    }
}

struct TuiProgress {
    tx: UnboundedSender<Message>,
    addr: u32,
    total: usize,
}

impl ProgressCallbacks for TuiProgress {
    fn init(&mut self, addr: u32, total: usize) {
        self.addr = addr;
        self.total = total;
        let _ = self.tx.send(Message::FlashProgress {
            addr,
            current: 0,
            total,
        });
    }

    fn update(&mut self, current: usize) {
        let _ = self.tx.send(Message::FlashProgress {
            addr: self.addr,
            current,
            total: self.total,
        });
    }

    fn finish(&mut self) {
        let _ = self.tx.send(Message::FlashProgress {
            addr: self.addr,
            current: self.total,
            total: self.total,
        });
    }
}

fn open_flasher(
    port_name: &str,
    baud: u32,
    use_stub: bool,
) -> anyhow::Result<Flasher> {
    let serial = serialport::new(port_name, baud)
        .flow_control(FlowControl::None)
        .open_native()
        .with_context(|| format!("failed to open {port_name}"))?;

    let port_info = UsbPortInfo {
        vid: 0,
        pid: 0,
        serial_number: None,
        manufacturer: None,
        product: None,
    };

    Flasher::connect(
        serial,
        port_info,
        None,
        use_stub,
        false,
        false,
        None,
        ResetAfterOperation::NoReset,
        ResetBeforeOperation::DefaultReset,
    )
    .with_context(|| format!("failed to connect to {port_name}"))
}

fn collect_device_info(flasher: &mut Flasher) -> anyhow::Result<DeviceInfo> {
    let info = flasher
        .device_info()
        .context("failed to read device info")?;

    let chip = info.revision.map_or_else(
        || format!("{}", info.chip),
        |(major, minor)| format!("{} (rev v{major}.{minor})", info.chip),
    );

    Ok(DeviceInfo::new(
        chip,
        format!("{}", info.flash_size),
        info.mac_address,
        Vec::new(),
    ))
}

/// Flashes an ELF firmware image to the connected device.
///
/// Reads the ELF file, opens the port via espflash, and calls
/// `load_elf_to_flash`, sending [`Message::FlashProgress`] events while
/// writing. After the flasher connects successfully, a
/// [`Message::DeviceInfo`] is sent as a free side effect so board info is
/// refreshed on every flash.
///
/// # Arguments
///
/// * `port_name` - System port name.
/// * `baud` - Baud rate.
/// * `elf_path` - Path to the ELF firmware file.
/// * `tx` - Event sender for [`Message::FlashProgress`] and
///   [`Message::DeviceInfo`] updates.
///
/// # Errors
///
/// Returns an error if the ELF file cannot be read, the port cannot be
/// opened, or flashing fails.
pub(crate) fn flash_elf(
    port_name: &str,
    baud: u32,
    elf_path: &Path,
    tx: UnboundedSender<Message>,
) -> anyhow::Result<()> {
    let elf_data = std::fs::read(elf_path).context("failed to read ELF file")?;

    let mut flasher = open_flasher(port_name, baud, true)?;

    let _ = tx.send(Message::DeviceInfo(collect_device_info(&mut flasher)));

    let chip = flasher.chip();
    let xtal_freq = chip
        .into_target()
        .crystal_freq(flasher.connection())
        .context("failed to detect crystal frequency")?;

    let flash_data = FlashDataBuilder::new()
        .with_flash_settings(FlashSettings::default())
        .build()
        .context("failed to build flash data")?;

    let mut progress = TuiProgress {
        tx,
        addr: 0,
        total: 0,
    };

    flasher
        .load_elf_to_flash(&elf_data, flash_data, Some(&mut progress), xtal_freq)
        .context("failed to flash ELF")
}

/// Erases the entire flash of the connected device.
///
/// Opens the port via espflash and calls `erase_flash`.
///
/// # Arguments
///
/// * `port_name` - System port name.
/// * `baud` - Baud rate.
///
/// # Errors
///
/// Returns an error if the port cannot be opened or the erase command fails.
pub(crate) fn erase_flash(port_name: &str, baud: u32) -> anyhow::Result<()> {
    let mut flasher = open_flasher(port_name, baud, true)?;
    flasher.erase_flash().context("failed to erase flash")
}

#[cfg(test)]
mod tests {
    use tokio::sync::mpsc;

    use super::*;

    #[test]
    fn tui_progress_sends_flash_progress_messages() {
        let (tx, mut rx) = mpsc::unbounded_channel();
        let mut progress = TuiProgress {
            tx,
            addr: 0,
            total: 0,
        };

        progress.init(0x1000, 4096);
        let msg = rx.try_recv().unwrap();
        assert!(matches!(
            msg,
            Message::FlashProgress {
                addr: 0x1000,
                current: 0,
                total: 4096
            }
        ));

        progress.update(1024);
        let msg = rx.try_recv().unwrap();
        assert!(matches!(
            msg,
            Message::FlashProgress {
                addr: 0x1000,
                current: 1024,
                total: 4096
            }
        ));

        progress.finish();
        let msg = rx.try_recv().unwrap();
        assert!(matches!(
            msg,
            Message::FlashProgress {
                addr: 0x1000,
                current: 4096,
                total: 4096
            }
        ));
    }

    #[test]
    fn device_info_accessors() {
        let info = DeviceInfo::new(
            "ESP32-S3",
            "4MB",
            "AA:BB:CC:DD:EE:FF",
            vec![PartitionEntry {
                name: "nvs".into(),
                partition_type: "data".into(),
                subtype: "nvs".into(),
                offset: 0x9000,
                size: 0x6000,
            }],
        );

        assert_eq!(info.chip(), "ESP32-S3");
        assert_eq!(info.flash_size(), "4MB");
        assert_eq!(info.mac_address(), "AA:BB:CC:DD:EE:FF");
        assert_eq!(info.partitions().len(), 1);
        let p = &info.partitions()[0];
        assert_eq!(p.name(), "nvs");
        assert_eq!(p.partition_type(), "data");
        assert_eq!(p.subtype(), "nvs");
        assert_eq!(p.offset(), 0x9000);
        assert_eq!(p.size(), 0x6000);
    }
}
