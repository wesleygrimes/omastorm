//! Bounded decoder for NOAA MRMS CONUS MergedBaseReflectivityQC.
//! Retrieval and source selection are separate. See `docs/mrms-decoder.md`
//! for the narrow input profile and provider-specific CRS normalization.

use crate::protocol::{Crs, FrameStatus, MosaicFrame};
use chrono::{NaiveDate, NaiveDateTime};
use flate2::bufread::GzDecoder;
use std::io::{self, Cursor, Write};

mod live;
pub use live::{HISTORY, Mrms};
pub const ID: &str = "mrms-conus";

const BODY_MAX: usize = 64 << 20;
const PNG_MEMORY_MAX: usize = 16 << 20;
const TEXTURE_MAX: usize = 32 << 20;
const WIDTH: u32 = 7000;
const HEIGHT: u32 = 3500;
const CELLS: usize = WIDTH as usize * HEIGHT as usize;
const AFFINE: [f64; 6] = [-130.0, 0.01, 0.0, 55.0, 0.0, -0.01];
const ENDPOINT_TOLERANCE: f64 = 0.000003;

/// Takes ownership so compressed bytes and the inflated GRIB can be released
/// before encoding the display PNG. `stamp` is the object's UTC filename time.
/// No files or network are accessed; the caller owns persistence and scheduling.
pub fn decode_frame(
    bytes: Vec<u8>,
    stamp: NaiveDateTime,
    palette: &[String],
    bounds: &[f64],
) -> Result<(MosaicFrame, Vec<u8>, i64), String> {
    validate_palette(palette, bounds)?;
    let grib = inflate(&bytes, BODY_MAX)?;
    drop(bytes);
    let message = parse_message(&grib, stamp)?;
    message.grid.validate_conus()?;
    let pixels = classify_png(&message, palette.len(), bounds)?;
    drop(grib);
    let texture = encode_texture(WIDTH, HEIGHT, &pixels)?;
    let frame = MosaicFrame {
        id: format!("{ID}-{}", stamp.format("%Y%m%dT%H%M%SZ")),
        product: "REF".into(),
        product_name: "QC Base Reflectivity".into(),
        units: "dBZ".into(),
        scan_time: stamp.format("%Y-%m-%dT%H:%M:%SZ").to_string(),
        sweep_end: None,
        status: FrameStatus::Complete,
        texture: String::new(),
        width: WIDTH,
        height: HEIGHT,
        // Shape code 2 is IAU1965, not WGS84. Only this validated NOAA
        // profile is normalized to its matching EPSG:4326 GIS product.
        crs: Crs::wgs84_geographic(),
        geotransform: AFFINE,
        palette: palette.to_vec(),
        bounds: bounds.to_vec(),
    };
    Ok((frame, texture, stamp.and_utc().timestamp_millis()))
}

fn validate_palette(palette: &[String], bounds: &[f64]) -> Result<(), String> {
    if palette.is_empty()
        || palette.len() > 255
        || bounds.len() != palette.len() + 1
        || bounds.iter().any(|x| !x.is_finite())
        || bounds.windows(2).any(|w| w[0] >= w[1])
    {
        return Err("MRMS: expected 1..255 palette classes and increasing finite bounds".into());
    }
    Ok(())
}

/// Refuse a write before either length or requested capacity exceeds the cap.
/// Used for both gzip expansion and the encoded texture (Vec alone is unbounded).
struct BoundedBytes {
    bytes: Vec<u8>,
    limit: usize,
}

impl BoundedBytes {
    fn new(limit: usize) -> Self {
        Self {
            bytes: Vec::new(),
            limit,
        }
    }
}

impl Write for BoundedBytes {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let needed = self
            .bytes
            .len()
            .checked_add(buf.len())
            .filter(|&n| n <= self.limit)
            .ok_or_else(|| io::Error::other("MRMS byte limit exceeded"))?;
        if needed > self.bytes.capacity() {
            let capacity = needed
                .checked_next_power_of_two()
                .unwrap_or(self.limit)
                .min(self.limit);
            self.bytes
                .try_reserve_exact(capacity - self.bytes.len())
                .map_err(io::Error::other)?;
        }
        self.bytes.extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

fn inflate(bytes: &[u8], limit: usize) -> Result<Vec<u8>, String> {
    if bytes.len() > limit {
        return Err("MRMS gzip body exceeds byte limit".into());
    }
    let mut decoder = GzDecoder::new(bytes);
    let mut output = BoundedBytes::new(limit);
    // Read through EOF, including gzip CRC/length verification. bufread's
    // single-member decoder leaves all trailing bytes available for rejection.
    io::copy(&mut decoder, &mut output).map_err(|e| format!("MRMS gzip: {e}"))?;
    if !decoder.into_inner().is_empty() {
        return Err("MRMS gzip: extra member or trailing bytes".into());
    }
    Ok(output.bytes)
}

struct Message<'a> {
    grid: Grid,
    packing: Packing,
    png: &'a [u8],
}

/// Each field accessor below is used only after the enclosing section has
/// passed an exact-length check. Variable-length records use checked slices.
fn u16_at(b: &[u8], i: usize) -> u16 {
    u16::from_be_bytes(b[i..i + 2].try_into().expect("validated field length"))
}
fn u32_at(b: &[u8], i: usize) -> u32 {
    u32::from_be_bytes(b[i..i + 4].try_into().expect("validated field length"))
}
fn signed(value: u32, sign: u32) -> i64 {
    let magnitude = i64::from(value & (sign - 1));
    if value & sign == 0 {
        magnitude
    } else {
        -magnitude
    }
}

fn section<'a>(
    remaining: &mut &'a [u8],
    number: u8,
    size: Option<usize>,
) -> Result<&'a [u8], String> {
    if remaining.len() < 5 {
        return Err(format!("MRMS GRIB: truncated section {number}"));
    }
    let n = u32_at(remaining, 0) as usize;
    if remaining[4] != number || n < 5 || size.is_some_and(|s| n != s) {
        return Err(format!(
            "MRMS GRIB: invalid section {number} order or length"
        ));
    }
    let bytes = remaining
        .get(..n)
        .ok_or_else(|| format!("MRMS GRIB: truncated section {number}"))?;
    *remaining = &remaining[n..];
    Ok(bytes)
}

fn parse_message(bytes: &[u8], stamp: NaiveDateTime) -> Result<Message<'_>, String> {
    if bytes.len() < 16 || &bytes[..8] != b"GRIB\0\0\xd1\x02" {
        return Err("MRMS GRIB: expected edition 2, discipline 209".into());
    }
    let length = u64::from_be_bytes(bytes[8..16].try_into().expect("header checked"));
    if length != bytes.len() as u64 || length > BODY_MAX as u64 {
        return Err("MRMS GRIB: invalid message length".into());
    }
    let mut remaining = &bytes[16..];
    let id = section(&mut remaining, 1, Some(21))?;
    if id[5..12] != [0, 161, 0, 0, 255, 1, 3] || id[19..21] != [2, 7] {
        return Err("MRMS GRIB: unsupported origin, table, or observation metadata".into());
    }
    let observed = NaiveDate::from_ymd_opt(i32::from(u16_at(id, 12)), id[14].into(), id[15].into())
        .and_then(|date| date.and_hms_opt(id[16].into(), id[17].into(), id[18].into()))
        .ok_or("MRMS GRIB: invalid observation time")?;
    if observed != stamp {
        return Err("MRMS GRIB: observation time differs from object timestamp".into());
    }
    let grid = parse_grid(section(&mut remaining, 3, Some(72))?)?;
    let product = section(&mut remaining, 4, Some(34))?;
    if u16_at(product, 5) != 0 // no coordinate values
        || u16_at(product, 7) != 0 // template 4.0
        || product[9..14] != [11, 0, 8, 0, 97] // parameter and generating process
        || product[14..22] != [0; 8] // no cutoff or forecast lead, time unit 0
        || product[22..24] != [102, 0] // altitude above MSL, unscaled metres
        || u32_at(product, 24) != 500
        || product[28] != 255
    // no second surface
    {
        return Err("MRMS GRIB: expected observed QC base reflectivity at 500 m MSL".into());
    }
    // Scale/value of the absent second surface have no interpretation.
    let packed = section(&mut remaining, 5, Some(21))?;
    if u32_at(packed, 5) as usize != grid.cells()
        || u16_at(packed, 9) != 41
        || packed[19..21] != [16, 0]
    {
        return Err("MRMS GRIB: expected packing 5.41, uint16 PNG, and matching cell count".into());
    }
    let packing = Packing::new(
        f32::from_bits(u32_at(packed, 11)),
        signed(u16_at(packed, 15).into(), 0x8000) as i32,
        signed(u16_at(packed, 17).into(), 0x8000) as i32,
    )?;
    if section(&mut remaining, 6, Some(6))?[5] != 255 {
        return Err("MRMS GRIB: bitmaps are unsupported".into());
    }
    let data = section(&mut remaining, 7, None)?;
    if remaining != b"7777" {
        return Err("MRMS GRIB: missing terminator or extra field/message".into());
    }
    Ok(Message {
        grid,
        packing,
        png: &data[5..],
    })
}

#[derive(Debug)]
struct Grid {
    width: u32,
    height: u32,
    affine: [f64; 6],
}

impl Grid {
    fn cells(&self) -> usize {
        self.width as usize * self.height as usize
    }

    fn validate_conus(&self) -> Result<(), String> {
        if self.width != WIDTH
            || self.height != HEIGHT
            || self
                .affine
                .iter()
                .zip(AFFINE)
                .any(|(a, b)| (a - b).abs() > 1e-10)
        {
            return Err("MRMS GRIB: unsupported CONUS grid footprint or dimensions".into());
        }
        Ok(())
    }
}

fn parse_grid(s: &[u8]) -> Result<Grid, String> {
    if s[5] != 0 || s[10..15] != [0, 0, 0, 0, 2] || s[54] != 48 || s[71] != 0 {
        return Err("MRMS GRIB: unsupported grid template, earth shape, or orientation".into());
    }
    // Earth-axis scale/value slots are irrelevant for predefined shape 2.
    let width = u32_at(s, 30);
    let height = u32_at(s, 34);
    let cells = (width as usize).checked_mul(height as usize);
    if width == 0
        || height == 0
        || width > WIDTH
        || height > HEIGHT
        || cells.is_none_or(|n| n > CELLS || n != u32_at(s, 6) as usize)
    {
        return Err("MRMS GRIB: invalid grid dimensions or point count".into());
    }
    let unit = match (u32_at(s, 38), u32_at(s, 42)) {
        (1, 1_000_000) | (0, u32::MAX) => 1e-6,
        _ => return Err("MRMS GRIB: unsupported angular units".into()),
    };
    let lat = signed(u32_at(s, 46), 0x8000_0000) as f64 * unit;
    let lon = u32_at(s, 50) as f64 * unit;
    let last_lat = signed(u32_at(s, 55), 0x8000_0000) as f64 * unit;
    let last_lon = u32_at(s, 59) as f64 * unit;
    let dx = u32_at(s, 63) as f64 * unit;
    let dy = u32_at(s, 67) as f64 * unit;
    if dx == 0.0
        || dy == 0.0
        || lat.abs() > 90.0
        || last_lat.abs() > 90.0
        || lon >= 360.0
        || last_lon >= 360.0
        || (last_lat - (lat - f64::from(height - 1) * dy)).abs() > ENDPOINT_TOLERANCE + 1e-12
        || (last_lon - (lon + f64::from(width - 1) * dx)).abs() > ENDPOINT_TOLERANCE + 1e-12
    {
        return Err("MRMS GRIB: inconsistent grid endpoints or increments".into());
    }
    let west_center = if lon > 180.0 { lon - 360.0 } else { lon };
    Ok(Grid {
        width,
        height,
        affine: [west_center - dx / 2.0, dx, 0.0, lat + dy / 2.0, 0.0, -dy],
    })
}

#[derive(Debug, PartialEq)]
enum Value {
    Measured(f64),
    Missing,
    NoCoverage,
}

struct Packing {
    reference: f64,
    binary: f64,
    decimal: f64,
}

impl Packing {
    fn new(reference: f32, binary: i32, decimal: i32) -> Result<Self, String> {
        if !reference.is_finite() || !(-32..=32).contains(&binary) || !(-32..=32).contains(&decimal)
        {
            return Err("MRMS GRIB: invalid reference or unsupported scale exponent".into());
        }
        Ok(Self {
            reference: reference.into(),
            binary: 2_f64.powi(binary),
            decimal: 10_f64.powi(decimal),
        })
    }

    fn value(&self, sample: u16) -> Result<Value, String> {
        let value = (self.reference + f64::from(sample) * self.binary) / self.decimal;
        match value {
            -99.0 => Ok(Value::Missing),
            -999.0 => Ok(Value::NoCoverage),
            v if v.is_finite() => Ok(Value::Measured(v)),
            _ => Err("MRMS GRIB: nonfinite physical value".into()),
        }
    }
}

fn pixel(value: Value, classes: usize, bounds: &[f64]) -> [u8; 4] {
    match value {
        Value::Missing | Value::NoCoverage => [0, 1, 0, 255],
        Value::Measured(v) => {
            let class = bounds
                .partition_point(|&edge| edge <= v)
                .saturating_sub(1)
                .min(classes - 1);
            [class as u8 + 1, 0, 0, 255]
        }
    }
}

/// Inspect structure and dimensions before the PNG library can allocate.
/// Only IHDR/IDAT/IEND are needed by this packing profile; ancillary chunks
/// (including compressed metadata and APNG) are deliberately unsupported.
/// The PNG reader subsequently verifies critical CRCs. Its pixel reader is
/// intentionally permissive about excess/truncated zlib tails, so also validate
/// exact filtered-byte count and zlib EOF here with bounded scratch storage.
fn validate_png(bytes: &[u8], grid: &Grid) -> Result<(), String> {
    if bytes.get(..8) != Some(b"\x89PNG\r\n\x1a\n") {
        return Err("MRMS PNG: invalid signature".into());
    }
    let mut remaining = &bytes[8..];
    let mut header = false;
    let mut data = false;
    let mut inflater = flate2::Decompress::new(true);
    let mut ended = false;
    let expected = (u64::from(grid.width) * 2 + 1) * u64::from(grid.height);
    let mut scratch = [0; 32 << 10];
    while remaining.len() >= 12 {
        let len = u32_at(remaining, 0) as usize;
        let chunk_size = len
            .checked_add(12)
            .ok_or("MRMS PNG: chunk length overflow")?;
        let chunk = remaining
            .get(..chunk_size)
            .ok_or("MRMS PNG: truncated chunk")?;
        match &chunk[4..8] {
            b"IHDR" if !header && len == 13 => {
                if u32_at(chunk, 8) != grid.width
                    || u32_at(chunk, 12) != grid.height
                    || chunk[16..21] != [16, 0, 0, 0, 0]
                {
                    return Err(
                        "MRMS PNG: expected matching grayscale16 noninterlaced raster".into(),
                    );
                }
                header = true;
            }
            b"IDAT" if header => {
                data = true;
                let mut input = &chunk[8..8 + len];
                loop {
                    if ended {
                        if !input.is_empty() {
                            return Err("MRMS PNG: bytes after zlib stream".into());
                        }
                        break;
                    }
                    let before_in = inflater.total_in();
                    let before_out = inflater.total_out();
                    // One extra byte detects expansion beyond the exact raster.
                    let space = scratch.len().min((expected - before_out + 1) as usize);
                    let status = inflater
                        .decompress(input, &mut scratch[..space], flate2::FlushDecompress::None)
                        .map_err(|e| format!("MRMS PNG zlib: {e}"))?;
                    let used = (inflater.total_in() - before_in) as usize;
                    let produced = inflater.total_out() - before_out;
                    input = &input[used..];
                    if inflater.total_out() > expected {
                        return Err("MRMS PNG: excess filtered raster bytes".into());
                    }
                    if status == flate2::Status::StreamEnd {
                        if inflater.total_out() != expected {
                            return Err("MRMS PNG: short filtered raster".into());
                        }
                        ended = true;
                    } else if used == 0 && produced == 0 {
                        if !input.is_empty() {
                            return Err("MRMS PNG: stalled zlib stream".into());
                        }
                        break;
                    }
                }
            }
            b"IEND" if data && ended && len == 0 && remaining.len() == 12 => return Ok(()),
            _ => return Err("MRMS PNG: unsupported chunk, order, or trailing data".into()),
        }
        remaining = &remaining[chunk_size..];
    }
    Err("MRMS PNG: missing IEND or truncated chunk".into())
}

fn classify_png(message: &Message<'_>, classes: usize, bounds: &[f64]) -> Result<Vec<u8>, String> {
    let grid = &message.grid;
    validate_png(message.png, grid)?;
    let lut: Vec<[u8; 4]> = (0..=u16::MAX)
        .map(|sample| {
            message
                .packing
                .value(sample)
                .map(|v| pixel(v, classes, bounds))
        })
        .collect::<Result<_, _>>()?;
    let mut decoder = png::Decoder::new_with_limits(
        Cursor::new(message.png),
        png::Limits {
            bytes: PNG_MEMORY_MAX,
        },
    );
    decoder.ignore_checksums(false);
    let mut reader = decoder
        .read_info()
        .map_err(|e| format!("MRMS PNG header: {e}"))?;
    let mut pixels = Vec::new();
    pixels
        .try_reserve_exact(grid.cells() * 4)
        .map_err(|e| format!("MRMS raster allocation: {e}"))?;
    for _ in 0..grid.height {
        let row = reader
            .next_row()
            .map_err(|e| format!("MRMS PNG row: {e}"))?
            .ok_or("MRMS PNG: missing row")?;
        if row.data().len() != grid.width as usize * 2 {
            return Err("MRMS PNG: incorrect row length".into());
        }
        for &pair in row.data().as_chunks::<2>().0 {
            pixels.extend_from_slice(&lut[u16::from_be_bytes(pair) as usize]);
        }
    }
    if reader
        .next_row()
        .map_err(|e| format!("MRMS PNG end: {e}"))?
        .is_some()
    {
        return Err("MRMS PNG: extra row".into());
    }
    reader
        .finish()
        .map_err(|e| format!("MRMS PNG trailer: {e}"))?;
    Ok(pixels)
}

fn encode_texture(width: u32, height: u32, pixels: &[u8]) -> Result<Vec<u8>, String> {
    let mut output = BoundedBytes::new(TEXTURE_MAX);
    let mut encoder = png::Encoder::new(&mut output, width, height);
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    encoder.set_compression(png::Compression::Fast);
    let mut writer = encoder
        .write_header()
        .map_err(|e| format!("MRMS texture header: {e}"))?;
    // write_image_data buffers the whole compressed stream internally before
    // touching our capped writer. Streaming bounds that intermediate as well.
    let mut stream = writer
        .stream_writer_with_size(64 << 10)
        .map_err(|e| format!("MRMS texture stream: {e}"))?;
    stream
        .write_all(pixels)
        .map_err(|e| format!("MRMS texture pixels: {e}"))?;
    stream
        .finish()
        .map_err(|e| format!("MRMS texture stream end: {e}"))?;
    writer
        .finish()
        .map_err(|e| format!("MRMS texture trailer: {e}"))?;
    Ok(output.bytes)
}

#[cfg(test)]
mod tests;
