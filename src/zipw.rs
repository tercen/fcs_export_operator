//! A ZIP writer for entries compressed elsewhere, so the files can be deflated in parallel.
//!
//! The `zip` crate compresses as it writes, one entry at a time. Here each FCS file is deflated
//! on its own thread ([`Entry::deflate`]) and the archive is only stitched together in order:
//! local header, compressed bytes, and the central directory at the end. Names are flat, the
//! timestamp is fixed (1980-01-01 00:00), so the same files give the same archive byte for byte.
//! No ZIP64: an archive over 4 GiB or 65,535 entries is refused rather than written broken.
use std::io::Write;

use anyhow::{Result, bail};

/// One compressed member.
pub struct Entry {
    pub name: String,
    pub crc: u32,
    pub size: u64,
    pub data: Vec<u8>,
    /// 8 = deflate, 0 = stored.
    pub method: u16,
}

impl Entry {
    /// Deflate `bytes` (level 6); kept stored when deflate does not make it smaller.
    pub fn deflate(name: String, bytes: &[u8]) -> Result<Entry> {
        use flate2::{Compression, write::DeflateEncoder};
        let crc = crc32fast::hash(bytes);
        let mut enc = DeflateEncoder::new(Vec::with_capacity(bytes.len() / 2), Compression::new(6));
        enc.write_all(bytes)?;
        let data = enc.finish()?;
        Ok(if data.len() < bytes.len() {
            Entry {
                name,
                crc,
                size: bytes.len() as u64,
                data,
                method: 8,
            }
        } else {
            Entry {
                name,
                crc,
                size: bytes.len() as u64,
                data: bytes.to_vec(),
                method: 0,
            }
        })
    }
}

const DOS_DATE_1980_01_01: u16 = (1 << 5) | 1;
const UTF8_NAMES: u16 = 1 << 11;

/// Writes entries in the order given, then the central directory on [`ZipWriter::finish`].
pub struct ZipWriter<W: Write> {
    w: W,
    offset: u64,
    central: Vec<u8>,
    count: usize,
}

impl<W: Write> ZipWriter<W> {
    pub fn new(w: W) -> Self {
        ZipWriter {
            w,
            offset: 0,
            central: Vec::new(),
            count: 0,
        }
    }

    pub fn add(&mut self, e: &Entry) -> Result<()> {
        let name = e.name.as_bytes();
        let csize = e.data.len() as u64;
        if self.offset + csize + 30 + name.len() as u64 > u32::MAX as u64
            || e.size > u32::MAX as u64
        {
            bail!(
                "the export would be larger than 4 GiB, which needs ZIP64 (not written); export fewer cells"
            );
        }
        if self.count == u16::MAX as usize {
            bail!("more than 65,535 files in one export");
        }
        let mut h = Vec::with_capacity(30 + name.len());
        h.extend_from_slice(&0x0403_4b50u32.to_le_bytes());
        h.extend_from_slice(&20u16.to_le_bytes()); // version needed
        h.extend_from_slice(&UTF8_NAMES.to_le_bytes());
        h.extend_from_slice(&e.method.to_le_bytes());
        h.extend_from_slice(&0u16.to_le_bytes()); // time
        h.extend_from_slice(&DOS_DATE_1980_01_01.to_le_bytes());
        h.extend_from_slice(&e.crc.to_le_bytes());
        h.extend_from_slice(&(csize as u32).to_le_bytes());
        h.extend_from_slice(&(e.size as u32).to_le_bytes());
        h.extend_from_slice(&(name.len() as u16).to_le_bytes());
        h.extend_from_slice(&0u16.to_le_bytes()); // extra
        h.extend_from_slice(name);
        self.w.write_all(&h)?;
        self.w.write_all(&e.data)?;

        let c = &mut self.central;
        c.extend_from_slice(&0x0201_4b50u32.to_le_bytes());
        c.extend_from_slice(&20u16.to_le_bytes()); // version made by
        c.extend_from_slice(&20u16.to_le_bytes()); // version needed
        c.extend_from_slice(&UTF8_NAMES.to_le_bytes());
        c.extend_from_slice(&e.method.to_le_bytes());
        c.extend_from_slice(&0u16.to_le_bytes());
        c.extend_from_slice(&DOS_DATE_1980_01_01.to_le_bytes());
        c.extend_from_slice(&e.crc.to_le_bytes());
        c.extend_from_slice(&(csize as u32).to_le_bytes());
        c.extend_from_slice(&(e.size as u32).to_le_bytes());
        c.extend_from_slice(&(name.len() as u16).to_le_bytes());
        c.extend_from_slice(&[0u8; 12]); // extra, comment, disk, internal attr, external attr (4)
        c.extend_from_slice(&(self.offset as u32).to_le_bytes());
        c.extend_from_slice(name);

        self.offset += h.len() as u64 + csize;
        self.count += 1;
        Ok(())
    }

    pub fn finish(mut self) -> Result<W> {
        if self.offset + self.central.len() as u64 > u32::MAX as u64 {
            bail!("the export would be larger than 4 GiB, which needs ZIP64 (not written)");
        }
        self.w.write_all(&self.central)?;
        let mut end = Vec::with_capacity(22);
        end.extend_from_slice(&0x0605_4b50u32.to_le_bytes());
        end.extend_from_slice(&[0u8; 4]); // this disk, disk with the directory
        end.extend_from_slice(&(self.count as u16).to_le_bytes());
        end.extend_from_slice(&(self.count as u16).to_le_bytes());
        end.extend_from_slice(&(self.central.len() as u32).to_le_bytes());
        end.extend_from_slice(&(self.offset as u32).to_le_bytes());
        end.extend_from_slice(&0u16.to_le_bytes()); // comment
        self.w.write_all(&end)?;
        self.w.flush()?;
        Ok(self.w)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read;

    #[test]
    fn a_standard_reader_reads_it_back() {
        let a: Vec<u8> = (0..20_000u32).flat_map(|i| (i % 7).to_le_bytes()).collect();
        let b = b"tiny".to_vec(); // deflate makes it larger: stored
        let mut z = ZipWriter::new(Vec::new());
        z.add(&Entry::deflate("a.fcs".into(), &a).unwrap()).unwrap();
        z.add(&Entry::deflate("dir-free name é.fcs".into(), &b).unwrap())
            .unwrap();
        let bytes = z.finish().unwrap();
        let mut ar = zip::ZipArchive::new(std::io::Cursor::new(bytes.clone())).unwrap();
        assert_eq!(ar.len(), 2);
        let mut got = Vec::new();
        ar.by_name("a.fcs").unwrap().read_to_end(&mut got).unwrap();
        assert_eq!(got, a);
        let mut f = ar.by_index(1).unwrap();
        assert_eq!(f.name(), "dir-free name é.fcs");
        assert_eq!(f.compression(), zip::CompressionMethod::Stored);
        let mut got = Vec::new();
        f.read_to_end(&mut got).unwrap();
        assert_eq!(got, b);
        // Deterministic: the same entries give the same archive.
        let mut z = ZipWriter::new(Vec::new());
        z.add(&Entry::deflate("a.fcs".into(), &a).unwrap()).unwrap();
        z.add(&Entry::deflate("dir-free name é.fcs".into(), &b).unwrap())
            .unwrap();
        assert_eq!(z.finish().unwrap(), bytes);
    }
}
