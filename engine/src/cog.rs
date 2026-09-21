//! Minimal Classic TIFF / COG reader for OPERA COMP GeoTIFFs: little-endian,
//! tiled, Adobe Deflate, interleaved float32 samples. Also writes that shape
//! for tiny synthetic fixtures. Not a general GeoTIFF library.

#[cfg(test)]
use flate2::Compression;
use flate2::read::ZlibDecoder;
#[cfg(test)]
use flate2::write::ZlibEncoder;
use std::io::Read;
#[cfg(test)]
use std::io::Write;

const TAG_IMAGE_WIDTH: u16 = 256;
const TAG_IMAGE_LENGTH: u16 = 257;
const TAG_BITS_PER_SAMPLE: u16 = 258;
const TAG_COMPRESSION: u16 = 259;
#[cfg(test)]
const TAG_PHOTOMETRIC: u16 = 262;
const TAG_SAMPLES_PER_PIXEL: u16 = 277;
const TAG_PLANAR_CONFIG: u16 = 284;
const TAG_TILE_WIDTH: u16 = 322;
const TAG_TILE_LENGTH: u16 = 323;
const TAG_TILE_OFFSETS: u16 = 324;
const TAG_TILE_BYTE_COUNTS: u16 = 325;
const TAG_SAMPLE_FORMAT: u16 = 339;
const TAG_MODEL_PIXEL_SCALE: u16 = 33550;
const TAG_MODEL_TIEPOINT: u16 = 33922;
const TAG_GEO_DOUBLE_PARAMS: u16 = 34736;
const TAG_GDAL_NODATA: u16 = 42113;

const TYPE_ASCII: u16 = 2;
const TYPE_SHORT: u16 = 3;
const TYPE_LONG: u16 = 4;
const TYPE_DOUBLE: u16 = 12;

const COMPRESSION_DEFLATE: u16 = 8;
const SAMPLE_FORMAT_FLOAT: u16 = 3;

// OPERA's current COMP grid is roughly 3,800 x 4,400 pixels. Keep enough
// headroom for a product revision without allowing untrusted TIFF metadata to
// turn the bounded download into an unbounded raster or PNG allocation.
const MAX_RASTER_DIMENSION: u32 = 8_192;
const MAX_RASTER_PIXELS: u64 = 20_000_000;
const MAX_SAMPLES_PER_PIXEL: u16 = 16;
// Real OPERA tiles are much smaller than this. This is an independent bound
// because a TIFF tile may extend beyond the image edge and samples-per-pixel
// multiplies its decoded size.
const MAX_INFLATED_TILE_BYTES: usize = 16 << 20;

/// One band of measured values plus the GeoTIFF affine (pixel-corner GDAL).
#[derive(Debug, Clone)]
pub struct DecodedRaster {
    pub width: u32,
    pub height: u32,
    pub geotransform: [f64; 6],
    /// Projection doubles when present (OPERA: lat0, lon0, FE, FN, invF, a, …).
    pub geo_double_params: Vec<f64>,
    pub nodata: Option<f32>,
    /// First sample plane, row-major, length `width * height`.
    pub values: Vec<f32>,
}

#[derive(Debug, Clone)]
struct Ifd {
    width: u32,
    height: u32,
    samples: u16,
    bits: Vec<u16>,
    sample_format: Vec<u16>,
    compression: u16,
    planar: u16,
    tile_width: u32,
    tile_length: u32,
    tile_offsets: Vec<u32>,
    tile_byte_counts: Vec<u32>,
    model_pixel_scale: Option<[f64; 3]>,
    model_tiepoint: Option<[f64; 6]>,
    geo_double_params: Vec<f64>,
    nodata: Option<f32>,
}

/// Decode the first IFD of a Classic little-endian tiled Deflate float TIFF.
pub fn decode_float_cog(bytes: &[u8]) -> Result<DecodedRaster, String> {
    if bytes.len() < 8 {
        return Err("TIFF too short".into());
    }
    if &bytes[0..2] != b"II" {
        return Err("only little-endian Classic TIFF is supported".into());
    }
    let magic = u16::from_le_bytes(bytes[2..4].try_into().unwrap());
    if magic != 42 {
        return Err("only Classic TIFF (not BigTIFF) is supported".into());
    }
    let ifd_off = u32::from_le_bytes(bytes[4..8].try_into().unwrap()) as usize;
    let ifd = parse_ifd(bytes, ifd_off)?;
    validate_ifd(&ifd)?;
    let geotransform = geotransform_from(&ifd)?;
    let values = read_first_sample(bytes, &ifd)?;
    Ok(DecodedRaster {
        width: ifd.width,
        height: ifd.height,
        geotransform,
        geo_double_params: ifd.geo_double_params,
        nodata: ifd.nodata,
        values,
    })
}

fn validate_ifd(ifd: &Ifd) -> Result<(), String> {
    if ifd.width == 0 || ifd.height == 0 {
        return Err("raster dimensions must be non-zero".into());
    }
    if ifd.width > MAX_RASTER_DIMENSION || ifd.height > MAX_RASTER_DIMENSION {
        return Err(format!(
            "raster dimensions {}x{} exceed {MAX_RASTER_DIMENSION}",
            ifd.width, ifd.height
        ));
    }
    let pixels = u64::from(ifd.width) * u64::from(ifd.height);
    if pixels > MAX_RASTER_PIXELS {
        return Err(format!(
            "raster has {pixels} pixels, limit is {MAX_RASTER_PIXELS}"
        ));
    }
    if ifd.compression != COMPRESSION_DEFLATE {
        return Err(format!("unsupported compression {}", ifd.compression));
    }
    if ifd.planar != 1 {
        return Err("only contiguous planar config is supported".into());
    }
    if ifd.samples < 1 || ifd.samples > MAX_SAMPLES_PER_PIXEL {
        return Err(format!(
            "samples per pixel {} is outside 1..={MAX_SAMPLES_PER_PIXEL}",
            ifd.samples
        ));
    }
    if ifd.bits.len() != ifd.samples as usize || ifd.bits.iter().any(|&b| b != 32) {
        return Err("only 32-bit samples are supported".into());
    }
    if ifd.sample_format.len() != ifd.samples as usize
        || ifd.sample_format.iter().any(|&f| f != SAMPLE_FORMAT_FLOAT)
    {
        return Err("only IEEE float samples are supported".into());
    }
    if ifd.tile_width == 0
        || ifd.tile_length == 0
        || ifd.tile_width > MAX_RASTER_DIMENSION
        || ifd.tile_length > MAX_RASTER_DIMENSION
    {
        return Err(format!(
            "tile dimensions {}x{} are outside 1..={MAX_RASTER_DIMENSION}",
            ifd.tile_width, ifd.tile_length
        ));
    }
    let tiles_x = ifd.width.div_ceil(ifd.tile_width);
    let tiles_y = ifd.height.div_ceil(ifd.tile_length);
    let n = (tiles_x * tiles_y) as usize;
    if ifd.tile_offsets.len() != n || ifd.tile_byte_counts.len() != n {
        return Err("tile offset count does not match the raster".into());
    }
    Ok(())
}

fn geotransform_from(ifd: &Ifd) -> Result<[f64; 6], String> {
    let scale = ifd
        .model_pixel_scale
        .ok_or_else(|| "missing ModelPixelScale".to_owned())?;
    let tie = ifd
        .model_tiepoint
        .ok_or_else(|| "missing ModelTiepoint".to_owned())?;
    let sx = scale[0];
    let sy = scale[1];
    let x0 = tie[3] - tie[0] * sx;
    let y0 = tie[4] + tie[1] * sy;
    Ok([x0, sx, 0.0, y0, 0.0, -sy])
}

fn read_first_sample(bytes: &[u8], ifd: &Ifd) -> Result<Vec<f32>, String> {
    let samples = ifd.samples as usize;
    let tiles_x = ifd.width.div_ceil(ifd.tile_width);
    let pixel_count = usize::try_from(u64::from(ifd.width) * u64::from(ifd.height))
        .map_err(|_| "raster pixel count does not fit this platform".to_owned())?;
    let mut out = vec![0f32; pixel_count];
    for (index, (&offset, &count)) in ifd
        .tile_offsets
        .iter()
        .zip(ifd.tile_byte_counts.iter())
        .enumerate()
    {
        let tile_col = (index as u32) % tiles_x;
        let tile_row = (index as u32) / tiles_x;
        let start = offset as usize;
        let end = start
            .checked_add(count as usize)
            .ok_or_else(|| "tile range overflow".to_owned())?;
        if end > bytes.len() {
            return Err("tile extends past the file".into());
        }
        let tw = ifd.tile_width as usize;
        let th = ifd.tile_length as usize;
        let expected = tw
            .checked_mul(th)
            .and_then(|n| n.checked_mul(samples))
            .and_then(|n| n.checked_mul(std::mem::size_of::<f32>()))
            .ok_or_else(|| "inflated tile size overflow".to_owned())?;
        if expected > MAX_INFLATED_TILE_BYTES {
            return Err(format!(
                "inflated tile size {expected} exceeds {MAX_INFLATED_TILE_BYTES} bytes"
            ));
        }
        let inflated = inflate_exact(&bytes[start..end], expected)?;
        let origin_x = (tile_col * ifd.tile_width) as usize;
        let origin_y = (tile_row * ifd.tile_length) as usize;
        for row in 0..th {
            let y = origin_y + row;
            if y >= ifd.height as usize {
                break;
            }
            for col in 0..tw {
                let x = origin_x + col;
                if x >= ifd.width as usize {
                    break;
                }
                let pix = (row * tw + col) * samples;
                let v = f32::from_le_bytes(inflated[pix * 4..pix * 4 + 4].try_into().unwrap());
                out[y * ifd.width as usize + x] = v;
            }
        }
    }
    Ok(out)
}

fn inflate_exact(input: &[u8], expected: usize) -> Result<Vec<u8>, String> {
    let mut decoder = ZlibDecoder::new(input);
    let mut out = Vec::with_capacity(expected);
    decoder
        .by_ref()
        .take(expected as u64)
        .read_to_end(&mut out)
        .map_err(|e| format!("deflate: {e}"))?;
    if out.len() != expected {
        return Err(format!(
            "inflated tile is {} bytes, expected {expected}",
            out.len()
        ));
    }
    let mut extra = [0u8; 1];
    if decoder
        .read(&mut extra)
        .map_err(|e| format!("deflate: {e}"))?
        != 0
    {
        return Err(format!(
            "inflated tile exceeds expected size of {expected} bytes"
        ));
    }
    Ok(out)
}

fn parse_ifd(bytes: &[u8], off: usize) -> Result<Ifd, String> {
    if off + 2 > bytes.len() {
        return Err("IFD offset past end".into());
    }
    let n = u16::from_le_bytes(bytes[off..off + 2].try_into().unwrap()) as usize;
    let mut cursor = off + 2;
    let mut width = 0u32;
    let mut height = 0u32;
    let mut samples = 1u16;
    let mut bits = Vec::new();
    let mut sample_format = Vec::new();
    let mut compression = 1u16;
    let mut planar = 1u16;
    let mut tile_width = 0u32;
    let mut tile_length = 0u32;
    let mut tile_offsets = Vec::new();
    let mut tile_byte_counts = Vec::new();
    let mut model_pixel_scale = None;
    let mut model_tiepoint = None;
    let mut geo_double_params = Vec::new();
    let mut nodata = None;
    for _ in 0..n {
        if cursor + 12 > bytes.len() {
            return Err("IFD entry past end".into());
        }
        let tag = u16::from_le_bytes(bytes[cursor..cursor + 2].try_into().unwrap());
        let typ = u16::from_le_bytes(bytes[cursor + 2..cursor + 4].try_into().unwrap());
        let count = u32::from_le_bytes(bytes[cursor + 4..cursor + 8].try_into().unwrap());
        let value = u32::from_le_bytes(bytes[cursor + 8..cursor + 12].try_into().unwrap());
        cursor += 12;
        match tag {
            TAG_IMAGE_WIDTH => width = read_u32(bytes, typ, count, value)?,
            TAG_IMAGE_LENGTH => height = read_u32(bytes, typ, count, value)?,
            TAG_BITS_PER_SAMPLE => bits = read_u16s(bytes, typ, count, value)?,
            TAG_COMPRESSION => compression = read_u16s(bytes, typ, count, value)?[0],
            TAG_SAMPLES_PER_PIXEL => samples = read_u16s(bytes, typ, count, value)?[0],
            TAG_PLANAR_CONFIG => planar = read_u16s(bytes, typ, count, value)?[0],
            TAG_TILE_WIDTH => tile_width = read_u32(bytes, typ, count, value)?,
            TAG_TILE_LENGTH => tile_length = read_u32(bytes, typ, count, value)?,
            TAG_TILE_OFFSETS => tile_offsets = read_u32s(bytes, typ, count, value)?,
            TAG_TILE_BYTE_COUNTS => tile_byte_counts = read_u32s(bytes, typ, count, value)?,
            TAG_SAMPLE_FORMAT => sample_format = read_u16s(bytes, typ, count, value)?,
            TAG_MODEL_PIXEL_SCALE => {
                let v = read_f64s(bytes, typ, count, value)?;
                if v.len() >= 3 {
                    model_pixel_scale = Some([v[0], v[1], v[2]]);
                }
            }
            TAG_MODEL_TIEPOINT => {
                let v = read_f64s(bytes, typ, count, value)?;
                if v.len() >= 6 {
                    model_tiepoint = Some([v[0], v[1], v[2], v[3], v[4], v[5]]);
                }
            }
            TAG_GEO_DOUBLE_PARAMS => geo_double_params = read_f64s(bytes, typ, count, value)?,
            TAG_GDAL_NODATA => {
                let s = read_ascii(bytes, typ, count, value)?;
                nodata = s.trim().parse().ok();
            }
            _ => {}
        }
    }
    if bits.is_empty() {
        bits = vec![32; samples as usize];
    }
    if sample_format.is_empty() {
        sample_format = vec![SAMPLE_FORMAT_FLOAT; samples as usize];
    }
    Ok(Ifd {
        width,
        height,
        samples,
        bits,
        sample_format,
        compression,
        planar,
        tile_width,
        tile_length,
        tile_offsets,
        tile_byte_counts,
        model_pixel_scale,
        model_tiepoint,
        geo_double_params,
        nodata,
    })
}

fn read_u16s(bytes: &[u8], typ: u16, count: u32, value: u32) -> Result<Vec<u16>, String> {
    if typ != TYPE_SHORT {
        return Err("expected SHORT".into());
    }
    let size = 2 * count as usize;
    if size <= 4 {
        let mut out = Vec::with_capacity(count as usize);
        let mut v = value;
        for _ in 0..count {
            out.push((v & 0xffff) as u16);
            v >>= 16;
        }
        return Ok(out);
    }
    let start = value as usize;
    let end = start + size;
    if end > bytes.len() {
        return Err("SHORT array past end".into());
    }
    Ok(bytes[start..end]
        .as_chunks::<2>()
        .0
        .iter()
        .map(|c| u16::from_le_bytes(*c))
        .collect())
}

fn read_u32(bytes: &[u8], typ: u16, count: u32, value: u32) -> Result<u32, String> {
    Ok(read_u32s(bytes, typ, count, value)?[0])
}

fn read_u32s(bytes: &[u8], typ: u16, count: u32, value: u32) -> Result<Vec<u32>, String> {
    match typ {
        TYPE_SHORT => Ok(read_u16s(bytes, typ, count, value)?
            .into_iter()
            .map(u32::from)
            .collect()),
        TYPE_LONG => {
            let size = 4 * count as usize;
            if size <= 4 {
                return Ok(vec![value]);
            }
            let start = value as usize;
            let end = start + size;
            if end > bytes.len() {
                return Err("LONG array past end".into());
            }
            Ok(bytes[start..end]
                .as_chunks::<4>()
                .0
                .iter()
                .map(|c| u32::from_le_bytes(*c))
                .collect())
        }
        _ => Err("expected SHORT or LONG".into()),
    }
}

fn read_f64s(bytes: &[u8], typ: u16, count: u32, value: u32) -> Result<Vec<f64>, String> {
    if typ != TYPE_DOUBLE {
        return Err("expected DOUBLE".into());
    }
    let size = 8 * count as usize;
    let start = value as usize;
    let end = start
        .checked_add(size)
        .ok_or_else(|| "DOUBLE array overflow".to_owned())?;
    if end > bytes.len() {
        return Err("DOUBLE array past end".into());
    }
    Ok(bytes[start..end]
        .as_chunks::<8>()
        .0
        .iter()
        .map(|c| f64::from_le_bytes(*c))
        .collect())
}

fn read_ascii(bytes: &[u8], typ: u16, count: u32, value: u32) -> Result<String, String> {
    if typ != TYPE_ASCII {
        return Err("expected ASCII".into());
    }
    let size = count as usize;
    let raw = if size <= 4 {
        value.to_le_bytes()[..size].to_vec()
    } else {
        let start = value as usize;
        let end = start + size;
        if end > bytes.len() {
            return Err("ASCII past end".into());
        }
        bytes[start..end].to_vec()
    };
    let end = raw.iter().position(|&b| b == 0).unwrap_or(raw.len());
    String::from_utf8(raw[..end].to_vec()).map_err(|e| e.to_string())
}

/// Build a tiny Classic TIFF matching the OPERA COMP layout (tiled Deflate,
/// two float32 samples, ModelTiepoint / ModelPixelScale / GeoDoubleParams).
#[cfg(test)]
#[allow(clippy::too_many_arguments)]
pub fn write_float_cog(
    width: u32,
    height: u32,
    tile_width: u32,
    tile_length: u32,
    geotransform: [f64; 6],
    geo_doubles: &[f64],
    nodata: f32,
    band0: &[f32],
    band1: &[f32],
) -> Result<Vec<u8>, String> {
    if band0.len() != (width * height) as usize || band1.len() != band0.len() {
        return Err("band length must be width*height".into());
    }
    if geotransform[2] != 0.0 || geotransform[4] != 0.0 || geotransform[5] >= 0.0 {
        return Err("writer expects north-up affine with zero rotation".into());
    }
    let tiles_x = width.div_ceil(tile_width);
    let tiles_y = height.div_ceil(tile_length);
    let n_tiles = (tiles_x * tiles_y) as usize;
    let mut tile_blobs = Vec::with_capacity(n_tiles);
    for ty in 0..tiles_y {
        for tx in 0..tiles_x {
            let mut raw = vec![0u8; (tile_width as usize) * (tile_length as usize) * 8];
            for row in 0..tile_length {
                let y = ty * tile_length + row;
                for col in 0..tile_width {
                    let x = tx * tile_width + col;
                    let pix = (row * tile_width + col) as usize;
                    let (v0, v1) = if y < height && x < width {
                        let i = (y * width + x) as usize;
                        (band0[i], band1[i])
                    } else {
                        (nodata, nodata)
                    };
                    raw[pix * 8..pix * 8 + 4].copy_from_slice(&v0.to_le_bytes());
                    raw[pix * 8 + 4..pix * 8 + 8].copy_from_slice(&v1.to_le_bytes());
                }
            }
            tile_blobs.push(deflate(&raw)?);
        }
    }

    let scale = [geotransform[1], -geotransform[5], 0.0];
    let tie = [0.0, 0.0, 0.0, geotransform[0], geotransform[3], 0.0];
    let nodata_ascii = format!("{nodata}\0");

    #[derive(Clone)]
    enum Val {
        Short(u16),
        TwoShorts(u16, u16),
        Long(u32),
        Ext(Vec<u8>),
    }
    let tile_offsets_val = if n_tiles == 1 {
        Val::Long(0) // patched once tiles are placed
    } else {
        Val::Ext(vec![0u8; n_tiles * 4])
    };
    let tile_counts_val = if n_tiles == 1 {
        Val::Long(tile_blobs[0].len() as u32)
    } else {
        Val::Ext(
            tile_blobs
                .iter()
                .flat_map(|b| (b.len() as u32).to_le_bytes())
                .collect(),
        )
    };
    let mut entries: Vec<(u16, u16, u32, Val)> = vec![
        (TAG_IMAGE_WIDTH, TYPE_SHORT, 1, Val::Short(width as u16)),
        (TAG_IMAGE_LENGTH, TYPE_SHORT, 1, Val::Short(height as u16)),
        (TAG_BITS_PER_SAMPLE, TYPE_SHORT, 2, Val::TwoShorts(32, 32)),
        (
            TAG_COMPRESSION,
            TYPE_SHORT,
            1,
            Val::Short(COMPRESSION_DEFLATE),
        ),
        (TAG_PHOTOMETRIC, TYPE_SHORT, 1, Val::Short(1)),
        (TAG_SAMPLES_PER_PIXEL, TYPE_SHORT, 1, Val::Short(2)),
        (TAG_PLANAR_CONFIG, TYPE_SHORT, 1, Val::Short(1)),
        (TAG_TILE_WIDTH, TYPE_SHORT, 1, Val::Short(tile_width as u16)),
        (
            TAG_TILE_LENGTH,
            TYPE_SHORT,
            1,
            Val::Short(tile_length as u16),
        ),
        (
            TAG_SAMPLE_FORMAT,
            TYPE_SHORT,
            2,
            Val::TwoShorts(SAMPLE_FORMAT_FLOAT, SAMPLE_FORMAT_FLOAT),
        ),
        (
            TAG_TILE_OFFSETS,
            TYPE_LONG,
            n_tiles as u32,
            tile_offsets_val,
        ),
        (
            TAG_TILE_BYTE_COUNTS,
            TYPE_LONG,
            n_tiles as u32,
            tile_counts_val,
        ),
        (
            TAG_MODEL_PIXEL_SCALE,
            TYPE_DOUBLE,
            3,
            Val::Ext(scale.iter().flat_map(|v| v.to_le_bytes()).collect()),
        ),
        (
            TAG_MODEL_TIEPOINT,
            TYPE_DOUBLE,
            6,
            Val::Ext(tie.iter().flat_map(|v| v.to_le_bytes()).collect()),
        ),
        (
            TAG_GEO_DOUBLE_PARAMS,
            TYPE_DOUBLE,
            geo_doubles.len() as u32,
            Val::Ext(geo_doubles.iter().flat_map(|v| v.to_le_bytes()).collect()),
        ),
        (
            TAG_GDAL_NODATA,
            TYPE_ASCII,
            nodata_ascii.len() as u32,
            Val::Ext(nodata_ascii.into_bytes()),
        ),
    ];
    entries.sort_by_key(|(tag, ..)| *tag);

    let ifd_size = 2 + entries.len() * 12 + 4;
    let ext_start = 8 + ifd_size;
    let mut ext = Vec::new();
    let mut resolved: Vec<(u16, u16, u32, u32)> = Vec::new();
    let mut tile_offsets_ext_at = None;
    let mut tile_offsets_inline = false;
    for (tag, typ, count, val) in &entries {
        let value_field = match val {
            Val::Short(v) => u32::from(*v),
            Val::TwoShorts(a, b) => u32::from(*a) | (u32::from(*b) << 16),
            Val::Long(v) => {
                if *tag == TAG_TILE_OFFSETS {
                    tile_offsets_inline = true;
                }
                *v
            }
            Val::Ext(blob) => {
                if ext.len() % 2 == 1 {
                    ext.push(0);
                }
                let off = (ext_start + ext.len()) as u32;
                if *tag == TAG_TILE_OFFSETS {
                    tile_offsets_ext_at = Some(ext.len());
                }
                ext.extend_from_slice(blob);
                off
            }
        };
        resolved.push((*tag, *typ, *count, value_field));
    }

    let tiles_start = ext_start + ext.len();
    let mut tile_data = Vec::new();
    let mut offsets = Vec::with_capacity(n_tiles);
    for blob in &tile_blobs {
        offsets.push((tiles_start + tile_data.len()) as u32);
        tile_data.extend_from_slice(blob);
    }
    if let Some(at) = tile_offsets_ext_at {
        for (i, off) in offsets.iter().enumerate() {
            ext[at + i * 4..at + i * 4 + 4].copy_from_slice(&off.to_le_bytes());
        }
    }
    if tile_offsets_inline {
        for (tag, _, _, value) in &mut resolved {
            if *tag == TAG_TILE_OFFSETS {
                *value = offsets[0];
            }
        }
    }

    let mut out = Vec::with_capacity(tiles_start + tile_data.len());
    out.extend_from_slice(b"II");
    out.extend_from_slice(&42u16.to_le_bytes());
    out.extend_from_slice(&8u32.to_le_bytes());
    out.extend_from_slice(&(resolved.len() as u16).to_le_bytes());
    for (tag, typ, count, value) in resolved {
        out.extend_from_slice(&tag.to_le_bytes());
        out.extend_from_slice(&typ.to_le_bytes());
        out.extend_from_slice(&count.to_le_bytes());
        out.extend_from_slice(&value.to_le_bytes());
    }
    out.extend_from_slice(&0u32.to_le_bytes());
    debug_assert_eq!(out.len(), ext_start);
    out.extend_from_slice(&ext);
    out.extend_from_slice(&tile_data);
    Ok(out)
}

#[cfg(test)]
fn deflate(raw: &[u8]) -> Result<Vec<u8>, String> {
    let mut enc = ZlibEncoder::new(Vec::new(), Compression::fast());
    enc.write_all(raw).map_err(|e| e.to_string())?;
    enc.finish().map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn set_inline_ifd_value(bytes: &mut [u8], wanted_tag: u16, value: u32) {
        let ifd = u32::from_le_bytes(bytes[4..8].try_into().unwrap()) as usize;
        let entries = u16::from_le_bytes(bytes[ifd..ifd + 2].try_into().unwrap()) as usize;
        for entry in bytes[ifd + 2..ifd + 2 + entries * 12]
            .as_chunks_mut::<12>()
            .0
        {
            let tag = u16::from_le_bytes(entry[..2].try_into().unwrap());
            if tag == wanted_tag {
                entry[8..12].copy_from_slice(&value.to_le_bytes());
                return;
            }
        }
        panic!("fixture has no tag {wanted_tag}");
    }

    fn tiny_cog() -> Vec<u8> {
        let width = 16;
        let height = 16;
        write_float_cog(
            width,
            height,
            16,
            16,
            [-500.0, 1000.0, 0.0, 500.0, 0.0, -1000.0],
            &[55.0, 10.0, 1_950_000.0, -2_100_000.0],
            -9999000.0,
            &vec![0.0; (width * height) as usize],
            &vec![0.0; (width * height) as usize],
        )
        .unwrap()
    }

    #[test]
    fn round_trip_tiny_cog() {
        let width = 16u32;
        let height = 16u32;
        let mut band0 = vec![f32::NAN; (width * height) as usize];
        let band1 = vec![-9999000f32; band0.len()];
        band0[0] = 12.5;
        band0[1] = -9999000.0;
        band0[2] = 40.0;
        let gt = [-500.0, 1000.0, 0.0, 500.0, 0.0, -1000.0];
        let doubles = [
            55.0,
            10.0,
            1_950_000.0,
            -2_100_000.0,
            298.257223563,
            6_378_137.0,
        ];
        let bytes = write_float_cog(
            width, height, 16, 16, gt, &doubles, -9999000.0, &band0, &band1,
        )
        .unwrap();
        let decoded = decode_float_cog(&bytes).unwrap();
        assert_eq!((decoded.width, decoded.height), (width, height));
        assert_eq!(decoded.geotransform, gt);
        assert_eq!(&decoded.geo_double_params[..6], &doubles);
        assert_eq!(decoded.nodata, Some(-9999000.0));
        assert!((decoded.values[0] - 12.5).abs() < 1e-6);
        assert!((decoded.values[1] - -9999000.0).abs() < 1.0);
        assert!((decoded.values[2] - 40.0).abs() < 1e-6);
        assert!(decoded.values[3].is_nan());
    }

    #[test]
    fn rejects_oversized_raster_dimension_before_allocation() {
        let mut bytes = tiny_cog();
        set_inline_ifd_value(&mut bytes, TAG_IMAGE_WIDTH, MAX_RASTER_DIMENSION + 1);

        let error = decode_float_cog(&bytes).unwrap_err();
        assert!(error.contains("raster dimensions"), "{error}");
    }

    #[test]
    fn rejects_raster_over_pixel_budget_before_allocation() {
        let mut bytes = tiny_cog();
        set_inline_ifd_value(&mut bytes, TAG_IMAGE_WIDTH, 5_000);
        set_inline_ifd_value(&mut bytes, TAG_IMAGE_LENGTH, 5_000);

        let error = decode_float_cog(&bytes).unwrap_err();
        assert!(error.contains("pixel"), "{error}");
    }

    #[test]
    fn rejects_oversized_tile_before_inflate() {
        let mut bytes = tiny_cog();
        set_inline_ifd_value(&mut bytes, TAG_TILE_WIDTH, 2_048);
        set_inline_ifd_value(&mut bytes, TAG_TILE_LENGTH, 2_048);

        let error = decode_float_cog(&bytes).unwrap_err();
        assert!(error.contains("inflated tile size"), "{error}");
    }

    #[test]
    fn inflate_rejects_output_larger_than_expected() {
        let compressed = deflate(&vec![7; 1_025]).unwrap();

        let error = inflate_exact(&compressed, 1_024).unwrap_err();
        assert!(error.contains("exceeds expected size"), "{error}");
    }
}
