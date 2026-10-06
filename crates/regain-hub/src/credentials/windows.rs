use std::{
    io,
    ptr::{null, null_mut},
};
use windows_sys::Win32::{
    Foundation::LocalFree,
    Security::Cryptography::{
        CRYPT_INTEGER_BLOB, CRYPTPROTECT_UI_FORBIDDEN, CryptProtectData, CryptUnprotectData,
    },
};
use zeroize::{Zeroize, Zeroizing};

struct Output(CRYPT_INTEGER_BLOB);
impl Drop for Output {
    fn drop(&mut self) {
        if !self.0.pbData.is_null() {
            unsafe { std::slice::from_raw_parts_mut(self.0.pbData, self.0.cbData as usize) }
                .zeroize();
            unsafe {
                LocalFree(self.0.pbData.cast());
            }
        }
    }
}
pub fn protect(bytes: &[u8], entropy: &[u8]) -> io::Result<Zeroizing<Vec<u8>>> {
    convert(bytes, entropy, true)
}
pub fn unprotect(bytes: &[u8], entropy: &[u8]) -> io::Result<Zeroizing<Vec<u8>>> {
    convert(bytes, entropy, false)
}
fn convert(bytes: &[u8], entropy: &[u8], encrypt: bool) -> io::Result<Zeroizing<Vec<u8>>> {
    if bytes.len() > super::MAX_RECORD_BYTES || entropy.len() > 1024 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "Credential data exceeds the size limit",
        ));
    }
    let input = CRYPT_INTEGER_BLOB {
        cbData: bytes.len() as u32,
        pbData: bytes.as_ptr().cast_mut(),
    };
    let entropy = CRYPT_INTEGER_BLOB {
        cbData: entropy.len() as u32,
        pbData: entropy.as_ptr().cast_mut(),
    };
    let mut output = Output(CRYPT_INTEGER_BLOB {
        cbData: 0,
        pbData: null_mut(),
    });
    let success = unsafe {
        if encrypt {
            CryptProtectData(
                &input,
                null(),
                &entropy,
                null(),
                null(),
                CRYPTPROTECT_UI_FORBIDDEN,
                &mut output.0,
            )
        } else {
            CryptUnprotectData(
                &input,
                null_mut(),
                &entropy,
                null(),
                null(),
                CRYPTPROTECT_UI_FORBIDDEN,
                &mut output.0,
            )
        }
    };
    if success == 0 {
        return Err(io::Error::last_os_error());
    }
    if output.0.pbData.is_null() || output.0.cbData as usize > super::MAX_RECORD_BYTES {
        return Err(io::Error::other("Invalid protected credential result"));
    }
    Ok(Zeroizing::new(
        unsafe { std::slice::from_raw_parts(output.0.pbData, output.0.cbData as usize) }.to_vec(),
    ))
}
