use std::collections::BTreeMap;
use std::time::Duration;

pub const SESSION_COOKIE: &str = "arvm_anon";
pub const SESSION_TTL_SECS: u64 = 1800;
pub const SESSION_TTL: Duration = Duration::from_secs(SESSION_TTL_SECS);

pub fn extract_sid(cookie_header: &str) -> Option<String> {
    for part in cookie_header.split(';') {
        let t = part.trim();
        #[allow(clippy::collapsible_if)]
        if let Some((k, v)) = t.split_once('=') {
            if k.trim() == SESSION_COOKIE {
                return Some(v.trim().to_owned());
            }
        }
    }
    None
}

pub fn is_valid_sid_format(s: &str) -> bool {
    s.len() == 64 && s.chars().all(|c| c.is_ascii_hexdigit())
}

pub fn generate_sid() -> Option<String> {
    let mut buf = [0u8; 32];
    read_urandom(&mut buf).ok()?;
    Some(buf.iter().map(|b| format!("{b:02x}")).collect())
}

fn read_urandom(buf: &mut [u8]) -> std::io::Result<()> {
    use std::io::Read;
    let mut f = std::fs::File::open("/dev/urandom")?;
    f.read_exact(buf)
}

pub fn cookie_header(id: &str, secure: bool) -> String {
    let secure_attribute = if secure { "; Secure" } else { "" };
    format!(
        "{SESSION_COOKIE}={id}; Path=/; HttpOnly; SameSite=Strict; Max-Age={SESSION_TTL_SECS}{secure_attribute}"
    )
}

pub fn check_origin(headers: &BTreeMap<String, String>, allowed_origin: &str) -> bool {
    headers
        .get("origin")
        .is_some_and(|origin| origin.trim() == allowed_origin)
}
