//! crypto.rs —— 米家通信所需的加密原语
//!
//! 移植自 Node 版 `node-mihome` 的用法，涉及三类：
//!
//! 1. **miIO 局域网**：AES-128-CBC。密钥 = MD5(token)，IV 在握手阶段是
//!    MD5(key ‖ token)，握手完成后换成 MD5(key ‖ 设备回传的 token)。
//! 2. **小米云签名**：ssecurity + nonce → SHA256 → base64，用于 `_s` 签名头。
//! 3. **云端 token 解密**：服务端用 RC4 加密 serviceToken（`_security` 流程）。
//!
//! 全部用 RustCrypto 的纯 Rust 实现，不依赖 OpenSSL，交叉编译和分发都省事。

use aes::cipher::{block_padding::Pkcs7, BlockDecryptMut, BlockEncryptMut, KeyIvInit};
use md5::{Digest, Md5};

type Aes128CbcEnc = cbc::Encryptor<aes::Aes128>;
type Aes128CbcDec = cbc::Decryptor<aes::Aes128>;

/// MD5 摘要（miIO 的密钥派生到处都用它）。
pub fn md5(data: &[u8]) -> [u8; 16] {
    let mut h = Md5::new();
    h.update(data);
    let out = h.finalize();
    let mut r = [0u8; 16];
    r.copy_from_slice(&out);
    r
}

/// AES-128-CBC 加密 + PKCS7 填充。
pub fn aes_cbc_encrypt(key: &[u8; 16], iv: &[u8; 16], plain: &[u8]) -> Vec<u8> {
    Aes128CbcEnc::new(key.into(), iv.into()).encrypt_padded_vec_mut::<Pkcs7>(plain)
}

/// AES-128-CBC 解密 + 去 PKCS7 填充。
///
/// 设备回的包可能不带合法填充（个别固件如此），此时退化为「原样返回解密结果」，
/// 避免因为填充问题把整个响应丢掉。
pub fn aes_cbc_decrypt(key: &[u8; 16], iv: &[u8; 16], cipher: &[u8]) -> Vec<u8> {
    if cipher.is_empty() || cipher.len() % 16 != 0 {
        return Vec::new();
    }
    match Aes128CbcDec::new(key.into(), iv.into()).decrypt_padded_vec_mut::<Pkcs7>(cipher) {
        Ok(v) => v,
        Err(_) => {
            // 填充不合法：直接按块解密，不做去填充
            let mut buf = cipher.to_vec();
            let mut dec = Aes128CbcDec::new(key.into(), iv.into());
            for chunk in buf.chunks_mut(16) {
                let block: &mut [u8; 16] = chunk.try_into().unwrap();
                dec.decrypt_block_mut(block.into());
            }
            buf
        }
    }
}

/// 小米云签名：`_s` 头。
///
/// 算法（与 node-mihome / python-miio 一致）：
///   sig = base64( SHA256( ssecurity_bytes ‖ nonce_bytes ) )
/// 其中 nonce 是 `_nonce` 头的 base64 解码结果。
pub fn cloud_signature(ssecurity: &[u8], nonce: &[u8]) -> String {
    use base64::engine::general_purpose::STANDARD;
    use base64::Engine;
    use sha2::Sha256;

    let mut h = Sha256::new();
    h.update(ssecurity);
    h.update(nonce);
    let sig = h.finalize();
    STANDARD.encode(sig)
}

/// RC4 流加密（云端 `_security` 流程解密 serviceToken 用）。
///
/// KSA + PRGA，标准实现。
pub fn rc4(key: &[u8], data: &[u8]) -> Vec<u8> {
    let mut s: [u8; 256] = [0; 256];
    for (i, v) in s.iter_mut().enumerate() {
        *v = i as u8;
    }
    let mut j: u8 = 0;
    for i in 0..256 {
        j = j
            .wrapping_add(s[i])
            .wrapping_add(key[i % key.len().max(1)]);
        s.swap(i, j as usize);
    }

    let mut out = Vec::with_capacity(data.len());
    let (mut i, mut j) = (0u8, 0u8);
    for &b in data {
        i = i.wrapping_add(1);
        j = j.wrapping_add(s[i as usize]);
        s.swap(i as usize, j as usize);
        let k = s[(s[i as usize].wrapping_add(s[j as usize])) as usize];
        out.push(b ^ k);
    }
    out
}

/// 生成一个指定字节长度的随机数（用系统熵源）。
pub fn random_bytes(n: usize) -> Vec<u8> {
    use rand::RngCore;
    let mut buf = vec![0u8; n];
    rand::rngs::OsRng.fill_bytes(&mut buf);
    buf
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::engine::general_purpose::STANDARD;
    use base64::Engine;

    #[test]
    fn md5_known_vector() {
        // MD5("") = d41d8cd98f00b204e9800998ecf8427e
        assert_eq!(
            md5(b""),
            [
                0xd4, 0x1d, 0x8c, 0xd9, 0x8f, 0x00, 0xb2, 0x04, 0xe9, 0x80, 0x09, 0x98,
                0xec, 0xf8, 0x42, 0x7e
            ]
        );
        // MD5("abc") = 900150983cd24fb0d6963f7d28e17f72
        assert_eq!(md5(b"abc")[0], 0x90);
    }

    #[test]
    fn aes_roundtrip() {
        let key = md5(b"0123456789abcdef0123456789abcdef");
        let iv = md5(b"hello");
        let plain = b"\x21\x31\x00\x20this is a miio payload with \xe4\xb8\xad\xe6\x96\x87";
        let ct = aes_cbc_encrypt(&key, &iv, plain);
        assert_eq!(ct.len() % 16, 0);
        let back = aes_cbc_decrypt(&key, &iv, &ct);
        assert_eq!(&back[..], &plain[..]);
    }

    #[test]
    fn rc4_known_vector() {
        // RC4("Key", "Plaintext") = BBF316E8D940AF0AD3
        let out = rc4(b"Key", b"Plaintext");
        assert_eq!(
            out,
            vec![0xBB, 0xF3, 0x16, 0xE8, 0xD9, 0x40, 0xAF, 0x0A, 0xD3]
        );
    }

    #[test]
    fn signature_is_base64_sha256() {
        // 用固定输入验证签名长度稳定为 44 字节 base64（32 字节 SHA256）
        let sig = cloud_signature(b"0123456789abcdef", b"nonce");
        assert_eq!(sig.len(), 44);
        let raw = STANDARD.decode(sig).unwrap();
        assert_eq!(raw.len(), 32);
    }
}
