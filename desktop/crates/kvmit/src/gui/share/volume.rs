use std::collections::HashSet;
use std::fs::{self, OpenOptions};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};

use fatfs::{FileSystem, FormatVolumeOptions, FsOptions, ReadWriteSeek};

const LIMIT: u64 = 8 * 1024 * 1024;
const COUNT: usize = 128;
const OUT: &str = "From host";
const BACK: &str = "To host";

fn valid_name(name: &str) -> bool {
    let first = name
        .split('.')
        .next()
        .unwrap_or("")
        .trim_end()
        .to_uppercase();
    !name.is_empty()
        && name.encode_utf16().count() <= 240
        && !name.ends_with(['.', ' '])
        && !name
            .chars()
            .any(|c| c.is_control() || r#"<>:"/\|?*"#.contains(c))
        && !matches!(
            first.as_str(),
            "CON" | "PRN" | "AUX" | "NUL" | "CONIN$" | "CONOUT$"
        )
        && !["COM", "LPT"].iter().any(|p| {
            first.strip_prefix(p).is_some_and(|s| {
                matches!(s, "¹" | "²" | "³") || s.len() == 1 && s.as_bytes()[0].is_ascii_digit()
            })
        })
}

pub(super) fn mount<T: ReadWriteSeek>(mut disk: T, initialize: bool) -> io::Result<FileSystem<T>> {
    let mut boot = [0; 512];
    disk.read_exact(&mut boot)?;
    disk.rewind()?;
    if initialize && boot.iter().all(|b| *b == 0xff) {
        // Publish the boot sector last. A cable pull during first-time formatting
        // leaves the erased signature in place so the next attempt can finish.
        let capacity = disk.seek(std::io::SeekFrom::End(0))?;
        if !(1024 * 1024..=LIMIT).contains(&capacity) || capacity % 512 != 0 {
            return Err(io::Error::other("Unsupported exchange volume capacity."));
        }
        let mut image = io::Cursor::new(vec![0xff; capacity as usize]);
        fatfs::format_volume(
            &mut image,
            FormatVolumeOptions::new()
                .volume_label(*b"KVM SHARE  ")
                .bytes_per_cluster(2048),
        )?;
        let image = image.into_inner();
        for (index, sector) in image.as_chunks::<512>().0.iter().enumerate().skip(1) {
            if sector.iter().any(|b| *b != 0xff) {
                disk.seek(std::io::SeekFrom::Start(index as u64 * 512))?;
                disk.write_all(sector)?;
            }
        }
        disk.flush()?;
        disk.rewind()?;
        disk.write_all(&image[..512])?;
        disk.flush()?;
        disk.rewind()?;
    }
    let fs = FileSystem::new(disk, FsOptions::new()).map_err(|_| {
        io::Error::other("Cannot mount KVM SHARE. Its existing files have been preserved.")
    })?;
    if initialize {
        fs.root_dir().create_dir(OUT)?;
        fs.root_dir().create_dir(BACK)?;
    }
    Ok(fs)
}

pub(super) fn send<T: ReadWriteSeek>(
    fs: &FileSystem<T>,
    paths: &[PathBuf],
    batch: &str,
    progress: &dyn Fn(u64, u64) -> io::Result<()>,
) -> io::Result<()> {
    if paths.is_empty() || paths.len() > COUNT {
        return Err(io::Error::other("Copy 1 to 128 individual files first."));
    }
    let mut entries = Vec::new();
    let mut names = HashSet::new();
    let mut total = 0;
    for path in paths {
        let name = path
            .file_name()
            .and_then(|n| n.to_str())
            .filter(|n| valid_name(n))
            .ok_or_else(|| io::Error::other("Unsupported filename."))?;
        let metadata = path.symlink_metadata()?;
        if !metadata.is_file() || !names.insert(name.to_uppercase()) {
            return Err(io::Error::other(
                "Select regular files with distinct names.",
            ));
        }
        total += metadata.len().min(LIMIT + 1);
        if total > LIMIT {
            return Err(io::Error::other(
                "These files exceed the adapter’s 8 MiB exchange capacity.",
            ));
        }
        entries.push((path, name, metadata.len()));
    }
    let stats = fs.stats()?;
    if total > stats.free_clusters() as u64 * stats.cluster_size() as u64 {
        return Err(io::Error::other(
            "The exchange drive is full. Remove earlier files on the target and eject it.",
        ));
    }
    let root = fs.root_dir();
    let out = root.open_dir(OUT)?;
    let stage = format!("Pending-{batch}");
    let staged = root.create_dir(&stage)?;
    let mut done = 0;
    let mut buffer = [0; 4096];
    let result = (|| {
        progress(0, total)?;
        for (path, name, len) in &entries {
            let mut source = fs::File::open(path)?;
            let mut dest = staged.create_file(name)?;
            let mut left = *len;
            while left > 0 {
                let n = left.min(buffer.len() as u64) as usize;
                source.read_exact(&mut buffer[..n])?;
                dest.write_all(&buffer[..n])?;
                left -= n as u64;
                done += n as u64;
                progress(done, total)?;
            }
            if source.read(&mut buffer[..1])? != 0 {
                return Err(io::Error::other("A copied file changed while sending."));
            }
            dest.flush()?;
        }
        root.rename(&stage, &out, batch)
    })();
    if result.is_err() {
        for (_, name, _) in &entries {
            let _ = staged.remove(name);
        }
        let _ = root.remove(&stage);
    }
    result
}

pub(super) fn collect<T: ReadWriteSeek>(
    fs: &FileSystem<T>,
    parent: &Path,
    progress: &dyn Fn(u64, u64) -> io::Result<()>,
) -> io::Result<Vec<PathBuf>> {
    let dir = fs.root_dir().open_dir(BACK)?;
    let mut names = HashSet::new();
    let mut items = Vec::new();
    let mut total = 0u64;
    for (i, item) in dir.iter().enumerate() {
        if i >= COUNT + 2 {
            return Err(io::Error::other("Collect up to 128 files at a time."));
        }
        let item = item?;
        let name = item.file_name();
        if name == "." || name == ".." {
            continue;
        }
        if !item.is_file() || !valid_name(&name) || !names.insert(name.to_uppercase()) {
            return Err(io::Error::other(
                "The To host folder must contain individual files with distinct, valid names.",
            ));
        }
        total = total
            .checked_add(item.len())
            .filter(|n| *n <= LIMIT)
            .ok_or_else(|| io::Error::other("Invalid exchange drive size."))?;
        items.push((name, item.len()));
    }
    if items.is_empty() {
        return Err(io::Error::other("The To host folder is empty."));
    }
    fs::create_dir_all(parent)?;
    let directory = tempfile::Builder::new()
        .prefix("Transfer-")
        .tempdir_in(parent)?;
    let mut done = 0;
    let mut buf = [0; 4096];
    progress(0, total)?;
    for (name, len) in &items {
        let mut src = dir.open_file(name)?;
        let mut dst = OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(directory.path().join(name))?;
        let mut left = *len;
        while left > 0 {
            let n = left.min(buf.len() as u64) as usize;
            src.read_exact(&mut buf[..n])?;
            dst.write_all(&buf[..n])?;
            left -= n as u64;
            done += n as u64;
            progress(done, total)?;
        }
        dst.sync_all()?;
    }
    let dir = directory.keep();
    Ok(items.into_iter().map(|(name, _)| dir.join(name)).collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Seek;
    #[test]
    fn exchange_round_trip_uses_a_real_fat_volume_and_preserves_binary_files() {
        let mut disk = io::Cursor::new(vec![0xff; 2 * 1024 * 1024]);
        let files = tempfile::tempdir().unwrap();
        let name = "Long file name 01.bin";
        let src = files.path().join(name);
        let bytes: Vec<_> = (0..15000).map(|n| (n % 251) as u8).collect();
        fs::write(&src, &bytes).unwrap();
        let fs = mount(&mut disk, true).unwrap();
        send(&fs, &[src], "Batch", &|_, _| Ok(())).unwrap();
        fs.unmount().unwrap();
        disk.rewind().unwrap();
        let target = mount(&mut disk, false).unwrap();
        let root = target.root_dir();
        let mut got = Vec::new();
        root.open_file(&format!("{OUT}/Batch/{name}"))
            .unwrap()
            .read_to_end(&mut got)
            .unwrap();
        assert_eq!(got, bytes);
        root.create_file(&format!("{BACK}/{name}"))
            .unwrap()
            .write_all(&got)
            .unwrap();
        drop(root);
        target.unmount().unwrap();
        disk.rewind().unwrap();
        let fs = mount(&mut disk, false).unwrap();
        let paths = collect(&fs, files.path(), &|_, _| Ok(())).unwrap();
        assert_eq!(fs::read(&paths[0]).unwrap(), bytes);
        assert_eq!(paths[0].file_name().unwrap(), name);
    }
    #[test]
    fn reserved_names_and_unrecognized_disks_are_rejected() {
        for name in ["../x", "LPT1.txt", "com¹.x", "z:s", "a.", "con .txt"] {
            assert!(!valid_name(name));
        }
        let mut disk = io::Cursor::new(vec![0x6a; 8192]);
        assert!(mount(&mut disk, true).is_err());
        assert!(disk.into_inner().iter().all(|b| *b == 0x6a));
    }
}
