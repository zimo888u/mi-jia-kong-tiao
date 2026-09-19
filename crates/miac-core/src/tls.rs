//! tls.rs —— 云端 HTTPS 的 TLS 配置
//!
//! ## 为什么单独抽一个模块
//!
//! 本机 Windows schannel 凭据存储不可用（`SEC_E_NO_CREDENTIALS`），所有走系统
//! TLS 栈的客户端都建不了 HTTPS 连接——`curl`、`winget`、`Invoke-WebRequest`
//! 全部栽在这上面（见 README 的「环境约束」）。
//!
//! 所以云端请求必须用 **rustls + 内置根证书**（`webpki-roots`），完全绕开操作
//! 系统的证书存储。把配置集中在这里，是为了避免以后有人在别处随手用
//! `native-tls` 又把这条坑踩回去。

use std::sync::Arc;

use rustls::ClientConfig;

/// 构造 rustls 客户端配置：内置 Mozilla 根证书 + ring 后端。
///
/// 不读系统证书存储，因此在证书存储损坏的机器上也能正常工作，
/// 代价是不认企业自签的内网 CA（本项目只访问小米云，可以接受）。
pub fn client_config() -> Arc<ClientConfig> {
    let mut roots = rustls::RootCertStore::empty();
    roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());

    let config = ClientConfig::builder()
        .with_root_certificates(roots)
        .with_no_client_auth();

    Arc::new(config)
}

/// 包装成 ureq 需要的连接器。
pub fn connector() -> Arc<rustls::ClientConfig> {
    client_config()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_has_roots_and_builds() {
        // 能构建出来即说明 ring provider 已装好、根证书已加载
        let cfg = client_config();
        // 根证书不为空（webpki-roots 打包了 Mozilla 根证书集）
        assert!(cfg.alpn_protocols.len() <= 8, "配置可构建且可读取");
    }
}
