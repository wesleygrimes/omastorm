//! Bound property and geometry expansion before mvt-reader decodes features.

use std::io;

const MAX_PROPERTIES: usize = 16 << 20;
// Allow for scalar values and hash table storage as well as string contents.
const PROPERTY_OVERHEAD: usize = 256;
// Across 72 dense OpenFreeMap z7-z14 tiles, peaks were 10,796 parts, 47,373
// coordinates and 218,098,502 estimated live coordinate slots. A counting
// allocator measured 1,746,677,821 peak live requested bytes with mvt-reader
// 2.5.0, including reader/properties. Allow 4 GiB of f32 coordinate reservations
// per layer (over 2.4x headroom). These mostly untouched reservations are address
// space, not RSS; parts and coordinates separately bound the populated data.
const MAX_PARTS: usize = 16_384;
const MAX_COORDINATES: usize = 262_144;
const MAX_RESERVED: usize = 512 << 20;

struct GeometryBudget {
    parts: usize,
    coordinates: usize,
    reserved: usize,
}

pub(super) fn check(mut tile: &[u8]) -> io::Result<()> {
    if tile.len() > super::MAX_BODY {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "vector tile body over the size limit",
        ));
    }
    let mut remaining = MAX_PROPERTIES;
    let mut geometry = GeometryBudget {
        parts: MAX_PARTS,
        coordinates: MAX_COORDINATES,
        reserved: MAX_RESERVED,
    };
    while !tile.is_empty() {
        if let (3, 2, layer) = field(&mut tile)? {
            // render() drops the previous layer's features before decoding this one.
            geometry.reserved = MAX_RESERVED;
            check_layer(layer, &mut remaining, &mut geometry)?;
        }
    }
    Ok(())
}

fn check_layer(
    layer: &[u8],
    remaining: &mut usize,
    geometry: &mut GeometryBudget,
) -> io::Result<()> {
    let mut input = layer;
    let mut name = &[][..];
    let mut keys = Vec::new();
    let mut values = Vec::new();
    while !input.is_empty() {
        match field(&mut input)? {
            (1, 2, value) => name = value,
            (3, 2, key) => keys.push(key.len()),
            // The encoded Value bounds its string length, including values
            // with several scalar fields that mvt-reader clones temporarily.
            (4, 2, value) => values.push(value.len()),
            _ => {}
        }
    }
    if !super::LAYERS.iter().any(|layer| layer.as_bytes() == name) {
        return Ok(());
    }
    input = layer;
    while !input.is_empty() {
        if let (2, 2, mut feature) = field(&mut input)? {
            let original = feature;
            let mut kind = None;
            let mut key = None;
            let mut tag = |index: u64| -> io::Result<()> {
                // prost decodes uint32 tags by truncating the wire varint.
                let index = index as u32 as usize;
                if let Some(key_size) = key.take() {
                    let value_size = values.get(index).ok_or_else(invalid)?;
                    let size = key_size + value_size + PROPERTY_OVERHEAD;
                    *remaining = remaining.checked_sub(size).ok_or_else(|| {
                        io::Error::new(
                            io::ErrorKind::InvalidData,
                            "vector tile properties over the size limit",
                        )
                    })?;
                } else {
                    key = Some(*keys.get(index).ok_or_else(invalid)?);
                }
                Ok(())
            };
            while !feature.is_empty() {
                match field(&mut feature)? {
                    (2, 0, mut value) => tag(varint(&mut value)?)?,
                    (2, 2, mut packed) => {
                        while !packed.is_empty() {
                            tag(varint(&mut packed)?)?;
                        }
                    }
                    (3, 0, mut value) => kind = Some(varint(&mut value)? as u32),
                    _ => {}
                }
            }
            if key.is_some() {
                return Err(invalid());
            }
            if let Some(kind) = kind {
                check_geometry(original, kind, geometry)?;
            }
        }
    }
    Ok(())
}

fn check_geometry(mut feature: &[u8], kind: u32, budget: &mut GeometryBudget) -> io::Result<()> {
    if !(1..=3).contains(&kind) {
        return Err(invalid_geometry());
    }
    let mut words = 0usize;
    let mut parts = 0usize;
    let mut points = 0usize;
    let mut parameters = 0usize;
    let coordinates_before = budget.coordinates;
    let mut word = |value: u64| -> io::Result<()> {
        words += 1;
        if parameters != 0 {
            parameters -= 1;
            return Ok(());
        }
        // Like prost's repeated uint32, truncate the wire varint first.
        let value = value as u32;
        let count = (value >> 3) as usize;
        if count == 0 {
            return Err(invalid_geometry());
        }
        match value & 7 {
            1 => {
                let valid = match kind {
                    1 => parts == 0,
                    2 => count == 1 && (points == 0 || points >= 2),
                    3 => count == 1 && points == 0,
                    _ => false,
                };
                if !valid {
                    return Err(invalid_geometry());
                }
                budget.parts = budget.parts.checked_sub(1).ok_or_else(geometry_limit)?;
                parts += 1;
                points = 0;
            }
            2 if kind != 1 && points == 1 => {}
            7 if kind == 3 && count == 1 && points >= 3 => {
                budget.coordinates = budget
                    .coordinates
                    .checked_sub(1)
                    .ok_or_else(geometry_limit)?;
                points = 0;
                return Ok(());
            }
            _ => return Err(invalid_geometry()),
        }
        budget.coordinates = budget
            .coordinates
            .checked_sub(count)
            .ok_or_else(geometry_limit)?;
        points += count;
        parameters = count * 2;
        Ok(())
    };
    // Repeated geometry words concatenate across packed and unpacked fields.
    while !feature.is_empty() {
        match field(&mut feature)? {
            (4, 0, mut value) => word(varint(&mut value)?)?,
            (4, 2, mut packed) => {
                while !packed.is_empty() {
                    word(varint(&mut packed)?)?;
                }
            }
            _ => {}
        }
    }
    if parameters != 0 || parts == 0 || (kind == 2 && points < 2) || (kind == 3 && points != 0) {
        return Err(invalid_geometry());
    }
    let coordinates = coordinates_before - budget.coordinates;
    // Lines keep every full-length buffer. Points keep only their coordinates.
    // Polygons keep length-sized clones; reserved rings and the spare buffer
    // die before the next feature. Conservatively count all rings in a feature
    // together, plus a temporary ring clone, without interpreting winding.
    let (retained, transient) = match kind {
        1 => (coordinates, words),
        2 => (parts.checked_mul(words).ok_or_else(geometry_limit)?, 0),
        _ => (
            coordinates,
            (parts + 1)
                .checked_mul(words)
                .and_then(|reserved| reserved.checked_add(coordinates))
                .ok_or_else(geometry_limit)?,
        ),
    };
    if retained.checked_add(transient).ok_or_else(geometry_limit)? > budget.reserved {
        return Err(geometry_limit());
    }
    budget.reserved = budget
        .reserved
        .checked_sub(retained)
        .ok_or_else(geometry_limit)?;
    Ok(())
}

fn invalid_geometry() -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, "invalid vector tile geometry")
}

fn geometry_limit() -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidData,
        "vector tile geometry over the size limit",
    )
}

fn invalid() -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, "invalid vector tile properties")
}

// Read protobuf fields without allocating their contents. Only layers, key
// and value lengths, feature tag indices and geometry words are needed.
fn field<'a>(input: &mut &'a [u8]) -> io::Result<(u32, u8, &'a [u8])> {
    let tag = u32::try_from(varint(input)?).map_err(|_| invalid())?;
    if tag >> 3 == 0 {
        return Err(invalid());
    }
    let wire = (tag & 7) as u8;
    let data = match wire {
        0 => {
            let start = *input;
            varint(input)?;
            &start[..start.len() - input.len()]
        }
        1 => take(input, 8)?,
        2 => {
            let size = usize::try_from(varint(input)?).map_err(|_| invalid())?;
            take(input, size)?
        }
        5 => take(input, 4)?,
        _ => return Err(invalid()),
    };
    Ok((tag >> 3, wire, data))
}

fn take<'a>(input: &mut &'a [u8], size: usize) -> io::Result<&'a [u8]> {
    let (head, tail) = input.split_at_checked(size).ok_or_else(invalid)?;
    *input = tail;
    Ok(head)
}

fn varint(input: &mut &[u8]) -> io::Result<u64> {
    let mut value = 0;
    for shift in (0..=63).step_by(7) {
        let byte = take(input, 1)?[0];
        if shift == 63 && byte > 1 {
            return Err(invalid());
        }
        value |= u64::from(byte & 127) << shift;
        if byte & 128 == 0 {
            return Ok(value);
        }
    }
    Err(invalid())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_malformed_wire_fields() {
        for bytes in [
            &[0][..],                             // Zero field number.
            &[128][..],                           // Truncated varint.
            &[255; 10][..],                       // Overflowing varint.
            &[26, 5, 0][..],                      // Truncated length-delimited field.
            &[9, 0][..],                          // Truncated fixed64 field.
            &[13, 0][..],                         // Truncated fixed32 field.
            &[26, 255, 255, 255, 255, 15][..],    // Length exceeds the input.
            &[255, 255, 255, 255, 255, 0, 0][..], // Field tag exceeds uint32.
        ] {
            assert_eq!(check(bytes).unwrap_err().kind(), io::ErrorKind::InvalidData);
        }
    }
}
