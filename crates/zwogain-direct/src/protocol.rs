//! Observed Cypress packed SINGLE_TRANSFER ABI. No native struct alignment.
use anyhow::{Result, ensure};
use serde_json::{Value, json};

pub const HEADER: usize = 38;
pub const VERSION: u32 = 0x220000;
pub const CONTROL: u32 = 0x220020;
pub const BULK: u32 = 0x22004b;

fn u16_at(bytes: &[u8], at: usize) -> u16 {
    u16::from_le_bytes(bytes[at..at + 2].try_into().unwrap())
}
fn u32_at(bytes: &[u8], at: usize) -> u32 {
    u32::from_le_bytes(bytes[at..at + 4].try_into().unwrap())
}

pub fn descriptor_request(kind: u8, length: u16) -> Vec<u8> {
    let mut data = vec![0; HEADER + length as usize];
    data[0] = 0x80; // Standard device-to-host GET_DESCRIPTOR only.
    data[1] = 6;
    data[3] = kind;
    data[6..8].copy_from_slice(&length.to_le_bytes());
    data[8..12].copy_from_slice(&5_u32.to_le_bytes());
    data[30..34].copy_from_slice(&(HEADER as u32).to_le_bytes());
    data[34..38].copy_from_slice(&(length as u32).to_le_bytes());
    data
}

pub fn status(data: &[u8]) -> Result<(u32, u32)> {
    ensure!(data.len() >= HEADER, "truncated transfer header");
    Ok((u32_at(data, 14), u32_at(data, 18)))
}

pub fn descriptor_payload(data: &[u8], count: usize, expected: usize) -> Result<&[u8]> {
    let (nt, usb) = status(data)?;
    ensure!(
        nt == 0 && usb == 0,
        "driver status NT={nt:08x} USBD={usb:08x}"
    );
    ensure!(
        count <= data.len() && count == HEADER + expected,
        "invalid returned size"
    );
    ensure!(
        u32_at(data, 30) as usize == HEADER && u32_at(data, 34) as usize == expected,
        "invalid descriptor offset/length"
    );
    Ok(&data[HEADER..count])
}

pub fn config_length(prefix: &[u8]) -> Result<usize> {
    ensure!(
        prefix.len() >= 9 && prefix[0] == 9 && prefix[1] == 2,
        "invalid configuration header"
    );
    let length = u16_at(prefix, 2) as usize;
    ensure!((9..=4096).contains(&length), "invalid configuration size");
    Ok(length)
}

pub fn describe(device: &[u8], config: &[u8], version: u32) -> Result<Value> {
    ensure!(
        device.len() == 18 && device[0] == 18 && device[1] == 1,
        "invalid device descriptor"
    );
    ensure!(u16_at(device, 8) == 0x03c3, "interface is not a ZWO camera");
    ensure!(
        config_length(config)? == config.len(),
        "truncated configuration"
    );
    let mut endpoints = Vec::<Value>::new();
    let mut at = 0;
    let mut interface = None;
    let mut previous_endpoint = false;
    while at < config.len() {
        ensure!(config.len() - at >= 2, "truncated descriptor");
        let length = config[at] as usize;
        ensure!(
            length >= 2 && length <= config.len() - at,
            "invalid descriptor chain"
        );
        let data = &config[at..at + length];
        match data[1] {
            4 => {
                ensure!(length >= 9, "truncated interface");
                interface = Some((data[2], data[3]));
            }
            5 => {
                ensure!(length >= 7 && interface.is_some(), "invalid endpoint");
                let (number, alternate) = interface.unwrap();
                endpoints.push(json!({"interface":number,"alternate":alternate,
                    "address":data[2],"attributes":data[3],"maxPacketSize":u16_at(data,4)}));
            }
            48 => {
                ensure!(
                    length >= 6 && previous_endpoint,
                    "orphan or truncated companion"
                );
                endpoints.last_mut().unwrap()["maxBurst"] = json!(data[2]);
            }
            _ => {}
        }
        previous_endpoint = data[1] == 5;
        at += length;
    }
    Ok(
        json!({"sdkLoaded":false,"driverVersionRaw":format!("0x{version:08x}"),
        "vendorId":u16_at(device,8),"productId":u16_at(device,10),
        "usbVersionBcd":u16_at(device,2),"configurationBytes":config.len(),"endpoints":endpoints}),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn descriptor_abi_and_invalid_completions() {
        let request = descriptor_request(1, 18);
        assert_eq!(&request[..8], &[0x80, 6, 0, 1, 0, 0, 18, 0]);
        assert_eq!(descriptor_payload(&request, 56, 18).unwrap().len(), 18);
        assert!(descriptor_payload(&request, 55, 18).is_err());
        let mut bad = request.clone();
        bad[14] = 1;
        assert!(descriptor_payload(&bad, 56, 18).is_err());
        bad = request;
        bad[30] = 255;
        assert!(descriptor_payload(&bad, 56, 18).is_err());
        assert!(status(&[0; 37]).is_err());
    }
    #[test]
    fn malformed_descriptor_chains_are_rejected() {
        let mut device = vec![0; 18];
        device[0] = 18;
        device[1] = 1;
        device[8..10].copy_from_slice(&0x03c3_u16.to_le_bytes());
        let config = [
            9, 2, 31, 0, 1, 1, 0, 0x80, 0, 9, 4, 0, 0, 1, 0xff, 0, 0, 0, 7, 5, 0x81, 2, 0, 4, 0, 6,
            48, 15, 0, 0, 0,
        ];
        let result = describe(&device, &config, 0x01020200).unwrap();
        assert_eq!(result["endpoints"][0]["maxPacketSize"], 1024);
        assert_eq!(result["endpoints"][0]["maxBurst"], 15);
        for length in 0..config.len() {
            assert!(describe(&device, &config[..length], 0).is_err());
        }
        let mut bad = config;
        bad[18] = 0;
        assert!(describe(&device, &bad, 0).is_err());
        bad = config;
        bad[19] = 48;
        assert!(describe(&device, &bad, 0).is_err());
    }
}
