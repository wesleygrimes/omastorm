use super::*;
use flate2::{Compression, write::GzEncoder};

fn stamp() -> NaiveDateTime {
    NaiveDate::from_ymd_opt(2026, 9, 26)
        .unwrap()
        .and_hms_opt(18, 21, 39)
        .unwrap()
}

fn palette() -> (Vec<String>, Vec<f64>) {
    let template: crate::protocol::Frame =
        serde_json::from_str(include_str!("../../data/product.json")).unwrap();
    (
        template.palette,
        template.bounds.into_iter().map(f64::from).collect(),
    )
}

fn put16(b: &mut [u8], i: usize, v: u16) {
    b[i..i + 2].copy_from_slice(&v.to_be_bytes());
}
fn put32(b: &mut [u8], i: usize, v: u32) {
    b[i..i + 4].copy_from_slice(&v.to_be_bytes());
}

fn gzip(bytes: &[u8]) -> Vec<u8> {
    let mut writer = GzEncoder::new(Vec::new(), Compression::fast());
    writer.write_all(bytes).unwrap();
    writer.finish().unwrap()
}

fn packed_png(width: u32, height: u32, samples: &[u16]) -> Vec<u8> {
    assert_eq!(samples.len(), (width * height) as usize);
    let mut bytes = Vec::new();
    let mut encoder = png::Encoder::new(&mut bytes, width, height);
    encoder.set_color(png::ColorType::Grayscale);
    encoder.set_depth(png::BitDepth::Sixteen);
    let mut writer = encoder.write_header().unwrap();
    let raw: Vec<u8> = samples.iter().flat_map(|x| x.to_be_bytes()).collect();
    writer.write_image_data(&raw).unwrap();
    writer.finish().unwrap();
    bytes
}

/// Construct format bytes from the documented fields, with entirely invented
/// measurements. No downloaded NOAA data is embedded in the test binary.
struct Fixture {
    sections: Vec<Vec<u8>>,
}

impl Fixture {
    fn new() -> Self {
        let mut sections: Vec<Vec<u8>> = [(1, 21), (3, 72), (4, 34), (5, 21), (6, 6), (7, 5)]
            .into_iter()
            .map(|(number, size)| {
                let mut s = vec![0; size];
                put32(&mut s, 0, size as u32);
                s[4] = number;
                s
            })
            .collect();
        let id = &mut sections[0];
        id[5..12].copy_from_slice(&[0, 161, 0, 0, 255, 1, 3]);
        put16(id, 12, 2026);
        id[14..21].copy_from_slice(&[9, 26, 18, 21, 39, 2, 7]);
        let grid = &mut sections[1];
        grid[14] = 2;
        put32(grid, 38, 1);
        put32(grid, 42, 1_000_000);
        put32(grid, 46, 54_995_000);
        put32(grid, 50, 230_005_000);
        grid[54] = 48;
        put32(grid, 63, 10_000);
        put32(grid, 67, 10_000);
        let product = &mut sections[2];
        product[9..14].copy_from_slice(&[11, 0, 8, 0, 97]);
        product[22] = 102;
        put32(product, 24, 500);
        product[28] = 255;
        let packing = &mut sections[3];
        put16(packing, 9, 41);
        put32(packing, 11, (-9990_f32).to_bits());
        put16(packing, 17, 1);
        packing[19] = 16;
        sections[4][5] = 255;
        let mut fixture = Self { sections };
        fixture.dimensions(4, 2);
        fixture.png(packed_png(
            4,
            2,
            &[0, 9000, 9815, 9990, 10090, 10190, 10200, 10695],
        ));
        fixture
    }

    fn dimensions(&mut self, width: u32, height: u32) {
        let grid = &mut self.sections[1];
        put32(grid, 6, width * height);
        put32(grid, 30, width);
        put32(grid, 34, height);
        put32(grid, 55, 54_995_000 - (height - 1) * 10_000);
        put32(grid, 59, 230_005_000 + (width - 1) * 10_000);
        put32(&mut self.sections[3], 5, width * height);
    }

    fn png(&mut self, png: Vec<u8>) {
        self.sections[5].truncate(5);
        self.sections[5].extend(png);
    }

    fn grib(&self) -> Vec<u8> {
        let mut bytes = b"GRIB\0\0\xd1\x02\0\0\0\0\0\0\0\0".to_vec();
        for section in &self.sections {
            let start = bytes.len();
            bytes.extend(section);
            put32(&mut bytes, start, section.len() as u32);
        }
        bytes.extend(b"7777");
        let size = bytes.len() as u64;
        bytes[8..16].copy_from_slice(&size.to_be_bytes());
        bytes
    }
}

#[test]
fn physical_values_are_classified_after_big_endian_unpacking() {
    let fixture = Fixture::new().grib();
    let message = parse_message(&fixture, stamp()).unwrap();
    let (palette, bounds) = palette();
    let pixels = classify_png(&message, palette.len(), &bounds).unwrap();
    let expected = [
        [0, 1, 0, 255],
        [0, 1, 0, 255],
        [1, 0, 0, 255],
        [2, 0, 0, 255],
        [3, 0, 0, 255],
        [4, 0, 0, 255],
        [4, 0, 0, 255],
        [12, 0, 0, 255],
    ];
    assert_eq!(pixels, expected.concat());
    assert_eq!(message.packing.value(0).unwrap(), Value::NoCoverage);
    assert_eq!(message.packing.value(9000).unwrap(), Value::Missing);
    assert_eq!(message.packing.value(9815).unwrap(), Value::Measured(-17.5));
    assert_eq!(message.packing.value(10695).unwrap(), Value::Measured(70.5));
    let encoded = encode_texture(4, 2, &pixels).unwrap();
    let mut reader = png::Decoder::new(Cursor::new(encoded)).read_info().unwrap();
    let mut decoded = vec![0; 32];
    reader.next_frame(&mut decoded).unwrap();
    reader.finish().unwrap();
    assert_eq!(decoded, pixels);
}

#[test]
fn scale_exponents_use_sign_magnitude_and_sentinels_use_physical_values() {
    let mut fixture = Fixture::new();
    let s = &mut fixture.sections[3];
    put32(s, 11, (-999_f32).to_bits());
    put16(s, 15, 0x8001); // E = -1, not -32767
    put16(s, 17, 0);
    fixture.png(packed_png(
        4,
        2,
        &[0, 1800, 1943, 1998, 2018, 2038, 2067, 65535],
    ));
    let grib = fixture.grib();
    let message = parse_message(&grib, stamp()).unwrap();
    let (palette, bounds) = palette();
    let pixels = classify_png(&message, palette.len(), &bounds).unwrap();
    assert_eq!(&pixels[..12], &[0, 1, 0, 255, 0, 1, 0, 255, 1, 0, 0, 255]);
    assert_eq!(message.packing.value(1943).unwrap(), Value::Measured(-27.5));
    assert_eq!(message.packing.value(2067).unwrap(), Value::Measured(34.5));

    put32(&mut fixture.sections[3], 11, (-100_f32).to_bits());
    put16(&mut fixture.sections[3], 17, 0x8001); // D = -1
    let grib = fixture.grib();
    let message = parse_message(&grib, stamp()).unwrap();
    assert_eq!(message.packing.value(201).unwrap(), Value::Measured(5.0));
    let positive = Packing::new(-999.0, 1, 0).unwrap();
    assert_eq!(positive.value(450).unwrap(), Value::Missing);
    assert_eq!(positive.value(512).unwrap(), Value::Measured(25.0));
    assert_eq!(signed(0x8000_000a, 0x8000_0000), -10);
}

#[test]
fn palette_edges_clamp_and_neither_nonmeasurement_becomes_undetect() {
    let (palette, bounds) = palette();
    for (index, &edge) in bounds.iter().take(palette.len()).enumerate() {
        assert_eq!(
            pixel(Value::Measured(edge), palette.len(), &bounds)[0],
            index as u8 + 1
        );
        if index > 0 {
            assert_eq!(
                pixel(Value::Measured(edge - 0.001), palette.len(), &bounds)[0],
                index as u8
            );
        }
    }
    assert_eq!(
        pixel(Value::Measured(-100.0), palette.len(), &bounds),
        [1, 0, 0, 255]
    );
    assert_eq!(
        pixel(Value::Measured(1000.0), palette.len(), &bounds),
        [12, 0, 0, 255]
    );
    for value in [Value::Missing, Value::NoCoverage] {
        assert_eq!(pixel(value, palette.len(), &bounds), [0, 1, 0, 255]);
    }
    let largest = vec![String::new(); 255];
    let edges: Vec<f64> = (0..=255).map(f64::from).collect();
    validate_palette(&largest, &edges).unwrap();
    assert_eq!(pixel(Value::Measured(255.0), 255, &edges)[0], 255);
    for edges in [
        vec![],
        vec![0.0],
        vec![0.0, 0.0],
        vec![1.0, 0.0],
        vec![0.0, f64::NAN],
    ] {
        assert!(validate_palette(&[String::new()], &edges).is_err());
    }
    assert!(validate_palette(&[], &[0.0]).is_err());
    assert!(
        validate_palette(
            &vec![String::new(); 256],
            &(0..=256).map(f64::from).collect::<Vec<_>>()
        )
        .is_err()
    );
}

#[test]
fn native_centers_corners_and_half_open_extent_are_preserved() {
    let mut fixture = Fixture::new();
    fixture.dimensions(WIDTH, HEIGHT);
    // NOAA's last-center quantization must not stretch the grid.
    put32(&mut fixture.sections[1], 55, 20_005_001);
    put32(&mut fixture.sections[1], 59, 299_994_998);
    let grid = parse_grid(&fixture.sections[1]).unwrap();
    grid.validate_conus().unwrap();
    let [x, dx, _, y, _, dy] = grid.affine;
    for (actual, expected) in [
        (x, -130.0),
        (y, 55.0),
        (x + dx / 2.0, -129.995),
        (y + dy / 2.0, 54.995),
        (x + (f64::from(WIDTH) - 0.5) * dx, -60.005),
        (y + (f64::from(HEIGHT) - 0.5) * dy, 20.005),
    ] {
        assert!((actual - expected).abs() < 1e-10, "{actual} != {expected}");
    }
    let cell = |lon: f64, lat: f64| {
        let col = (lon - AFFINE[0]) / AFFINE[1];
        let row = (lat - AFFINE[3]) / AFFINE[5];
        if col < 0.0 || row < 0.0 || col >= f64::from(WIDTH) || row >= f64::from(HEIGHT) {
            None
        } else {
            Some((col.floor() as u32, row.floor() as u32))
        }
    };
    assert_eq!(cell(-129.995, 54.995), Some((0, 0)));
    assert_eq!(cell(-60.005, 20.005), Some((6999, 3499)));
    for (lon, lat) in [
        (-130.001, 30.0),
        (-60.0, 30.0),
        (-100.0, 55.001),
        (-100.0, 20.0),
    ] {
        assert_eq!(cell(lon, lat), None);
    }
    put32(&mut fixture.sections[1], 38, 0);
    put32(&mut fixture.sections[1], 42, u32::MAX);
    parse_grid(&fixture.sections[1])
        .unwrap()
        .validate_conus()
        .unwrap();
    put32(&mut fixture.sections[1], 59, 299_994_997);
    parse_grid(&fixture.sections[1]).unwrap(); // exactly 3 microdegrees
    put32(&mut fixture.sections[1], 59, 299_994_996);
    assert!(parse_grid(&fixture.sections[1]).is_err());
    assert!(
        parse_grid(&Fixture::new().sections[1])
            .unwrap()
            .validate_conus()
            .is_err()
    );
}

#[test]
fn wrong_source_time_product_and_format_fields_are_rejected() {
    // Section index, byte offset, replacement. Mutations exercise each accepted
    // metadata constraint, including scan reversal/transposition and forecasts.
    let mutations = [
        (0, 6, 160),
        (0, 8, 1),
        (0, 9, 0),
        (0, 10, 2),
        (0, 11, 1),
        (0, 14, 13),
        (0, 15, 32),
        (0, 16, 24),
        (0, 17, 60),
        (0, 18, 60),
        (0, 19, 0),
        (0, 20, 1),
        (1, 5, 1),
        (1, 9, 9),
        (1, 10, 1),
        (1, 11, 1),
        (1, 13, 1),
        (1, 14, 5),
        (1, 54, 0),
        (1, 71, 0x80),
        (1, 71, 0x40),
        (1, 71, 0x20),
        (1, 71, 0x10),
        (2, 6, 1),
        (2, 8, 8),
        (2, 9, 10),
        (2, 10, 1),
        (2, 11, 2),
        (2, 12, 1),
        (2, 13, 98),
        (2, 15, 1),
        (2, 16, 1),
        (2, 17, 1),
        (2, 21, 1),
        (2, 22, 103),
        (2, 23, 1),
        (2, 27, 0),
        (2, 28, 100),
        (3, 8, 1),
        (3, 10, 40),
        (3, 19, 8),
        (3, 20, 1),
        (4, 5, 0),
        (4, 5, 254),
    ];
    for (section, offset, value) in mutations {
        let mut fixture = Fixture::new();
        fixture.sections[section][offset] = value;
        assert!(
            parse_message(&fixture.grib(), stamp()).is_err(),
            "section {section}, byte {offset}"
        );
    }
    let grib = Fixture::new().grib();
    assert!(parse_message(&grib, stamp() + chrono::Duration::seconds(1)).is_err());
    for reference in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY] {
        let mut fixture = Fixture::new();
        put32(&mut fixture.sections[3], 11, reference.to_bits());
        assert!(parse_message(&fixture.grib(), stamp()).is_err());
    }
    for offset in [15, 17] {
        for exponent in [33, 0x8021, 0xffff] {
            let mut fixture = Fixture::new();
            put16(&mut fixture.sections[3], offset, exponent);
            assert!(parse_message(&fixture.grib(), stamp()).is_err());
        }
    }
}

#[test]
fn grid_limits_units_and_geometry_fail_before_raster_allocation() {
    for (offset, value) in [
        (30, 0),
        (34, 0),
        (30, u32::MAX),
        (34, u32::MAX),
        (38, 2),
        (42, 0),
        (46, 0x8000_0000 | 54_995_000),
        (50, 360_000_000),
        (55, 90_000_001),
        (59, 230_000_000),
        (63, 0),
        (67, 0),
    ] {
        let mut fixture = Fixture::new();
        put32(&mut fixture.sections[1], offset, value);
        assert!(
            parse_message(&fixture.grib(), stamp()).is_err(),
            "offset {offset}"
        );
    }
    let (palette, bounds) = palette();
    assert!(
        decode_frame(gzip(&Fixture::new().grib()), stamp(), &palette, &bounds)
            .unwrap_err()
            .contains("CONUS")
    );
    let mut shifted = Fixture::new();
    shifted.dimensions(WIDTH, HEIGHT);
    put32(&mut shifted.sections[1], 50, 230_010_000);
    put32(&mut shifted.sections[1], 59, 300_000_000);
    assert!(
        parse_message(&shifted.grib(), stamp())
            .unwrap()
            .grid
            .validate_conus()
            .is_err()
    );
}

#[test]
fn truncated_reordered_or_multiple_grib_messages_fail_without_panics() {
    let original = Fixture::new().grib();
    for length in 0..original.len() {
        let mut bytes = original[..length].to_vec();
        if length >= 16 {
            bytes[8..16].copy_from_slice(&(length as u64).to_be_bytes());
        }
        assert!(parse_message(&bytes, stamp()).is_err(), "prefix {length}");
    }
    for offset in [0, 4, 6, 7, 15, original.len() - 1] {
        let mut bytes = original.clone();
        bytes[offset] ^= 1;
        assert!(parse_message(&bytes, stamp()).is_err());
    }
    for section_index in 0..6 {
        let mut fixture = Fixture::new();
        fixture.sections[section_index][4] = 2; // includes unsupported local section
        assert!(parse_message(&fixture.grib(), stamp()).is_err());
    }
    let mut reordered = Fixture::new();
    reordered.sections.swap(1, 2);
    assert!(parse_message(&reordered.grib(), stamp()).is_err());
    for length in [0, 4, 20, u32::MAX] {
        let mut bytes = original.clone();
        put32(&mut bytes, 16, length);
        assert!(parse_message(&bytes, stamp()).is_err());
    }
    let mut double = original.repeat(2);
    let len = double.len() as u64;
    double[8..16].copy_from_slice(&len.to_be_bytes());
    assert!(parse_message(&double, stamp()).is_err());
    let mut duplicate = Fixture::new();
    duplicate.sections.push(duplicate.sections[5].clone());
    assert!(parse_message(&duplicate.grib(), stamp()).is_err());
}

#[test]
fn gzip_checks_crc_truncation_expansion_and_single_member_boundary() {
    let body = Fixture::new().grib();
    let valid = gzip(&body);
    assert_eq!(inflate(&valid, BODY_MAX).unwrap(), body);
    for length in 0..valid.len() {
        assert!(
            inflate(&valid[..length], BODY_MAX).is_err(),
            "prefix {length}"
        );
    }
    for offset in [valid.len() - 8, valid.len() - 4] {
        let mut corrupt = valid.clone();
        corrupt[offset] ^= 1;
        assert!(inflate(&corrupt, BODY_MAX).is_err());
    }
    assert!(inflate(&valid.repeat(2), BODY_MAX).is_err());
    let mut tail = valid.clone();
    tail.push(0);
    assert!(inflate(&tail, BODY_MAX).is_err());
    assert!(inflate(&valid, valid.len() - 1).is_err());
    assert!(
        inflate(&gzip(&vec![0; 8192]), 4096)
            .unwrap_err()
            .contains("byte limit")
    );
    let mut output = BoundedBytes::new(100);
    output.write_all(&[0; 99]).unwrap();
    assert!(output.write_all(&[1; 2]).is_err());
    assert_eq!(output.bytes.len(), 99);
    assert!(output.bytes.capacity() <= 100);
    output.write_all(&[1]).unwrap();
    assert_eq!(output.bytes.len(), 100);
}

fn check_png(png: Vec<u8>) -> Result<Vec<u8>, String> {
    let mut fixture = Fixture::new();
    fixture.png(png);
    let grib = fixture.grib();
    let message = parse_message(&grib, stamp()).unwrap();
    let (palette, bounds) = palette();
    classify_png(&message, palette.len(), &bounds)
}

// Test-only CRC to build structurally valid corrupt-zlib/unsupported-chunk cases.
fn crc(bytes: &[u8]) -> u32 {
    let mut crc = u32::MAX;
    for &byte in bytes {
        crc ^= u32::from(byte);
        for _ in 0..8 {
            crc = (crc >> 1) ^ (0xedb8_8320 & 0_u32.wrapping_sub(crc & 1));
        }
    }
    !crc
}

#[test]
fn png_rejects_truncation_crc_corruption_and_header_allocation_bombs() {
    let valid = packed_png(4, 2, &[0, 9000, 9815, 9990, 10090, 10190, 10200, 10695]);
    for length in 0..valid.len() {
        assert!(
            check_png(valid[..length].to_vec()).is_err(),
            "prefix {length}"
        );
    }
    for offset in [0, 29, valid.len() - 1] {
        // signature, IHDR CRC, IEND CRC
        let mut bad = valid.clone();
        bad[offset] ^= 1;
        assert!(check_png(bad).is_err(), "byte {offset}");
    }
    for (offset, value) in [(16, u32::MAX), (20, u32::MAX), (16, 0), (20, 1)] {
        let mut bad = valid.clone();
        put32(&mut bad, offset, value);
        assert!(check_png(bad).is_err());
    }
    for (offset, value) in [
        (24, 8),
        (25, 2),
        (25, 4),
        (25, 6),
        (26, 1),
        (27, 1),
        (28, 1),
    ] {
        let mut bad = valid.clone();
        bad[offset] = value;
        assert!(check_png(bad).is_err());
    }
    let mut tail = valid.clone();
    tail.push(0);
    assert!(check_png(tail).is_err());
    let mut bad_length = valid.clone();
    put32(&mut bad_length, 33, u32::MAX);
    assert!(check_png(bad_length).is_err());
    for kind in [b"acTL", b"iCCP", b"zTXt", b"tEXt", b"IHDR", b"tRNS"] {
        let mut chunk = vec![0; 12];
        chunk[4..8].copy_from_slice(kind);
        let checksum = crc(&chunk[4..8]);
        put32(&mut chunk, 8, checksum);
        let mut bad = valid.clone();
        bad.splice(33..33, chunk);
        assert!(check_png(bad).is_err(), "chunk {kind:?}");
    }
    // Corrupt IDAT CRC, then corrupt only the zlib Adler checksum with a valid
    // PNG CRC: both layers must be checked, including the tail after the last row.
    let len = u32_at(&valid, 33) as usize;
    let checksum_offset = 33 + 8 + len;
    let mut bad_crc = valid.clone();
    bad_crc[checksum_offset] ^= 1;
    assert!(check_png(bad_crc).is_err());
    let mut bad_adler = valid;
    bad_adler[checksum_offset - 1] ^= 1;
    let checksum = crc(&bad_adler[37..checksum_offset]);
    put32(&mut bad_adler, checksum_offset, checksum);
    assert!(check_png(bad_adler).is_err());
}

fn replace_idat(png: &[u8], data: &[u8]) -> Vec<u8> {
    let mut result = png[..33].to_vec();
    result.extend((data.len() as u32).to_be_bytes());
    result.extend(b"IDAT");
    result.extend(data);
    result.extend(crc(&result[37..]).to_be_bytes());
    result.extend(&png[png.len() - 12..]);
    result
}

#[test]
fn png_requires_exact_zlib_end_and_filtered_byte_count() {
    let valid = packed_png(4, 2, &[9000; 8]);
    let len = u32_at(&valid, 33) as usize;
    let data = &valid[41..41 + len];
    for removed in 1..=5 {
        assert!(check_png(replace_idat(&valid, &data[..data.len() - removed])).is_err());
    }
    let mut junk = data.to_vec();
    junk.push(0);
    assert!(check_png(replace_idat(&valid, &junk)).is_err());
    assert!(check_png(replace_idat(&valid, &data.repeat(2))).is_err());
    for size in [17, 19, 100_000] {
        // exactly (4*2+1)*2 = 18 filtered bytes required
        let mut zlib = flate2::write::ZlibEncoder::new(Vec::new(), Compression::fast());
        zlib.write_all(&vec![0; size]).unwrap();
        assert!(check_png(replace_idat(&valid, &zlib.finish().unwrap())).is_err());
    }
    // Arbitrary IDAT boundaries, including splitting the zlib trailer, are legal.
    let mut split = valid[..33].to_vec();
    for byte in data {
        split.extend(1_u32.to_be_bytes());
        let chunk_start = split.len();
        split.extend(b"IDAT");
        split.push(*byte);
        split.extend(crc(&split[chunk_start..]).to_be_bytes());
    }
    split.extend(&valid[valid.len() - 12..]);
    assert_eq!(check_png(split).unwrap(), [0, 1, 0, 255].repeat(8));
}

#[test]
fn full_conus_frame_has_complete_metadata_and_missing_texture() {
    let mut packed = Vec::new();
    let mut encoder = png::Encoder::new(&mut packed, WIDTH, HEIGHT);
    encoder.set_color(png::ColorType::Grayscale);
    encoder.set_depth(png::BitDepth::Sixteen);
    encoder.set_compression(png::Compression::Fast);
    let mut writer = encoder.write_header().unwrap();
    let mut stream = writer.stream_writer().unwrap();
    let row = vec![0; WIDTH as usize * 2];
    for _ in 0..HEIGHT {
        stream.write_all(&row).unwrap();
    }
    stream.finish().unwrap();
    writer.finish().unwrap();
    let mut fixture = Fixture::new();
    fixture.dimensions(WIDTH, HEIGHT);
    fixture.png(packed);
    let (palette, bounds) = palette();
    let (frame, texture, millis) =
        decode_frame(gzip(&fixture.grib()), stamp(), &palette, &bounds).unwrap();
    assert_eq!(frame.id, "mrms-conus-20260926T182139Z");
    assert_eq!(frame.scan_time, "2026-09-26T18:21:39Z");
    assert_eq!(millis, stamp().and_utc().timestamp_millis());
    assert_eq!((frame.width, frame.height), (WIDTH, HEIGHT));
    assert_eq!(frame.geotransform, AFFINE);
    assert_eq!(frame.crs, Crs::wgs84_geographic());
    assert_eq!(frame.product, "REF");
    assert_eq!(frame.product_name, "QC Base Reflectivity");
    assert_eq!(frame.units, "dBZ");
    assert_eq!(frame.status, FrameStatus::Complete);
    assert_eq!(frame.sweep_end, None);
    assert_eq!(frame.palette, palette);
    assert_eq!(frame.bounds, bounds);
    let wire = serde_json::to_value(frame).unwrap();
    for field in ["site", "tilt", "elevation", "rangeKm", "datumTransform"] {
        assert!(wire.get(field).is_none());
    }
    assert!(texture.len() < TEXTURE_MAX);
    let mut reader = png::Decoder::new(Cursor::new(texture)).read_info().unwrap();
    let mut rows = 0;
    while let Some(row) = reader.next_row().unwrap() {
        assert!(
            row.data()
                .as_chunks::<4>()
                .0
                .iter()
                .all(|p| *p == [0, 1, 0, 255])
        );
        rows += 1;
    }
    reader.finish().unwrap();
    assert_eq!(rows, HEIGHT);
}

/// Explicit runtime-only smoke test; ordinary tests never fetch or vendor data.
/// See docs/mrms-decoder.md for commands and resource measurement.
#[test]
#[ignore = "requires an explicitly supplied NOAA object and timestamp"]
fn mrms_runtime_sample() {
    let path = std::env::var("OMASTORM_MRMS_SAMPLE").expect("OMASTORM_MRMS_SAMPLE path");
    let stamp = NaiveDateTime::parse_from_str(
        &std::env::var("OMASTORM_MRMS_STAMP").expect("UTC timestamp"),
        "%Y%m%d-%H%M%S",
    )
    .unwrap();
    let bytes = std::fs::read(path).unwrap();
    let (palette, bounds) = palette();
    let started = std::time::Instant::now();
    let (frame, texture, _) = decode_frame(bytes, stamp, &palette, &bounds).unwrap();
    eprintln!(
        "MRMS {}: decode/classify/encode {:?}, texture {} bytes",
        frame.id,
        started.elapsed(),
        texture.len()
    );
    if let Ok(directory) = std::env::var("OMASTORM_MRMS_OUTPUT") {
        let directory = std::path::Path::new(&directory);
        std::fs::create_dir_all(directory).unwrap();
        std::fs::write(directory.join("texture.png"), &texture).unwrap();
        std::fs::write(
            directory.join("frame.json"),
            serde_json::to_vec_pretty(&frame).unwrap(),
        )
        .unwrap();
        write_preview(&directory.join("radar.png"), &texture, &frame.palette);
    }
}

/// Apply the frame palette for human inspection; the engine texture contains
/// class indices, not display RGB. Missing/no-coverage pixels stay transparent.
fn write_preview(path: &std::path::Path, texture: &[u8], palette: &[String]) {
    let colors: Vec<[u8; 4]> = palette
        .iter()
        .map(|hex| {
            let color = u32::from_str_radix(hex.trim_start_matches('#'), 16).unwrap();
            [(color >> 16) as u8, (color >> 8) as u8, color as u8, 255]
        })
        .collect();
    let mut reader = png::Decoder::new(Cursor::new(texture)).read_info().unwrap();
    let file = std::fs::File::create(path).unwrap();
    let mut encoder = png::Encoder::new(file, WIDTH, HEIGHT);
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    encoder.set_compression(png::Compression::Fast);
    let mut writer = encoder.write_header().unwrap();
    let mut stream = writer.stream_writer().unwrap();
    while let Some(row) = reader.next_row().unwrap() {
        let colored: Vec<u8> = row
            .data()
            .as_chunks::<4>()
            .0
            .iter()
            .flat_map(|p| {
                if p[1] == 1 {
                    [0; 4]
                } else {
                    colors[usize::from(p[0]) - 1]
                }
            })
            .collect();
        stream.write_all(&colored).unwrap();
    }
    reader.finish().unwrap();
    stream.finish().unwrap();
    writer.finish().unwrap();
}
