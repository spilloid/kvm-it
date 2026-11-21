use std::io::{self, Read, Seek, SeekFrom, Write};
use std::time::{Duration, Instant};

const BLOCK: usize = 512;

pub(super) fn hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8] = b"0123456789abcdef";
    let mut s = String::with_capacity(bytes.len() * 2);
    for &b in bytes {
        s.push(DIGITS[(b >> 4) as usize] as char);
        s.push(DIGITS[(b & 15) as usize] as char);
    }
    s
}

fn decode_sector(value: &str) -> io::Result<[u8; BLOCK]> {
    let mut out = [0; BLOCK];
    if !value.is_ascii() || value.len() != 2 * BLOCK {
        return Err(io::Error::other("Invalid sector size"));
    }
    for (i, b) in out.iter_mut().enumerate() {
        *b = u8::from_str_radix(&value[i * 2..i * 2 + 2], 16).map_err(io::Error::other)?;
    }
    Ok(out)
}

pub(super) struct Cable {
    port: Box<dyn serialport::SerialPort>,
    lease: String,
    sequence: u64,
}

impl Cable {
    pub(super) fn open(name: &str) -> io::Result<Self> {
        let port = serialport::new(name, 115200)
            .timeout(Duration::from_millis(100))
            .dtr_on_open(false)
            .open()
            .map_err(io::Error::other)?;
        let id = getrandom::u64().map_err(|_| io::Error::other("Cannot create transfer ID"))?;
        Ok(Self {
            port,
            lease: format!("{id:016x}"),
            sequence: id,
        })
    }

    pub(super) fn request(&mut self, command: &str) -> io::Result<String> {
        self.sequence = self.sequence.wrapping_add(1);
        let id = format!("{:016x}", self.sequence);
        let body = format!("K8 {id} {} {command}", self.lease);
        let frame = format!("\n{body} {:04x}\n", kvmit_protocol::crc16(body.as_bytes()));
        let mut line = Vec::with_capacity(1200);
        for _ in 0..3 {
            self.port.write_all(frame.as_bytes())?;
            let deadline = Instant::now() + Duration::from_millis(1250);
            while Instant::now() < deadline {
                let mut buf = [0; 256];
                let n = match self.port.read(&mut buf) {
                    Ok(n) => n,
                    Err(e)
                        if matches!(
                            e.kind(),
                            io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock
                        ) =>
                    {
                        continue
                    }
                    Err(e) => return Err(e),
                };
                for &b in &buf[..n] {
                    if b == b'\n' {
                        let reply = parse_reply(&line, &id);
                        line.clear();
                        if let Some(result) = reply {
                            return result;
                        }
                    } else if line.len() < 1200 {
                        line.push(b);
                    }
                }
            }
        }
        Err(io::Error::other("No reply on the COM cable. Check the cable, firmware 0.3.0 and that boot drive mode is off."))
    }
}

fn parse_reply(line: &[u8], id: &str) -> Option<io::Result<String>> {
    let (body, crc) = std::str::from_utf8(line).ok()?.rsplit_once(' ')?;
    if crc.len() != 4
        || kvmit_protocol::crc16(body.as_bytes()) != u16::from_str_radix(crc, 16).ok()?
    {
        return None;
    }
    let mut fields = body.split(' ');
    if fields.next()? != "K8R" || fields.next()? != id {
        return None;
    }
    let status = fields.next()?;
    let value = fields.next()?;
    if fields.next().is_some() {
        return None;
    }
    Some(match status {
        "OK" => Ok(value.to_owned()),
        "EJECT_FIRST" => Err(io::Error::other(
            "Eject KVM SHARE on the target, then try again.",
        )),
        "INPUT_ACTIVE" => Err(io::Error::other(
            "Release target input, then choose Show drive on target.",
        )),
        "WRONG_BOARD" => Err(io::Error::other(
            "This cable belongs to a different console.",
        )),
        "BUSY" => Err(io::Error::other("Another transfer is using this adapter.")),
        _ => Err(io::Error::other(
            "Adapter could not complete this disk operation. Check the cable and try again.",
        )),
    })
}

// FAT alternates between directory entries, allocation tables and file data.
// Keep confirmed sectors only for this exclusive lease. Switching blocks still
// flushes pending writes in order before any other block can be accessed.
const CACHE_SLOTS: usize = 32;
pub(super) struct Disk<'a> {
    request: &'a mut dyn FnMut(&str) -> io::Result<String>,
    size: u64,
    pos: u64,
    block: Option<u64>,
    dirty: bool,
    bytes: [u8; BLOCK],
    cache: [Option<(u64, [u8; BLOCK])>; CACHE_SLOTS],
    failed: bool,
    deadline: Instant,
}

impl<'a> Disk<'a> {
    pub(super) fn new(request: &'a mut dyn FnMut(&str) -> io::Result<String>, blocks: u32) -> Self {
        Self {
            request,
            size: blocks as u64 * BLOCK as u64,
            pos: 0,
            block: None,
            dirty: false,
            bytes: [0; BLOCK],
            cache: [None; CACHE_SLOTS],
            failed: false,
            deadline: Instant::now() + Duration::from_secs(3600),
        }
    }
    fn command(&mut self, cmd: &str) -> io::Result<String> {
        if self.failed || Instant::now() > self.deadline {
            return Err(io::Error::other("File operation interrupted."));
        }
        let reply = (self.request)(cmd);
        self.failed |= reply.is_err();
        reply
    }
    fn load(&mut self, block: u64, whole_write: bool) -> io::Result<()> {
        if self.block == Some(block) {
            return Ok(());
        }
        self.flush()?;
        if !whole_write {
            let slot = block as usize % CACHE_SLOTS;
            if let Some((_, bytes)) = self.cache[slot].as_ref().filter(|(id, _)| *id == block) {
                self.bytes = *bytes;
            } else {
                self.bytes = decode_sector(&self.command(&format!("R {block}"))?)?;
                self.cache[slot] = Some((block, self.bytes));
            }
        }
        self.block = Some(block);
        Ok(())
    }
}
impl Read for Disk<'_> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if self.pos >= self.size || buf.is_empty() {
            return Ok(0);
        }
        self.load(self.pos / BLOCK as u64, false)?;
        let off = (self.pos % BLOCK as u64) as usize;
        let n = buf.len().min(BLOCK - off);
        buf[..n].copy_from_slice(&self.bytes[off..off + n]);
        self.pos += n as u64;
        Ok(n)
    }
}
impl Write for Disk<'_> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        if self.pos >= self.size || buf.is_empty() {
            return Ok(0);
        }
        let off = (self.pos % BLOCK as u64) as usize;
        let n = buf.len().min(BLOCK - off);
        self.load(self.pos / BLOCK as u64, n == BLOCK)?;
        self.bytes[off..off + n].copy_from_slice(&buf[..n]);
        self.dirty = true;
        self.pos += n as u64;
        Ok(n)
    }
    fn flush(&mut self) -> io::Result<()> {
        if self.dirty {
            let block = self.block.unwrap();
            let slot = block as usize % CACHE_SLOTS;
            if self.cache[slot] != Some((block, self.bytes)) {
                self.command(&format!("W {block} {}", hex(&self.bytes)))?;
                self.cache[slot] = Some((block, self.bytes));
            }
            self.dirty = false;
        }
        Ok(())
    }
}
impl Seek for Disk<'_> {
    fn seek(&mut self, p: SeekFrom) -> io::Result<u64> {
        let pos = match p {
            SeekFrom::Start(n) => Some(n),
            SeekFrom::Current(n) => self.pos.checked_add_signed(n),
            SeekFrom::End(n) => self.size.checked_add_signed(n),
        }
        .filter(|n| *n <= self.size)
        .ok_or_else(|| io::Error::other("Drive offset is outside the exchange partition."))?;
        self.pos = pos;
        Ok(pos)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn revisiting_sectors_reuses_confirmed_bytes_and_flushes_edits_in_order() {
        let mut storage = [[5; BLOCK]; CACHE_SLOTS + 1];
        let mut log = Vec::new();
        let mut request = |command: &str| {
            let mut fields = command.split_whitespace();
            let op = fields.next().unwrap();
            let index: usize = fields.next().unwrap().parse().unwrap();
            log.push((op.to_owned(), index));
            if op == "R" {
                return Ok(hex(&storage[index]));
            }
            assert_eq!(op, "W");
            storage[index] = decode_sector(fields.next().unwrap())?;
            Ok("-".into())
        };
        let mut disk = Disk::new(&mut request, CACHE_SLOTS as u32 + 1);
        let mut byte = [0];
        for sector in [0, 1, 0, 1] {
            disk.seek(SeekFrom::Start(sector * BLOCK as u64)).unwrap();
            disk.read_exact(&mut byte).unwrap();
        }
        disk.write_all(&[9]).unwrap();
        disk.rewind().unwrap();
        disk.write_all(&[5]).unwrap(); // identical to the confirmed sector
        disk.seek(SeekFrom::Start((CACHE_SLOTS * BLOCK) as u64))
            .unwrap();
        disk.write_all(&[7; BLOCK]).unwrap(); // replaces sector 0's cache slot
        disk.rewind().unwrap();
        disk.read_exact(&mut byte).unwrap();
        assert_eq!(byte, [5]);
        assert_eq!(storage[1][..3], [5, 9, 5]);
        assert_eq!(storage[CACHE_SLOTS], [7; BLOCK]);
        let commands: Vec<_> = log.iter().map(|(op, i)| (op.as_str(), *i)).collect();
        assert_eq!(
            commands,
            [("R", 0), ("R", 1), ("W", 1), ("W", CACHE_SLOTS), ("R", 0)]
        );
    }

    fn frame(body: &str) -> String {
        format!("{body} {:04x}", kvmit_protocol::crc16(body.as_bytes()))
    }
    #[test]
    fn uart_noise_damaged_frames_and_late_acknowledgements_cannot_confirm_writes() {
        let id = "1234567812345678";
        let reply = frame(&format!("K8R {id} OK -"));
        assert_eq!(parse_reply(reply.as_bytes(), id).unwrap().unwrap(), "-");
        assert!(parse_reply(reply.as_bytes(), "0000000000000000").is_none());
        assert!(parse_reply(reply.replace(" OK ", " NO ").as_bytes(), id).is_none());
        assert!(parse_reply(b"USB: configured by target", id).is_none());
        assert!(
            parse_reply(frame(&format!("K8R {id} EJECT_FIRST -")).as_bytes(), id)
                .unwrap()
                .is_err()
        );
        let sector = [0xab; 512];
        assert_eq!(decode_sector(&hex(&sector)).unwrap(), sector);
        assert!(decode_sector(&"xx".repeat(512)).is_err());
    }
}
