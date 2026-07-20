#[repr(u32)]
#[derive(Clone, Copy, Debug)]
pub enum HexCasing {
    // Output [ '0' .. '9' ] and [ 'A' .. 'F' ].
    Upper = 0,

    // Output [ '0' .. '9' ] and [ 'a' .. 'f' ].
    // This works because values in the range [ 0x30 .. 0x39 ] ([ '0' .. '9' ])
    // already have the 0x20 bit set, so ORing them with 0x20 is a no-op,
    // while outputs in the range [ 0x41 .. 0x46 ] ([ 'A' .. 'F' ])
    // don't have the 0x20 bit set, so ORing them maps to
    // [ 0x61 .. 0x66 ] ([ 'a' .. 'f' ]), which is what we want.
    Lower = 0x2020,
}

pub const fn byte_to_hex(value: u8, casing: HexCasing) -> [u8; 2] {
    let diff = ((value & 0xF0) << 4) as u32 + (value & 0x0F) as u32 - 0x8989;
    let packed = (((diff.wrapping_neg() & 0x7070) >> 4) + diff + 0xB9B9) | (casing as u32);

    [(packed >> 8) as u8, (packed & 0xFF) as u8]
}

pub fn bytes_to_hex(
    bytes: impl Iterator<Item = u8>,
    casing: HexCasing,
) -> impl Iterator<Item = u8> {
    bytes.flat_map(move |b| byte_to_hex(b, casing))
}
