//! Upload a capture to a host that needs no account or key, and give back the link.
//! WinHTTP (part of Windows) does the request, so nothing extra is bundled.
//! Nothing is sent until the user asks for it with the Upload button.

use std::ffi::c_void;
use windows::core::{w, PCWSTR};
use windows::Win32::Networking::WinHttp::*;
use windows::Win32::UI::WindowsAndMessaging::WM_APP;

/// Worker -> session: Box<(Host, Result<link, reason>)> in lParam.
pub const WM_APP_UPLOADED: u32 = WM_APP + 42;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Host {
    /// uguu.se (Pomf AB, Sweden): 3 hours, the IP is forgotten when the file goes.
    Uguu,
    /// litterbox.catbox.moe: kept for 24 hours.
    Litter24,
    /// litterbox.catbox.moe: kept for 72 hours.
    Litter72,
    /// `[upload] url` in the ini: your own server.
    Custom,
}

impl Host {
    /// The ones offered in the settings window (Custom is set in the ini).
    pub const SHOWN: [Host; 3] = [Host::Uguu, Host::Litter24, Host::Litter72];

    pub fn ini(self) -> &'static str {
        match self {
            Host::Uguu => "uguu",
            Host::Litter24 => "litterbox_24h",
            Host::Litter72 => "litterbox_72h",
            Host::Custom => "custom",
        }
    }

    pub fn parse(s: &str) -> Host {
        match s.trim().to_ascii_lowercase().as_str() {
            "litterbox" | "litterbox_24h" => Host::Litter24,
            "litterbox_72h" => Host::Litter72,
            "custom" => Host::Custom,
            _ => Host::Uguu,
        }
    }

    /// Hours the host keeps the file (0: unknown).
    pub fn hours(self) -> u32 {
        match self {
            Host::Uguu => 3,
            Host::Litter24 => 24,
            Host::Litter72 => 72,
            Host::Custom => 0,
        }
    }

    /// Shown to the user before the first upload and in the notice.
    pub fn site(self, custom_url: &str) -> String {
        match self {
            Host::Uguu => "uguu.se".into(),
            Host::Litter24 | Host::Litter72 => "litterbox.catbox.moe".into(),
            Host::Custom => split_url(custom_url).map(|u| u.host).unwrap_or_else(|| custom_url.to_string()),
        }
    }
}

struct Url {
    host: String,
    port: u16,
    path: String,
    secure: bool,
}

fn split_url(u: &str) -> Option<Url> {
    let (secure, rest) =
        if let Some(r) = u.strip_prefix("https://") { (true, r) } else { (false, u.strip_prefix("http://")?) };
    let (hostport, path) = match rest.find('/') {
        Some(i) => (&rest[..i], &rest[i..]),
        None => (rest, "/"),
    };
    let (host, port) = match hostport.rsplit_once(':') {
        Some((h, p)) => (h, p.parse().ok()?),
        None => (hostport, if secure { 443 } else { 80 }),
    };
    if host.is_empty() {
        return None;
    }
    Some(Url { host: host.to_string(), port, path: path.to_string(), secure })
}

/// Uploads a PNG; the link on success, a short reason otherwise.
pub fn upload(host: Host, custom_url: &str, custom_field: &str, png: &[u8], name: &str) -> Result<String, String> {
    let (url, fields, file_field): (&str, Vec<(&str, &str)>, &str) = match host {
        Host::Uguu => ("https://uguu.se/upload", vec![], "files[]"),
        Host::Litter24 => (
            "https://litterbox.catbox.moe/resources/internals/api.php",
            vec![("reqtype", "fileupload"), ("time", "24h")],
            "fileToUpload",
        ),
        Host::Litter72 => (
            "https://litterbox.catbox.moe/resources/internals/api.php",
            vec![("reqtype", "fileupload"), ("time", "72h")],
            "fileToUpload",
        ),
        Host::Custom => (custom_url, vec![], if custom_field.is_empty() { "file" } else { custom_field }),
    };
    let u = split_url(url).ok_or_else(|| "bad url".to_string())?;
    let body = multipart(&fields, file_field, name, png);
    let (code, text) = post(&u, &body.1, &body.0)?;
    if !(200..300).contains(&code) {
        return Err(format!("HTTP {code}"));
    }
    find_link(&text).ok_or_else(|| "no link in the answer".to_string())
}

/// (content type, body)
fn multipart(fields: &[(&str, &str)], file_field: &str, name: &str, data: &[u8]) -> (String, Vec<u8>) {
    let boundary = format!(
        "----WispShot{:016x}",
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos() as u64).unwrap_or(0)
    );
    let mut b = Vec::with_capacity(data.len() + 512);
    for (k, v) in fields {
        b.extend_from_slice(
            format!("--{boundary}\r\nContent-Disposition: form-data; name=\"{k}\"\r\n\r\n{v}\r\n").as_bytes(),
        );
    }
    b.extend_from_slice(
        format!("--{boundary}\r\nContent-Disposition: form-data; name=\"{file_field}\"; filename=\"{name}\"\r\nContent-Type: image/png\r\n\r\n")
            .as_bytes(),
    );
    b.extend_from_slice(data);
    b.extend_from_slice(format!("\r\n--{boundary}--\r\n").as_bytes());
    (format!("multipart/form-data; boundary={boundary}"), b)
}

/// The first http(s) link in the answer (plain text, or JSON with escaped slashes).
fn find_link(text: &str) -> Option<String> {
    let t = text.replace("\\/", "/");
    let i = t.find("https://").or_else(|| t.find("http://"))?;
    let link: String =
        t[i..].chars().take_while(|c| !c.is_whitespace() && !matches!(c, '"' | '\'' | '<' | '>' | ',')).collect();
    (link.len() > 10).then_some(link)
}

struct H(*mut c_void);
impl Drop for H {
    fn drop(&mut self) {
        if !self.0.is_null() {
            unsafe {
                let _ = WinHttpCloseHandle(self.0);
            }
        }
    }
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(Some(0)).collect()
}

fn post(u: &Url, body: &[u8], content_type: &str) -> Result<(u32, String), String> {
    let fail = |what: &str| format!("{what}: {}", windows::core::Error::from_thread().message());
    unsafe {
        let ua = wide(&format!("WispShot/{}", env!("CARGO_PKG_VERSION")));
        let s =
            H(WinHttpOpen(PCWSTR(ua.as_ptr()), WINHTTP_ACCESS_TYPE_AUTOMATIC_PROXY, PCWSTR::null(), PCWSTR::null(), 0));
        if s.0.is_null() {
            return Err(fail("open"));
        }
        let _ = WinHttpSetTimeouts(s.0, 10_000, 10_000, 60_000, 60_000);
        let host = wide(&u.host);
        let c = H(WinHttpConnect(s.0, PCWSTR(host.as_ptr()), u.port, 0));
        if c.0.is_null() {
            return Err(fail("connect"));
        }
        let path = wide(&u.path);
        let flags = if u.secure { WINHTTP_FLAG_SECURE } else { WINHTTP_OPEN_REQUEST_FLAGS(0) };
        let r = H(WinHttpOpenRequest(
            c.0,
            w!("POST"),
            PCWSTR(path.as_ptr()),
            PCWSTR::null(),
            PCWSTR::null(),
            std::ptr::null(),
            flags,
        ));
        if r.0.is_null() {
            return Err(fail("request"));
        }
        let headers: Vec<u16> = format!("Content-Type: {content_type}\r\n").encode_utf16().collect();
        WinHttpSendRequest(
            r.0,
            Some(&headers),
            Some(body.as_ptr() as *const c_void),
            body.len() as u32,
            body.len() as u32,
            0,
        )
        .map_err(|e| format!("send: {}", e.message()))?;
        WinHttpReceiveResponse(r.0, std::ptr::null_mut()).map_err(|e| format!("receive: {}", e.message()))?;
        let mut code: u32 = 0;
        let mut len = std::mem::size_of::<u32>() as u32;
        let _ = WinHttpQueryHeaders(
            r.0,
            WINHTTP_QUERY_STATUS_CODE | WINHTTP_QUERY_FLAG_NUMBER,
            PCWSTR::null(),
            Some(&mut code as *mut u32 as *mut c_void),
            &mut len,
            std::ptr::null_mut(),
        );
        let mut out = vec![];
        loop {
            let mut avail = 0u32;
            if WinHttpQueryDataAvailable(r.0, &mut avail).is_err() || avail == 0 || out.len() > 1 << 20 {
                break;
            }
            let mut buf = vec![0u8; avail as usize];
            let mut got = 0u32;
            if WinHttpReadData(r.0, buf.as_mut_ptr() as *mut c_void, avail, &mut got).is_err() || got == 0 {
                break;
            }
            out.extend_from_slice(&buf[..got as usize]);
        }
        Ok((code, String::from_utf8_lossy(&out).into_owned()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_links_and_urls() {
        let uguu = r#"{"success":true,"files":[{"hash":"ab","filename":"x.png","url":"https:\/\/h.uguu.se\/aBcD.png","size":5}]}"#;
        assert_eq!(find_link(uguu).as_deref(), Some("https://h.uguu.se/aBcD.png"));
        assert_eq!(
            find_link("https://litter.catbox.moe/abc123.png\n").as_deref(),
            Some("https://litter.catbox.moe/abc123.png")
        );
        assert_eq!(find_link("error"), None);
        let u = split_url("https://img.example.com:8443/up?x=1").unwrap();
        assert_eq!((u.host.as_str(), u.port, u.path.as_str(), u.secure), ("img.example.com", 8443, "/up?x=1", true));
        assert!(split_url("ftp://x").is_none());
    }
}

#[cfg(test)]
mod live {
    /// Real uploads of a small gradient: `cargo test --release -- --ignored live_upload`
    #[test]
    #[ignore]
    fn live_upload() {
        unsafe {
            let _ =
                windows::Win32::System::Com::CoInitializeEx(None, windows::Win32::System::Com::COINIT_MULTITHREADED);
        }
        let mut img = crate::img::Img::new(64, 64);
        for (i, p) in img.px.iter_mut().enumerate() {
            *p = 0xFF00_0000 | ((i % 64) as u32 * 4) << 16 | ((i / 64) as u32 * 4);
        }
        let png = crate::output::encode(&img, crate::output::Fmt::Png).unwrap();
        for h in super::Host::SHOWN {
            println!("{:?}: {:?}", h, super::upload(h, "", "", &png, "wispshot-test.png"));
        }
    }
}
