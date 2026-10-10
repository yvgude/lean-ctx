// SPDX-License-Identifier: Apache-2.0
//! Which shared libraries a binary links against, read from its headers.
//!
//! The ORT CUDA provider's own import table says which CUDA major it was
//! built for (`cudart64_12.dll` vs `cudart64_13.dll`, `libcudart.so.12` vs
//! `.so.13`). Since onnxruntime-gpu 1.27 the PyPI wheels use CUDA 13 while
//! other builds stay on CUDA 12 (#2049), so the version cannot be guessed
//! from the ORT release. Only the headers and import names are read — a few
//! KiB of a file that is hundreds of MiB large — and every offset is bounded.

use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

/// Upper bounds against malformed or hostile files.
const MAX_SECTIONS: usize = 128;
const MAX_ENTRIES: usize = 4096;
const MAX_NAME: usize = 256;

/// Libraries imported by the PE (`.dll`/`.exe`) or ELF64 (`.so`) at `path`,
/// including PE delay-load imports, in table order.
pub(crate) fn imported_libraries(path: &Path) -> Result<Vec<String>, String> {
    let mut file = File::open(path).map_err(|e| format!("cannot open {}: {e}", path.display()))?;
    let mut magic = [0u8; 4];
    file.read_exact(&mut magic)
        .map_err(|e| format!("cannot read {}: {e}", path.display()))?;
    let mut reader = Reader { file: &mut file };
    let result = match magic {
        [b'M', b'Z', ..] => pe_imports(&mut reader),
        [0x7f, b'E', b'L', b'F'] => elf_needed(&mut reader),
        _ => Err("not a PE or ELF binary".to_string()),
    };
    result.map_err(|e| format!("{}: {e}", path.display()))
}

/// The CUDA major a binary was built against, from its `cudart` (or, if it
/// does not import cudart directly, `cublas`) dependency.
pub(crate) fn cuda_major_from_imports(imports: &[String]) -> Option<u32> {
    let major = |name: &str, prefix: &str, suffix: &str| -> Option<u32> {
        let rest = name.strip_prefix(prefix)?;
        let digits = if suffix.is_empty() {
            rest
        } else {
            rest.strip_suffix(suffix)?
        };
        // `cudart64_12`, `cudart64_110` (CUDA 11.0 naming) → major only.
        let digits: String = digits.chars().take_while(char::is_ascii_digit).collect();
        match digits.len() {
            1 | 2 => digits.parse().ok(),
            3 => digits[..2].parse().ok(),
            _ => None,
        }
    };
    for lib in ["cudart", "cublas"] {
        for name in imports {
            let lower = name.to_ascii_lowercase();
            let found = major(&lower, &format!("{lib}64_"), ".dll")
                .or_else(|| major(&lower, &format!("lib{lib}.so."), ""));
            if found.is_some() {
                return found;
            }
        }
    }
    None
}

struct Reader<'a> {
    file: &'a mut File,
}

impl Reader<'_> {
    fn bytes(&mut self, offset: u64, len: usize) -> Result<Vec<u8>, String> {
        self.file
            .seek(SeekFrom::Start(offset))
            .map_err(|e| format!("seek to {offset}: {e}"))?;
        let mut buf = vec![0u8; len];
        self.file
            .read_exact(&mut buf)
            .map_err(|e| format!("truncated at {offset}: {e}"))?;
        Ok(buf)
    }

    fn u16(&mut self, offset: u64) -> Result<u16, String> {
        let b = self.bytes(offset, 2)?;
        Ok(u16::from_le_bytes([b[0], b[1]]))
    }

    fn u32(&mut self, offset: u64) -> Result<u32, String> {
        let b = self.bytes(offset, 4)?;
        Ok(u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    }

    fn u64(&mut self, offset: u64) -> Result<u64, String> {
        let b = self.bytes(offset, 8)?;
        Ok(u64::from_le_bytes(b.try_into().expect("8 bytes")))
    }

    /// NUL-terminated name of at most [`MAX_NAME`] bytes; tolerates EOF.
    fn c_str(&mut self, offset: u64) -> Result<String, String> {
        self.file
            .seek(SeekFrom::Start(offset))
            .map_err(|e| format!("seek to {offset}: {e}"))?;
        let mut buf = Vec::with_capacity(64);
        (&mut *self.file)
            .take(MAX_NAME as u64)
            .read_to_end(&mut buf)
            .map_err(|e| format!("read name at {offset}: {e}"))?;
        let end = buf
            .iter()
            .position(|&b| b == 0)
            .ok_or_else(|| format!("unterminated name at {offset}"))?;
        String::from_utf8(buf[..end].to_vec()).map_err(|_| format!("non-UTF-8 name at {offset}"))
    }
}

struct Section {
    vaddr: u32,
    span: u32,
    raw_ptr: u32,
}

fn pe_imports(r: &mut Reader) -> Result<Vec<String>, String> {
    let pe = u64::from(r.u32(0x3c)?);
    if r.bytes(pe, 4)? != b"PE\0\0" {
        return Err("missing PE signature".to_string());
    }
    let sections = usize::from(r.u16(pe + 6)?);
    let opt_size = u64::from(r.u16(pe + 20)?);
    let opt = pe + 24;
    let (dirs_at, count_at) = match r.u16(opt)? {
        0x20b => (opt + 112, opt + 108), // PE32+
        0x10b => (opt + 96, opt + 92),   // PE32
        other => return Err(format!("unknown optional header magic {other:#x}")),
    };
    let dir_count = r.u32(count_at)?;
    let table = opt + opt_size;
    let sections = (0..sections.min(MAX_SECTIONS))
        .map(|i| {
            let at = table + 40 * i as u64;
            let vsize = r.u32(at + 8)?;
            let raw_size = r.u32(at + 16)?;
            Ok(Section {
                vaddr: r.u32(at + 12)?,
                span: vsize.max(raw_size),
                raw_ptr: r.u32(at + 20)?,
            })
        })
        .collect::<Result<Vec<_>, String>>()?;
    let to_offset = |rva: u32| -> Result<u64, String> {
        sections
            .iter()
            .find(|s| rva >= s.vaddr && rva - s.vaddr < s.span)
            .map(|s| u64::from(s.raw_ptr) + u64::from(rva - s.vaddr))
            .ok_or_else(|| format!("RVA {rva:#x} is outside every section"))
    };

    let mut names = Vec::new();
    // Data directory 1: import table; 20-byte descriptors, name RVA at +12.
    if dir_count > 1 {
        let rva = r.u32(dirs_at + 8)?;
        if rva != 0 {
            let start = to_offset(rva)?;
            for i in 0..MAX_ENTRIES as u64 {
                let desc = r.bytes(start + 20 * i, 20)?;
                if desc.iter().all(|&b| b == 0) {
                    break;
                }
                let name_rva = u32::from_le_bytes([desc[12], desc[13], desc[14], desc[15]]);
                names.push(r.c_str(to_offset(name_rva)?)?);
            }
        }
    }
    // Data directory 13: delay-load imports; 32-byte descriptors with the
    // attributes at +0 (bit 0: RVA-based) and the name RVA at +4.
    if dir_count > 13 {
        let rva = r.u32(dirs_at + 13 * 8)?;
        if rva != 0 {
            let start = to_offset(rva)?;
            for i in 0..MAX_ENTRIES as u64 {
                let at = start + 32 * i;
                let attributes = r.u32(at)?;
                let name_rva = r.u32(at + 4)?;
                if name_rva == 0 {
                    break;
                }
                // Pre-VC7 VA-based descriptors are not produced by modern
                // toolchains; skip rather than guess the image base.
                if attributes & 1 == 1 {
                    names.push(r.c_str(to_offset(name_rva)?)?);
                }
            }
        }
    }
    Ok(names)
}

fn elf_needed(r: &mut Reader) -> Result<Vec<String>, String> {
    let ident = r.bytes(0, 6)?;
    if ident[4] != 2 || ident[5] != 1 {
        return Err("only little-endian ELF64 is supported".to_string());
    }
    let phoff = r.u64(0x20)?;
    let phentsize = u64::from(r.u16(0x36)?);
    let phnum = usize::from(r.u16(0x38)?);
    if phentsize < 56 {
        return Err("program header entries too small".to_string());
    }
    let mut loads = Vec::new();
    let mut dynamic = None;
    for i in 0..phnum.min(MAX_SECTIONS) {
        let at = phoff + phentsize * i as u64;
        let (offset, vaddr, filesz) = (r.u64(at + 8)?, r.u64(at + 16)?, r.u64(at + 32)?);
        match r.u32(at)? {
            1 => loads.push((vaddr, filesz, offset)),
            2 => dynamic = Some((offset, filesz)),
            _ => {}
        }
    }
    let Some((dyn_offset, dyn_size)) = dynamic else {
        return Ok(Vec::new()); // statically linked
    };
    let mut needed = Vec::new();
    let mut strtab = None;
    for i in 0..(dyn_size / 16).min(MAX_ENTRIES as u64) {
        let at = dyn_offset + 16 * i;
        match r.u64(at)? {
            0 => break,
            1 => needed.push(r.u64(at + 8)?),
            5 => strtab = Some(r.u64(at + 8)?),
            _ => {}
        }
    }
    let strtab = strtab.ok_or("dynamic section without DT_STRTAB")?;
    let strtab = loads
        .iter()
        .find(|(vaddr, filesz, _)| strtab >= *vaddr && strtab - vaddr < *filesz)
        .map(|(vaddr, _, offset)| offset + (strtab - vaddr))
        .ok_or("DT_STRTAB outside every PT_LOAD segment")?;
    needed
        .into_iter()
        .map(|name| r.c_str(strtab + name))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn put(buf: &mut Vec<u8>, at: usize, bytes: &[u8]) {
        if buf.len() < at + bytes.len() {
            buf.resize(at + bytes.len(), 0);
        }
        buf[at..at + bytes.len()].copy_from_slice(bytes);
    }

    /// Minimal PE32+ with one `.idata` section (RVA 0x1000 at file 0x400)
    /// holding a normal import table and a delay-load table.
    fn pe_fixture(imports: &[&str], delay: &[&str]) -> Vec<u8> {
        let mut b = vec![0u8; 0x400];
        put(&mut b, 0, b"MZ");
        put(&mut b, 0x3c, &0x80u32.to_le_bytes());
        put(&mut b, 0x80, b"PE\0\0");
        put(&mut b, 0x80 + 6, &1u16.to_le_bytes()); // one section
        let opt_size: u16 = 240;
        put(&mut b, 0x80 + 20, &opt_size.to_le_bytes());
        let opt = 0x80 + 24;
        put(&mut b, opt, &0x20bu16.to_le_bytes());
        put(&mut b, opt + 108, &16u32.to_le_bytes());
        let section = opt + usize::from(opt_size);
        put(&mut b, section, b".idata\0\0");
        put(&mut b, section + 8, &0x1000u32.to_le_bytes()); // vsize
        put(&mut b, section + 12, &0x1000u32.to_le_bytes()); // vaddr
        put(&mut b, section + 16, &0x1000u32.to_le_bytes()); // raw size
        put(&mut b, section + 20, &0x400u32.to_le_bytes()); // raw ptr

        let rva = |off: usize| (0x1000 + off) as u32;
        let base = 0x400;
        let (import_at, delay_at, names_at) = (0usize, 0x200usize, 0x400usize);
        let mut name_off = names_at;
        for (i, name) in imports.iter().enumerate() {
            put(
                &mut b,
                base + import_at + 20 * i + 12,
                &rva(name_off).to_le_bytes(),
            );
            put(&mut b, base + name_off, format!("{name}\0").as_bytes());
            name_off += name.len() + 1;
        }
        for (i, name) in delay.iter().enumerate() {
            put(&mut b, base + delay_at + 32 * i, &1u32.to_le_bytes());
            put(
                &mut b,
                base + delay_at + 32 * i + 4,
                &rva(name_off).to_le_bytes(),
            );
            put(&mut b, base + name_off, format!("{name}\0").as_bytes());
            name_off += name.len() + 1;
        }
        b.resize(base + 0x1000, 0);
        put(&mut b, opt + 112 + 8, &rva(import_at).to_le_bytes());
        put(&mut b, opt + 112 + 13 * 8, &rva(delay_at).to_le_bytes());
        b
    }

    /// Minimal ELF64 with one PT_LOAD and a PT_DYNAMIC listing DT_NEEDED.
    fn elf_fixture(needed: &[&str]) -> Vec<u8> {
        let mut b = vec![0u8; 0x400];
        put(&mut b, 0, &[0x7f, b'E', b'L', b'F', 2, 1, 1]);
        put(&mut b, 0x20, &0x40u64.to_le_bytes()); // e_phoff
        put(&mut b, 0x36, &56u16.to_le_bytes());
        put(&mut b, 0x38, &2u16.to_le_bytes());
        // PT_LOAD: file 0 ↔ vaddr 0x10000, 0x400 bytes.
        put(&mut b, 0x40, &1u32.to_le_bytes());
        put(&mut b, 0x40 + 16, &0x10000u64.to_le_bytes());
        put(&mut b, 0x40 + 32, &0x400u64.to_le_bytes());
        // PT_DYNAMIC at file 0x100.
        let ph = 0x40 + 56;
        put(&mut b, ph, &2u32.to_le_bytes());
        put(&mut b, ph + 8, &0x100u64.to_le_bytes());
        put(&mut b, ph + 32, &0x100u64.to_le_bytes());
        let strtab = 0x300usize;
        let mut off = 1usize;
        let mut entry = 0x100usize;
        for name in needed {
            put(&mut b, entry, &1u64.to_le_bytes());
            put(&mut b, entry + 8, &(off as u64).to_le_bytes());
            put(&mut b, strtab + off, format!("{name}\0").as_bytes());
            off += name.len() + 1;
            entry += 16;
        }
        put(&mut b, entry, &5u64.to_le_bytes());
        put(
            &mut b,
            entry + 8,
            &(0x10000u64 + strtab as u64).to_le_bytes(),
        );
        b
    }

    fn write(bytes: &[u8]) -> tempfile::NamedTempFile {
        let mut file = tempfile::NamedTempFile::new().unwrap();
        std::io::Write::write_all(&mut file, bytes).unwrap();
        file
    }

    #[test]
    fn reads_pe_imports_and_delay_imports() {
        let file = write(&pe_fixture(
            &["onnxruntime_providers_shared.dll", "cudart64_13.dll"],
            &["cudnn64_9.dll"],
        ));
        assert_eq!(
            imported_libraries(file.path()).unwrap(),
            [
                "onnxruntime_providers_shared.dll",
                "cudart64_13.dll",
                "cudnn64_9.dll"
            ]
        );
    }

    #[test]
    fn reads_elf_needed() {
        let file = write(&elf_fixture(&[
            "libcublasLt.so.12",
            "libcudart.so.12",
            "libc.so.6",
        ]));
        assert_eq!(
            imported_libraries(file.path()).unwrap(),
            ["libcublasLt.so.12", "libcudart.so.12", "libc.so.6"]
        );
    }

    #[test]
    fn rejects_garbage_and_truncation_without_panicking() {
        assert!(imported_libraries(write(b"not a binary").path()).is_err());
        let pe = pe_fixture(&["cudart64_12.dll"], &[]);
        for cut in [2, 0x40, 0x84, 0x100, 0x200, 0x401] {
            let _ = imported_libraries(write(&pe[..cut]).path());
        }
        let elf = elf_fixture(&["libcudart.so.12"]);
        for cut in [4, 0x30, 0x80, 0x110, 0x301] {
            let _ = imported_libraries(write(&elf[..cut]).path());
        }
    }

    #[test]
    fn cuda_major_comes_from_cudart_then_cublas() {
        let names = |v: &[&str]| v.iter().map(ToString::to_string).collect::<Vec<_>>();
        assert_eq!(
            cuda_major_from_imports(&names(&["KERNEL32.dll", "CUDART64_13.DLL"])),
            Some(13)
        );
        assert_eq!(
            cuda_major_from_imports(&names(&["cublasLt64_12.dll", "cublas64_12.dll"])),
            Some(12)
        );
        assert_eq!(
            cuda_major_from_imports(&names(&["libcudart.so.12"])),
            Some(12)
        );
        assert_eq!(
            cuda_major_from_imports(&names(&["cudart64_110.dll"])),
            Some(11)
        );
        assert_eq!(cuda_major_from_imports(&names(&["libc.so.6"])), None);
    }
}
