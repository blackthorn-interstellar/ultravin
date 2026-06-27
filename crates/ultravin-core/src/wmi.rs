//! Ports of `vpic.fVinWMI` and `vpic.fVinDescriptor` (data-free).
//! Canonical: `vpic/procs/fvinwmi.sql`, `vpic/procs/fvindescriptor.sql`.

/// `fVinWMI`: positions 1-3, extended with positions 12-14 when position 3 is
/// `'9'` (low-volume manufacturer) and the VIN is long enough.
pub fn vin_wmi(vin: &str) -> String {
    let b = vin.as_bytes();
    let mut wmi: Vec<u8> = if b.len() > 3 {
        b[..3].to_vec()
    } else {
        b.to_vec()
    };
    // substring(wmi, 3, 1) = '9' (1-based pos 3) and length(vin) >= 14.
    if wmi.get(2) == Some(&b'9') && b.len() >= 14 {
        wmi.extend_from_slice(&b[11..14]); // substring(vin, 12, 3)
    }
    String::from_utf8_lossy(&wmi).into_owned()
}

/// `fVinDescriptor`: pad to 17 with `*`, mask position 9, then take the first 11
/// chars (14 for low-volume VINs where position 3 is `'9'`). Upper-cased.
pub fn vin_descriptor(vin: &str) -> String {
    let mut p: Vec<u8> = vin.trim().bytes().collect();
    p.resize(17, b'*');
    p.truncate(17);
    p[8] = b'*'; // position 9 (0-based 8)
    let take = if p.get(2) == Some(&b'9') { 14 } else { 11 };
    String::from_utf8_lossy(&p[..take]).to_ascii_uppercase()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn standard_wmi_is_first_three() {
        assert_eq!(vin_wmi("1HGCM82633A004352"), "1HG");
    }

    #[test]
    fn low_volume_wmi_includes_12_14() {
        // position 3 = '9' -> WMI = pos1-3 + pos12-14.
        assert_eq!(vin_wmi("1F9TC25FTAB123456"), "1F9123");
    }

    #[test]
    fn descriptor_masks_position_9_and_takes_11() {
        let d = vin_descriptor("1HGCM82633A004352");
        assert_eq!(d.len(), 11);
        assert_eq!(&d[..8], "1HGCM826");
        assert_eq!(&d[8..9], "*"); // position 9 masked
    }
}
