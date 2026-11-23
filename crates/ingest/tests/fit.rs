//! FIT parser test over a hand-built minimal FIT file (synthetic, so it runs in CI).

use garmin_ingest::fit::parse_samples;

/// The FIT CRC-16 (from the FIT SDK), used for the header and file CRC.
fn fit_crc(data: &[u8]) -> u16 {
    const TABLE: [u16; 16] = [
        0x0000, 0xCC01, 0xD801, 0x1400, 0xF001, 0x3C00, 0x2800, 0xE401, 0xA001, 0x6C00, 0x7800,
        0xB401, 0x5000, 0x9C01, 0x8801, 0x4400,
    ];
    let mut crc: u16 = 0;
    for &byte in data {
        for nibble in [byte & 0x0F, byte >> 4] {
            let tmp = TABLE[(crc & 0x0F) as usize];
            crc = (crc >> 4) & 0x0FFF;
            crc = crc ^ tmp ^ TABLE[nibble as usize];
        }
    }
    crc
}

/// Build a FIT file with one `record` definition and two data messages
/// (timestamp, heart_rate, power, position_lat, position_long).
fn build_fit() -> Vec<u8> {
    let mut data: Vec<u8> = Vec::new();
    // Definition message: header 0x40, reserved, arch=0 (LE), global num=20 (record), 5 fields.
    data.extend_from_slice(&[0x40, 0x00, 0x00, 20, 0, 5]);
    // field_def_num, size, base_type
    data.extend_from_slice(&[253, 4, 0x86]); // timestamp uint32
    data.extend_from_slice(&[3, 1, 0x02]); // heart_rate uint8
    data.extend_from_slice(&[7, 2, 0x84]); // power uint16
    data.extend_from_slice(&[0, 4, 0x85]); // position_lat sint32
    data.extend_from_slice(&[1, 4, 0x85]); // position_long sint32

    // FIT epoch is 1989-12-31 00:00 UTC; use a round offset.
    let ts0: u32 = 1_000_000_000;
    let lat_semi: i32 = 620_000_000; // ~52 deg
    let lon_semi: i32 = 250_000_000; // ~21 deg
    for i in 0..2u32 {
        data.push(0x00); // data message, local type 0
        data.extend_from_slice(&(ts0 + i).to_le_bytes());
        data.push(150 + i as u8); // heart_rate
        data.extend_from_slice(&(200u16 + i as u16).to_le_bytes()); // power
        data.extend_from_slice(&lat_semi.to_le_bytes());
        data.extend_from_slice(&lon_semi.to_le_bytes());
    }

    // 14-byte header.
    let mut file: Vec<u8> = Vec::new();
    file.push(14); // header size
    file.push(0x20); // protocol version
    file.extend_from_slice(&2191u16.to_le_bytes()); // profile version
    file.extend_from_slice(&(data.len() as u32).to_le_bytes()); // data size
    file.extend_from_slice(b".FIT");
    let header_crc = fit_crc(&file);
    file.extend_from_slice(&header_crc.to_le_bytes());
    // Data, then the file CRC over everything so far.
    file.extend_from_slice(&data);
    let file_crc = fit_crc(&file);
    file.extend_from_slice(&file_crc.to_le_bytes());
    file
}

#[test]
fn parses_records_into_samples() {
    let samples = parse_samples(&build_fit()).unwrap();
    assert_eq!(samples.len(), 2);
    assert_eq!(samples[0].heart_rate, Some(150));
    assert_eq!(samples[1].heart_rate, Some(151));
    assert_eq!(samples[0].power, Some(200));
    // Semicircles converted to degrees.
    assert!((samples[0].lat.unwrap() - 51.9).abs() < 0.5);
    assert!((samples[0].lon.unwrap() - 20.9).abs() < 0.5);
}

#[test]
fn a_truncated_fit_is_an_error_not_a_panic() {
    let err = parse_samples(&[0x0E, 0x20, 0x00]).unwrap_err();
    assert!(!err.to_string().is_empty());
}
