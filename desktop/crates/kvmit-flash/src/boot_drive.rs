use super::{err, FlashError, Image, Partition, DATA_FAT_SUBTYPE, DATA_TYPE};
use std::io::{Cursor, Write};
use std::ops::Range;
use url::Url;

const SECTOR_BYTES: usize = 512;

/// An HTTP(S) destination suitable for a single iPXE script argument.
#[derive(Clone, Debug)]
pub struct BootUrl {
    url: Url,
    append_mac: bool,
}

impl BootUrl {
    pub fn new(input: &str, append_mac: bool) -> Result<Self, FlashError> {
        let authority = input
            .strip_prefix("https://")
            .or_else(|| input.strip_prefix("http://"));
        if input.len() > 1024
            || authority.is_none_or(|value| value.is_empty() || value.starts_with('/'))
            || !input
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b":/[]-._~%?&=+,".contains(&b))
        {
            return err("Enter an HTTP(S) boot URL with no spaces or iPXE commands.");
        }
        let url = Url::parse(input).map_err(|e| FlashError(format!("Invalid boot URL: {e}")))?;
        if !url.has_host()
            || url.fragment().is_some()
            || !url.username().is_empty()
            || url.password().is_some()
        {
            return err("The boot URL needs a host and must omit credentials and fragments.");
        }
        if append_mac && url.query().is_some() {
            return err("A boot URL that appends the Ethernet MAC must omit the query string.");
        }
        Ok(Self { url, append_mac })
    }

    fn script(&self) -> String {
        let destination = if self.append_mac {
            format!("{}/${{netX/mac}}", self.url.as_str().trim_end_matches('/'))
        } else {
            self.url.to_string()
        };
        format!(
            "#!ipxe\nprompt --timeout 5000 kvm-it: press any key to boot from the configured URL... || exit 1\n\
             dhcp || goto dhcp_failed\nchain {destination} || goto boot_failed\nexit 1\n\
             :dhcp_failed\necho kvm-it: no DHCP address. Check the wired Ethernet connection.\nifstat\nexit 1\n\
             :boot_failed\necho kvm-it: the configured boot URL failed.\nexit 1\n"
        )
    }
}

impl Image {
    pub(super) fn boot_drive_partition(&self) -> Result<Partition, FlashError> {
        let part = self
            .parts
            .get(3)
            .ok_or_else(|| FlashError("This firmware bundle has no boot-drive image.".into()))?;
        self.table()?
            .into_iter()
            .find(|p| {
                p.name == "ipxe"
                    && p.offset == part.offset
                    && p.ptype == DATA_TYPE
                    && p.subtype == DATA_FAT_SUBTYPE
                    && p.flags == 0
                    && p.size as usize == part.data.len()
            })
            .ok_or_else(|| {
                FlashError(
                    "The boot-drive image must fill the bundle's unencrypted ipxe partition."
                        .into(),
                )
            })
    }

    /// Rewrite only the autoexec file in the validated bundled boot-drive image.
    pub fn set_boot_url(&mut self, url: &BootUrl) -> Result<(), FlashError> {
        self.validate()?;
        self.boot_drive_partition()?;
        let part = &mut self.parts[3];
        let mut data = part.data.clone();
        let volume = boot_volume(&data)?;
        let fs = fatfs::FileSystem::new(Cursor::new(&mut data[volume]), fatfs::FsOptions::new())
            .map_err(fat_error)?;
        {
            let root = fs.root_dir();
            let mut file = root.open_file("autoexec.ipxe").map_err(fat_error)?;
            file.truncate().map_err(fat_error)?;
            file.write_all(url.script().as_bytes()).map_err(fat_error)?;
        }
        fs.unmount().map_err(fat_error)?;
        part.data = data;
        Ok(())
    }
}

pub(super) fn check_installed_partition(
    installed: &[Partition],
    expected: &Partition,
) -> Result<(), FlashError> {
    if !installed.contains(expected) {
        return err("The installed iPXE partition differs from this bundle. Update the adapter firmware first.");
    }
    Ok(())
}

fn boot_volume(data: &[u8]) -> Result<Range<usize>, FlashError> {
    let Some(entry) = data.get(446..462) else {
        return err("The boot drive is missing its EFI partition.");
    };
    let first = u32::from_le_bytes(entry[8..12].try_into().unwrap()) as usize;
    let count = u32::from_le_bytes(entry[12..16].try_into().unwrap()) as usize;
    let end = first
        .checked_add(count)
        .and_then(|value| value.checked_mul(SECTOR_BYTES));
    match end {
        Some(end) if entry[4] == 0xef && first > 0 && count > 0 && end <= data.len() => {
            Ok(first * SECTOR_BYTES..end)
        }
        _ => err("The boot drive has an invalid EFI partition."),
    }
}

fn fat_error(error: std::io::Error) -> FlashError {
    FlashError(format!(
        "Could not edit the boot drive's autoexec.ipxe: {error}"
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read;
    use std::path::Path;

    fn bundle() -> Image {
        Image::from_build_dir(
            &Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../firmware/release"),
        )
        .unwrap()
    }

    fn read_file(data: &[u8], name: &str) -> Vec<u8> {
        let fs = fatfs::FileSystem::new(
            Cursor::new(data[boot_volume(data).unwrap()].to_vec()),
            fatfs::FsOptions::new(),
        )
        .unwrap();
        let mut contents = Vec::new();
        fs.root_dir()
            .open_file(name)
            .unwrap()
            .read_to_end(&mut contents)
            .unwrap();
        contents
    }

    #[test]
    fn packaged_image_keeps_its_signed_loaders_and_requires_a_key() {
        let mut image = bundle();
        let before = image.clone();
        let url = BootUrl::new("http://192.168.68.60:8080/boot/", true).unwrap();
        image.set_boot_url(&url).unwrap();
        image.validate().unwrap();
        assert_eq!(image.parts[..3], before.parts[..3]);
        let old_drive = &before.parts[3].data;
        let drive = &image.parts[3].data;
        assert_eq!(drive.len(), old_drive.len());
        for path in ["EFI/BOOT/BOOTX64.EFI", "EFI/BOOT/IPXE.EFI"] {
            assert_eq!(read_file(drive, path), read_file(old_drive, path));
        }
        let script = String::from_utf8(read_file(drive, "autoexec.ipxe")).unwrap();
        assert!(script.lines().nth(1).unwrap().contains("--timeout 5000"));
        assert!(script.lines().nth(1).unwrap().ends_with("|| exit 1"));
        assert!(
            script.contains("chain http://192.168.68.60:8080/boot/${netX/mac} || goto boot_failed")
        );
        assert!(!script.contains("boot.ipxe.org"));
    }

    #[test]
    fn fleet_url_accepts_static_https_and_refuses_script_syntax() {
        let url = BootUrl::new("https://boot.example.test/windows11/install.ipxe", false).unwrap();
        assert!(url
            .script()
            .contains("chain https://boot.example.test/windows11/install.ipxe "));
        for input in [
            "ftp://example.test/file",
            "http://",
            "http:///example.test",
            "https://user:pass@example.test/file",
            "https://example.test/script#value",
            "https://example.test/script || shell",
            "https://example.test/script\nreboot",
            "https://example.test/${script}",
        ] {
            assert!(BootUrl::new(input, false).is_err(), "{input:?}");
        }
        assert!(BootUrl::new("https://example.test/boot?key=value", true).is_err());
    }

    #[test]
    fn update_requires_the_same_installed_partition_and_writes_only_that_range() {
        let image = bundle();
        let expected = image.boot_drive_partition().unwrap();
        let mut installed = image.table().unwrap();
        assert!(check_installed_partition(&installed, &expected).is_ok());
        installed
            .iter_mut()
            .find(|part| part.name == "ipxe")
            .unwrap()
            .flags = 1;
        assert!(check_installed_partition(&installed, &expected).is_err());
        let parts = super::super::write_parts(&image, true);
        assert_eq!(parts[0].offset, expected.offset);
        assert_eq!(
            parts.iter().map(|part| part.data.len()).sum::<usize>(),
            expected.size as usize
        );
        assert!(parts.iter().all(|part| part.offset >= expected.offset
            && part.offset + part.data.len() as u32 <= expected.offset + expected.size));
    }
}
