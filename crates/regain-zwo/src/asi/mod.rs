//! ZWO ASI camera backends. Each runs in an isolated regain-device process.
#[cfg(feature = "asi-direct")]
pub mod direct;
#[cfg(feature = "asi-sdk")]
pub mod sdk;

/// Empty user input is an unspecified identity, never an empty serial filter.
pub(crate) fn normalized_serial(serial: Option<&str>) -> Option<String> {
    serial
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_ascii_lowercase)
}

#[cfg(test)]
mod tests {
    #[test]
    fn serial_input_is_optional_and_case_insensitive() {
        for input in [None, Some(""), Some("  ")] {
            assert_eq!(super::normalized_serial(input), None);
        }
        assert_eq!(
            super::normalized_serial(Some(" ABCDef0123456789 ")).as_deref(),
            Some("abcdef0123456789")
        );
    }
}
