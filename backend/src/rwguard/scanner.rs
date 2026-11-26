//! Native Rust port of RWGuard (v1.1 by iroxacu666): structural validation
//! and repair for GTA San Andreas RenderWare `.txd` (TexDict) and `.dff`
//! (Clump) model files.
//!
//! Detects malformed chunk headers that trigger stack/heap overflows in the
//! RenderWare engine:
//! - `.txd`: `TexDict STRUCT overflow` and `Mipmap heap overflow`
//! - `.dff`: `Atomic STRUCT overflow`, `Light STRUCT overflow`, and
//!   `Camera STRUCT overflow`
//!
//! Scanning streams directly through a `BufReader` and seeks past pixel/mesh
//! payloads without loading whole models into memory.

use std::fs::{self, File};
use std::io::{self, BufReader, Read, Seek, SeekFrom, Write};
use std::path::Path;

/// RenderWare binary stream chunk identifiers and struct size limits.
pub mod rw {
    pub const STRUCT: u32 = 0x01;
    pub const CAMERA: u32 = 0x05;
    pub const CLUMP: u32 = 0x10;
    pub const LIGHT: u32 = 0x12;
    pub const ATOMIC: u32 = 0x14;
    pub const TEXNATIVE: u32 = 0x15;
    pub const TEXDICT: u32 = 0x16;

    pub const ATOMIC_STRUCT_MAX: u32 = 16;
    pub const LIGHT_STRUCT_MAX: u32 = 24;
    pub const CAMERA_STRUCT_MAX: u32 = 32;
    pub const TEXDICT_STRUCT_OK: u32 = 4;

    pub const CHUNK_HEADER_SIZE: usize = 12;
    pub const RASTER_HDR_SIZE: usize = 88;
    /// FourCC `"DXT1"` in little-endian.
    pub const D3DFMT_DXT1: u32 = 0x3154_5844;
}

/// 12-byte RenderWare chunk header.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ChunkHeader {
    chunk_type: u32,
    length: u32,
    _version: u32,
}

/// Packed 88-byte RenderWare D3D9 native texture header (`RasterHdr`).
#[derive(Debug, Clone, Copy)]
struct RasterHdr {
    raster_format: u32,
    d3d_format: u32,
    width: u16,
    height: u16,
    depth: u8,
    num_mips: u8,
    compression: u8,
}

/// Result of scanning a single model file with RWGuard.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ScanVerdict {
    /// Clean (or non-RenderWare container).
    Safe,
    /// RenderWare structural overflow detected, with human-readable threat name.
    Threat(String),
    /// Could not open or read the leading header (e.g. file shorter than 12 B).
    Error,
}

fn read_u32_le<R: Read>(r: &mut R) -> Option<u32> {
    let mut b = [0u8; 4];
    r.read_exact(&mut b).ok()?;
    Some(u32::from_le_bytes(b))
}

fn read_chunk_header<R: Read>(r: &mut R) -> Option<ChunkHeader> {
    let mut b = [0u8; rw::CHUNK_HEADER_SIZE];
    r.read_exact(&mut b).ok()?;
    Some(ChunkHeader {
        chunk_type: u32::from_le_bytes([b[0], b[1], b[2], b[3]]),
        length: u32::from_le_bytes([b[4], b[5], b[6], b[7]]),
        _version: u32::from_le_bytes([b[8], b[9], b[10], b[11]]),
    })
}

fn parse_raster_hdr(b: &[u8; rw::RASTER_HDR_SIZE]) -> RasterHdr {
    // Offsets match the C++ `#pragma pack(push, 1)` RasterHdr struct:
    //  0..4   platformId
    //  4..8   filterFlags
    //  8..40  texName[32]
    // 40..72  maskName[32]
    // 72..76  rasterFormat
    // 76..80  d3dFormat
    // 80..82  width
    // 82..84  height
    // 84      depth
    // 85      numMips
    // 86      rasterType
    // 87      compression
    RasterHdr {
        raster_format: u32::from_le_bytes([b[72], b[73], b[74], b[75]]),
        d3d_format: u32::from_le_bytes([b[76], b[77], b[78], b[79]]),
        width: u16::from_le_bytes([b[80], b[81]]),
        height: u16::from_le_bytes([b[82], b[83]]),
        depth: b[84],
        num_mips: b[85],
        compression: b[87],
    }
}

fn read_raster_hdr<R: Read>(r: &mut R) -> Option<RasterHdr> {
    let mut b = [0u8; rw::RASTER_HDR_SIZE];
    r.read_exact(&mut b).ok()?;
    Some(parse_raster_hdr(&b))
}

fn palette_bytes(raster_format: u32) -> u32 {
    if (raster_format & 0x4000) != 0 {
        128
    } else if (raster_format & 0x2000) != 0 {
        1024
    } else {
        0
    }
}

fn actual_mip_bytes(d3d_fmt: u32, compression: u8, depth: u8, mw: u32, mh: u32) -> u64 {
    if (compression & 0x08) != 0 {
        let bw = u64::from(mw).div_ceil(4).max(1);
        let bh = u64::from(mh).div_ceil(4).max(1);
        let bpb = if d3d_fmt == rw::D3DFMT_DXT1 { 8 } else { 16 };
        bw.saturating_mul(bh).saturating_mul(bpb)
    } else {
        let bpp = if depth >= 8 { u64::from(depth / 8) } else { 1 };
        u64::from(mw.max(1))
            .saturating_mul(u64::from(mh.max(1)))
            .saturating_mul(bpp)
    }
}

/// Scans a RenderWare `.txd` (TexDict) stream.
pub fn scan_txd<R: Read + Seek>(r: &mut R) -> ScanVerdict {
    let Some(outer) = read_chunk_header(r) else {
        return ScanVerdict::Error;
    };
    if outer.chunk_type != rw::TEXDICT {
        return ScanVerdict::Safe;
    }
    let Some(txd_struct) = read_chunk_header(r) else {
        return ScanVerdict::Error;
    };
    if txd_struct.chunk_type != rw::STRUCT {
        return ScanVerdict::Safe;
    }
    if txd_struct.length != rw::TEXDICT_STRUCT_OK {
        return ScanVerdict::Threat("TexDict STRUCT overflow".into());
    }

    let Some(num_tex) = read_u32_le(r) else {
        return ScanVerdict::Safe;
    };
    let txd_end = (rw::CHUNK_HEADER_SIZE as u64).saturating_add(u64::from(outer.length));

    for _ in 0..num_tex {
        let Ok(pos) = r.stream_position() else {
            break;
        };
        if pos.saturating_add(rw::CHUNK_HEADER_SIZE as u64) > txd_end {
            break;
        }
        let Some(tn) = read_chunk_header(r) else {
            break;
        };
        let Ok(after_tn) = r.stream_position() else {
            break;
        };
        let tn_end = after_tn.saturating_add(u64::from(tn.length));

        if tn.chunk_type != rw::TEXNATIVE {
            if r.seek(SeekFrom::Start(tn_end)).is_err() {
                break;
            }
            continue;
        }

        let Some(ts) = read_chunk_header(r) else {
            break;
        };
        if ts.chunk_type != rw::STRUCT || (ts.length as usize) < rw::RASTER_HDR_SIZE {
            if r.seek(SeekFrom::Start(tn_end)).is_err() {
                break;
            }
            continue;
        }

        let Some(hdr) = read_raster_hdr(r) else {
            break;
        };

        let pal = i64::from(palette_bytes(hdr.raster_format));
        if pal > 0 && r.seek(SeekFrom::Current(pal)).is_err() {
            if r.seek(SeekFrom::Start(tn_end)).is_err() {
                break;
            }
            continue;
        }

        let mut mw = if hdr.width != 0 {
            u32::from(hdr.width)
        } else {
            1
        };
        let mut mh = if hdr.height != 0 {
            u32::from(hdr.height)
        } else {
            1
        };

        for _ in 0..hdr.num_mips {
            let Some(data_size) = read_u32_le(r) else {
                break;
            };
            let exp_max = actual_mip_bytes(hdr.d3d_format, hdr.compression, hdr.depth, mw, mh);
            if u64::from(data_size) > exp_max.saturating_mul(2) && data_size > 4096 {
                return ScanVerdict::Threat("Mipmap heap overflow".into());
            }
            if r.seek(SeekFrom::Current(i64::from(data_size))).is_err() {
                break;
            }
            if mw > 1 {
                mw >>= 1;
            }
            if mh > 1 {
                mh >>= 1;
            }
        }

        if r.seek(SeekFrom::Start(tn_end)).is_err() {
            break;
        }
    }

    ScanVerdict::Safe
}

/// Scans a RenderWare `.dff` (Clump) stream.
pub fn scan_dff<R: Read + Seek>(r: &mut R) -> ScanVerdict {
    let Some(clump) = read_chunk_header(r) else {
        return ScanVerdict::Error;
    };
    if clump.chunk_type != rw::CLUMP {
        return ScanVerdict::Safe;
    }

    let clump_end = (rw::CHUNK_HEADER_SIZE as u64).saturating_add(u64::from(clump.length));

    while let Ok(pos) = r.stream_position() {
        if pos.saturating_add(rw::CHUNK_HEADER_SIZE as u64) > clump_end {
            break;
        }
        let Some(chunk) = read_chunk_header(r) else {
            break;
        };
        let Ok(after_chunk) = r.stream_position() else {
            break;
        };
        let parent_end = after_chunk.saturating_add(u64::from(chunk.length));

        if matches!(chunk.chunk_type, rw::ATOMIC | rw::LIGHT | rw::CAMERA) {
            if let Some(inner) = read_chunk_header(r) {
                if inner.chunk_type == rw::STRUCT {
                    let (max_size, name) = match chunk.chunk_type {
                        rw::ATOMIC => (rw::ATOMIC_STRUCT_MAX, "Atomic"),
                        rw::LIGHT => (rw::LIGHT_STRUCT_MAX, "Light"),
                        rw::CAMERA => (rw::CAMERA_STRUCT_MAX, "Camera"),
                        _ => (0, ""),
                    };
                    if max_size > 0 && inner.length > max_size {
                        return ScanVerdict::Threat(format!("{name} STRUCT overflow"));
                    }
                }
            }
            if r.seek(SeekFrom::Start(parent_end)).is_err() {
                break;
            }
            continue;
        }

        if r.seek(SeekFrom::Start(parent_end)).is_err() {
            break;
        }
    }

    ScanVerdict::Safe
}

/// Returns whether a filename has a RenderWare model extension (`.dff` or `.txd`).
pub fn is_model_name(name: &str) -> bool {
    model_ext(name).is_some()
}

fn model_ext(name: &str) -> Option<&'static str> {
    let (_, ext) = name.rsplit_once('.')?;
    if ext.eq_ignore_ascii_case("txd") {
        Some("txd")
    } else if ext.eq_ignore_ascii_case("dff") {
        Some("dff")
    } else {
        None
    }
}

/// Scans `path`, determining `.txd` vs `.dff` from `logical_name` (needed when
/// validating temporary `*.sarp-part` files before renaming into place).
pub fn scan_file_as(path: &Path, logical_name: &str) -> ScanVerdict {
    let Some(ext) = model_ext(logical_name) else {
        return ScanVerdict::Safe;
    };
    let Ok(file) = File::open(path) else {
        return ScanVerdict::Error;
    };
    let mut reader = BufReader::with_capacity(16 * 1024, file);
    match ext {
        "txd" => scan_txd(&mut reader),
        "dff" => scan_dff(&mut reader),
        _ => ScanVerdict::Safe,
    }
}

/// Scans a model file on disk using its own filename extension.
pub fn scan_file(path: &Path) -> ScanVerdict {
    let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
        return ScanVerdict::Safe;
    };
    scan_file_as(path, name)
}

fn read_u32(data: &[u8], off: usize) -> Option<u32> {
    let slice = data.get(off..off.checked_add(4)?)?;
    Some(u32::from_le_bytes([slice[0], slice[1], slice[2], slice[3]]))
}

fn write_u32(data: &mut [u8], off: usize, value: u32) {
    if let Some(slice) = data.get_mut(off..off + 4) {
        slice.copy_from_slice(&value.to_le_bytes());
    }
}

/// Truncates RenderWare overflow payloads in-place (port of RWGuard `HealFile`).
/// Returns `true` if `data` was modified.
pub fn heal_bytes(data: &mut Vec<u8>) -> bool {
    if data.len() < 24 {
        return false;
    }

    let mut changed = false;
    let Some(outer_type) = read_u32(data, 0) else {
        return false;
    };

    if outer_type == rw::TEXDICT {
        if read_u32(data, 12) == Some(rw::STRUCT) {
            if let Some(bad_len) = read_u32(data, 16) {
                if bad_len > rw::TEXDICT_STRUCT_OK {
                    let erase_start = 24usize + rw::TEXDICT_STRUCT_OK as usize;
                    let erase_end = (24usize).saturating_add(bad_len as usize).min(data.len());
                    if erase_start < erase_end {
                        let cut = (erase_end - erase_start) as u32;
                        write_u32(data, 16, rw::TEXDICT_STRUCT_OK);
                        if let Some(outer_len) = read_u32(data, 4) {
                            write_u32(data, 4, outer_len.saturating_sub(cut));
                        }
                        data.drain(erase_start..erase_end);
                        changed = true;
                    }
                }
            }
        }

        if data.len() >= 28
            && read_u32(data, 12) == Some(rw::STRUCT)
            && read_u32(data, 16) == Some(rw::TEXDICT_STRUCT_OK)
        {
            let outer_len = read_u32(data, 4).unwrap_or(0) as usize;
            let txd_end = 12usize.saturating_add(outer_len).min(data.len());

            struct MipFix {
                mip_size_off: usize,
                orig_size: u32,
                cap_size: u32,
                struct_size_off: usize,
                tn_size_off: usize,
            }

            let mut fixes = Vec::new();
            let mut pos = 28usize;

            while pos.saturating_add(12) <= data.len() && pos < txd_end {
                let tn_type = read_u32(data, pos).unwrap_or(0);
                let tn_size = read_u32(data, pos + 4).unwrap_or(0);
                if tn_size == 0 {
                    break;
                }
                let tn_end = pos.saturating_add(12).saturating_add(tn_size as usize);

                if tn_type != rw::TEXNATIVE {
                    pos = tn_end;
                    continue;
                }

                let hdr_ok = pos.saturating_add(24 + rw::RASTER_HDR_SIZE) <= data.len()
                    && read_u32(data, pos + 12) == Some(rw::STRUCT)
                    && read_u32(data, pos + 16).is_some_and(|l| l as usize >= rw::RASTER_HDR_SIZE);
                if !hdr_ok {
                    pos = tn_end;
                    continue;
                }

                let tn_size_off = pos + 4;
                let struct_size_off = pos + 16;
                let hdr_off = pos + 24;
                let mut hdr_buf = [0u8; rw::RASTER_HDR_SIZE];
                hdr_buf.copy_from_slice(&data[hdr_off..hdr_off + rw::RASTER_HDR_SIZE]);
                let hdr = parse_raster_hdr(&hdr_buf);

                let pal_size = palette_bytes(hdr.raster_format) as usize;
                let mut mip_off = hdr_off
                    .saturating_add(rw::RASTER_HDR_SIZE)
                    .saturating_add(pal_size);
                let mut mw = if hdr.width != 0 {
                    u32::from(hdr.width)
                } else {
                    1
                };
                let mut mh = if hdr.height != 0 {
                    u32::from(hdr.height)
                } else {
                    1
                };

                for _ in 0..hdr.num_mips {
                    if mip_off.saturating_add(4) > data.len() {
                        break;
                    }
                    let Some(ds) = read_u32(data, mip_off) else {
                        break;
                    };
                    let exp_max =
                        actual_mip_bytes(hdr.d3d_format, hdr.compression, hdr.depth, mw, mh);
                    let cap_size = u32::try_from(exp_max).unwrap_or(u32::MAX);
                    if u64::from(ds) > exp_max.saturating_mul(2) && ds > 4096 {
                        fixes.push(MipFix {
                            mip_size_off: mip_off,
                            orig_size: ds,
                            cap_size,
                            struct_size_off,
                            tn_size_off,
                        });
                    }
                    mip_off = mip_off.saturating_add(4).saturating_add(ds as usize);
                    if mw > 1 {
                        mw >>= 1;
                    }
                    if mh > 1 {
                        mh >>= 1;
                    }
                }

                pos = tn_end;
            }

            let mut total_cut = 0u32;
            for fx in fixes.into_iter().rev() {
                let erase_start = fx
                    .mip_size_off
                    .saturating_add(4)
                    .saturating_add(fx.cap_size as usize)
                    .min(data.len());
                let erase_end = fx
                    .mip_size_off
                    .saturating_add(4)
                    .saturating_add(fx.orig_size as usize)
                    .min(data.len());
                if erase_start < erase_end {
                    data.drain(erase_start..erase_end);
                }
                let cut = fx.orig_size.saturating_sub(fx.cap_size);
                write_u32(data, fx.mip_size_off, fx.cap_size);
                if let Some(s_len) = read_u32(data, fx.struct_size_off) {
                    write_u32(data, fx.struct_size_off, s_len.saturating_sub(cut));
                }
                if let Some(tn_len) = read_u32(data, fx.tn_size_off) {
                    write_u32(data, fx.tn_size_off, tn_len.saturating_sub(cut));
                }
                total_cut = total_cut.saturating_add(cut);
                changed = true;
            }
            if total_cut > 0 {
                if let Some(outer_len) = read_u32(data, 4) {
                    write_u32(data, 4, outer_len.saturating_sub(total_cut));
                }
            }
        }
    }

    if outer_type == rw::CLUMP {
        let mut pos = 12usize;
        while pos.saturating_add(12) <= data.len() {
            let clump_size = read_u32(data, 4).unwrap_or(0) as usize;
            if pos.saturating_add(12) > 12usize.saturating_add(clump_size) {
                break;
            }
            let c_type = read_u32(data, pos).unwrap_or(0);
            let c_size = read_u32(data, pos + 4).unwrap_or(0);

            if matches!(c_type, rw::ATOMIC | rw::LIGHT | rw::CAMERA) {
                let inner_off = pos + 12;
                if inner_off.saturating_add(12) <= data.len()
                    && read_u32(data, inner_off) == Some(rw::STRUCT)
                {
                    let s_len = read_u32(data, inner_off + 4).unwrap_or(0);
                    let max_len = match c_type {
                        rw::ATOMIC => rw::ATOMIC_STRUCT_MAX,
                        rw::LIGHT => rw::LIGHT_STRUCT_MAX,
                        rw::CAMERA => rw::CAMERA_STRUCT_MAX,
                        _ => 0,
                    };
                    if max_len > 0 && s_len > max_len {
                        let cut = s_len - max_len;
                        let erase_start = inner_off
                            .saturating_add(12)
                            .saturating_add(max_len as usize);
                        let erase_end = inner_off.saturating_add(12).saturating_add(s_len as usize);
                        if erase_end <= data.len() && erase_start < erase_end {
                            write_u32(data, inner_off + 4, max_len);
                            data.drain(erase_start..erase_end);
                            write_u32(data, pos + 4, c_size.saturating_sub(cut));
                            if let Some(clump_len) = read_u32(data, 4) {
                                write_u32(data, 4, clump_len.saturating_sub(cut));
                            }
                            changed = true;
                        }
                    }
                }
            }

            let updated_size = read_u32(data, pos + 4).unwrap_or(0) as usize;
            let next_pos = pos.saturating_add(12).saturating_add(updated_size);
            if next_pos <= pos {
                break;
            }
            pos = next_pos;
        }
    }

    changed
}

/// Repairs a model file atomically (write to temp + rename). Returns `true` if
/// the file was healed and replaced on disk.
pub fn heal_file(path: &Path) -> io::Result<bool> {
    let mut data = fs::read(path)?;
    if !heal_bytes(&mut data) {
        return Ok(false);
    }
    let tmp = path.with_extension(format!(
        "{}.rwg-heal",
        path.extension().and_then(|e| e.to_str()).unwrap_or("tmp")
    ));
    let write_res = (|| {
        let mut out = File::create(&tmp)?;
        out.write_all(&data)?;
        out.sync_all()?;
        Ok::<_, io::Error>(())
    })();
    if let Err(err) = write_res {
        let _ = fs::remove_file(&tmp);
        return Err(err);
    }
    if let Err(err) = crate::install::cache::with_writable(path, || fs::rename(&tmp, path)) {
        let _ = fs::remove_file(&tmp);
        return Err(err);
    }
    Ok(true)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::io::Cursor;

    fn push_header(buf: &mut Vec<u8>, chunk_type: u32, length: u32) {
        buf.extend_from_slice(&chunk_type.to_le_bytes());
        buf.extend_from_slice(&length.to_le_bytes());
        buf.extend_from_slice(&0x1803_FFFFu32.to_le_bytes());
    }

    /// Builds a valid minimal `.dff` Clump containing one Atomic/Light/Camera
    /// whose inner STRUCT has `struct_len` bytes.
    pub fn build_dff(child_type: u32, struct_len: u32) -> Vec<u8> {
        let child_len = 12 + struct_len;
        let clump_len = 12 + child_len;
        let mut out = Vec::with_capacity(12 + clump_len as usize);
        push_header(&mut out, rw::CLUMP, clump_len);
        push_header(&mut out, child_type, child_len);
        push_header(&mut out, rw::STRUCT, struct_len);
        out.resize(out.len() + struct_len as usize, 0xAA);
        out
    }

    /// Builds a `.txd` with configurable TexDict STRUCT length and one texture
    /// whose single mipmap declares `mip_declared_size` bytes.
    pub fn build_txd(
        txd_struct_len: u32,
        width: u16,
        height: u16,
        depth: u8,
        compression: u8,
        d3d_fmt: u32,
        mip_declared_size: u32,
    ) -> Vec<u8> {
        let mut raster = [0u8; rw::RASTER_HDR_SIZE];
        raster[76..80].copy_from_slice(&d3d_fmt.to_le_bytes());
        raster[80..82].copy_from_slice(&width.to_le_bytes());
        raster[82..84].copy_from_slice(&height.to_le_bytes());
        raster[84] = depth;
        raster[85] = 1; // numMips
        raster[87] = compression;

        let ts_len = (rw::RASTER_HDR_SIZE + 4 + mip_declared_size as usize) as u32;
        let tn_len = 12 + ts_len;
        let outer_len = 12 + txd_struct_len + 12 + tn_len;

        let mut out = Vec::with_capacity(12 + outer_len as usize);
        push_header(&mut out, rw::TEXDICT, outer_len);
        push_header(&mut out, rw::STRUCT, txd_struct_len);
        out.extend_from_slice(&1u32.to_le_bytes()); // numTex = 1
        if txd_struct_len > 4 {
            out.resize(out.len() + (txd_struct_len - 4) as usize, 0x90);
        }
        push_header(&mut out, rw::TEXNATIVE, tn_len);
        push_header(&mut out, rw::STRUCT, ts_len);
        out.extend_from_slice(&raster);
        out.extend_from_slice(&mip_declared_size.to_le_bytes());
        out.resize(out.len() + mip_declared_size as usize, 0x55);
        out
    }

    #[test]
    fn clean_dff_and_txd_pass() {
        let dff = build_dff(rw::ATOMIC, rw::ATOMIC_STRUCT_MAX);
        assert_eq!(scan_dff(&mut Cursor::new(&dff)), ScanVerdict::Safe);

        // 16x16 DXT1 = 4x4 blocks * 8 B = 128 bytes.
        let txd = build_txd(4, 16, 16, 16, 0x08, rw::D3DFMT_DXT1, 128);
        assert_eq!(scan_txd(&mut Cursor::new(&txd)), ScanVerdict::Safe);
    }

    #[test]
    fn detects_and_heals_dff_struct_overflows() {
        for (kind, max_len, label) in [
            (rw::ATOMIC, rw::ATOMIC_STRUCT_MAX, "Atomic STRUCT overflow"),
            (rw::LIGHT, rw::LIGHT_STRUCT_MAX, "Light STRUCT overflow"),
            (rw::CAMERA, rw::CAMERA_STRUCT_MAX, "Camera STRUCT overflow"),
        ] {
            let mut dff = build_dff(kind, max_len + 64);
            assert_eq!(
                scan_dff(&mut Cursor::new(&dff)),
                ScanVerdict::Threat(label.into())
            );
            assert!(heal_bytes(&mut dff));
            assert_eq!(scan_dff(&mut Cursor::new(&dff)), ScanVerdict::Safe);
        }
    }

    #[test]
    fn detects_and_heals_txd_struct_and_mipmap_overflows() {
        let mut bad_struct = build_txd(64, 16, 16, 32, 0, 0, 16 * 16 * 4);
        assert_eq!(
            scan_txd(&mut Cursor::new(&bad_struct)),
            ScanVerdict::Threat("TexDict STRUCT overflow".into())
        );
        assert!(heal_bytes(&mut bad_struct));
        assert_eq!(scan_txd(&mut Cursor::new(&bad_struct)), ScanVerdict::Safe);

        // 16x16 32bpp = 1024 B expected; declare 8192 B (> 2x and > 4096).
        let mut bad_mip = build_txd(4, 16, 16, 32, 0, 0, 8192);
        assert_eq!(
            scan_txd(&mut Cursor::new(&bad_mip)),
            ScanVerdict::Threat("Mipmap heap overflow".into())
        );
        assert!(heal_bytes(&mut bad_mip));
        assert_eq!(scan_txd(&mut Cursor::new(&bad_mip)), ScanVerdict::Safe);
    }

    #[test]
    fn handles_non_rw_short_and_malformed_inputs_without_panicking() {
        assert_eq!(scan_dff(&mut Cursor::new(b"x")), ScanVerdict::Error);
        assert_eq!(scan_txd(&mut Cursor::new(b"x")), ScanVerdict::Error);
        assert_eq!(scan_dff(&mut Cursor::new(&[0u8; 64])), ScanVerdict::Safe);
        // Huge chunk length: must not overflow or loop.
        let mut crafted = Vec::new();
        push_header(&mut crafted, rw::CLUMP, u32::MAX);
        push_header(&mut crafted, rw::ATOMIC, u32::MAX);
        push_header(&mut crafted, rw::STRUCT, u32::MAX);
        assert_eq!(
            scan_dff(&mut Cursor::new(&crafted)),
            ScanVerdict::Threat("Atomic STRUCT overflow".into())
        );
        let _ = heal_bytes(&mut crafted);
    }

    #[test]
    fn scan_file_as_supports_temp_suffixes_and_heal_file_works() {
        let dir = tempfile::tempdir().unwrap();
        let temp = dir.path().join("0x1A2B.dff.abcd.sarp-part");
        let bad = build_dff(rw::ATOMIC, 128);
        fs::write(&temp, &bad).unwrap();

        assert_eq!(
            scan_file_as(&temp, "0x1A2B.dff"),
            ScanVerdict::Threat("Atomic STRUCT overflow".into())
        );
        assert!(heal_file(&temp).unwrap());
        assert_eq!(scan_file_as(&temp, "0x1A2B.dff"), ScanVerdict::Safe);
    }
}
