//! Bound dictionary expansion before mvt-reader clones properties per feature.

use std::io;

const MAX_PROPERTIES: usize = 16 << 20;
// Allow for scalar values and hash table storage as well as string contents.
const PROPERTY_OVERHEAD: usize = 256;

pub(super) fn check(mut tile: &[u8]) -> io::Result<()> {
    if tile.len() > super::MAX_BODY {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "vector tile body over the size limit",
        ));
    }
    let mut remaining = MAX_PROPERTIES;
    while !tile.is_empty() {
        if let (3, 2, layer) = field(&mut tile)? {
            check_layer(layer, &mut remaining)?;
        }
    }
    Ok(())
}

fn check_layer(layer: &[u8], remaining: &mut usize) -> io::Result<()> {
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
                    _ => {}
                }
            }
            if key.is_some() {
                return Err(invalid());
            }
        }
    }
    Ok(())
}

fn invalid() -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, "invalid vector tile properties")
}

// Read protobuf fields without allocating their contents. Only layers, key
// and value lengths, and feature tag indices are needed for the budget.
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
