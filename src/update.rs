//! 「关于」里的版本检查：问 GitHub 最新 Release 是哪个版本。
//!
//! 用系统自带的 WinHTTP（不额外拖 TLS 依赖）。两条路：
//!
//! 1. **主路**：GET `https://github.com/qlings/rdp-launcher/releases/latest`，
//!    GitHub 会 302 到 `/releases/tag/vX.Y.Z`，从 `Location` 里取 tag。
//!    这条路不碰 REST API，不受「同一出口 IP 60 次/小时」的限流。
//! 2. **兜底**：REST API `releases/latest` 的 `tag_name`（受限流影响）。
//!
//! 检查在后台线程里跑，UI 只轮询结果，网络慢也不会卡住界面。

use std::ffi::c_void;

use windows::core::PCWSTR;
use windows::Win32::Networking::WinHttp::{
    WinHttpAddRequestHeaders, WinHttpCloseHandle, WinHttpConnect, WinHttpOpen, WinHttpOpenRequest,
    WinHttpQueryDataAvailable, WinHttpQueryHeaders, WinHttpQueryOption, WinHttpReadData,
    WinHttpReceiveResponse, WinHttpSendRequest, WinHttpSetTimeouts, INTERNET_DEFAULT_HTTPS_PORT,
    WINHTTP_ACCESS_TYPE_AUTOMATIC_PROXY, WINHTTP_ADDREQ_FLAG_ADD, WINHTTP_FLAG_SECURE,
    WINHTTP_OPTION_URL, WINHTTP_QUERY_FLAG_NUMBER, WINHTTP_QUERY_LOCATION, WINHTTP_QUERY_STATUS_CODE,
};

/// 仓库主页（关于里可点）。
pub const REPO_URL: &str = "https://github.com/qlings/rdp-launcher";
/// Releases 页：永远指向最新版，下载 exe 就点这个。
pub const DOWNLOAD_URL: &str = "https://github.com/qlings/rdp-launcher/releases/latest";

const API_HOST: &str = "api.github.com";
const API_PATH: &str = "/repos/qlings/rdp-launcher/releases/latest";
const WEB_HOST: &str = "github.com";
const WEB_LATEST_PATH: &str = "/qlings/rdp-launcher/releases/latest";
const TAG_PATH_MARKER: &str = "/releases/tag/";
/// 整个请求最多等这么久，离线时别让「检查中…」一直转。
const RESOLVE_TIMEOUT_MS: i32 = 3_000;
const CONNECT_TIMEOUT_MS: i32 = 3_000;
const SEND_TIMEOUT_MS: i32 = 3_000;
const RECEIVE_TIMEOUT_MS: i32 = 5_000;

/// 「关于」里显示的最新版状态。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum UpdateState {
    /// 还没查过
    #[default]
    Idle,
    Checking,
    /// 远端版本不比本机新
    Latest(String),
    /// 远端有更新的版本
    Newer(String),
    /// 检查失败（没网 / GitHub 不通 / 被限流）
    Failed(String),
}

impl UpdateState {
    pub fn status_text(&self) -> String {
        match self {
            UpdateState::Idle | UpdateState::Checking => "最新版本：检查中…".into(),
            UpdateState::Latest(tag) => format!("最新版本：{tag}（已是最新）"),
            UpdateState::Newer(tag) => format!("最新版本：{tag}（有新版，点下面的下载地址）"),
            UpdateState::Failed(reason) => format!("最新版本：检查失败（{reason}）"),
        }
    }

    /// 「重新检查」是否可点（已经在查就不重复发请求）。
    pub fn can_recheck(&self) -> bool {
        !matches!(self, UpdateState::Checking)
    }
}

/// 包一层 WinHTTP 句柄，保证每条返回路径都会关掉。
struct Internet(*mut c_void);

impl Internet {
    fn new(raw: *mut c_void) -> Option<Self> {
        (!raw.is_null()).then_some(Self(raw))
    }
}

impl Drop for Internet {
    fn drop(&mut self) {
        // SAFETY: 句柄由 WinHTTP 创建，Drop 里只关一次。
        unsafe {
            let _ = WinHttpCloseHandle(self.0);
        }
    }
}

struct HttpResponse {
    status: u32,
    /// 302/301 的跳转目标
    location: Option<String>,
    /// 跟完跳转后的最终地址（WinHTTP 默认会跟 GET 的跳转）
    final_url: Option<String>,
    body: String,
}

fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(std::iter::once(0)).collect()
}

fn read_header(request: *mut c_void, query: u32) -> Option<String> {
    let mut buffer = [0u16; 1024];
    let mut size = (buffer.len() * std::mem::size_of::<u16>()) as u32;
    // SAFETY: buffer 按字节长度 size 传入，调用后 size 回写实际长度。
    let status = unsafe {
        WinHttpQueryHeaders(
            request,
            query,
            PCWSTR::null(),
            Some(buffer.as_mut_ptr() as *mut c_void),
            &mut size,
            std::ptr::null_mut(),
        )
    };
    if status.is_err() {
        return None;
    }
    decode_wide(&buffer, size)
}

/// 读一个字符串型的 WinHTTP 选项（目前只用来取跳转后的最终 URL）。
fn read_option_string(handle: *mut c_void, option: u32) -> Option<String> {
    let mut buffer = [0u16; 2048];
    let mut size = (buffer.len() * std::mem::size_of::<u16>()) as u32;
    // SAFETY: buffer 按字节长度传入，调用后 size 回写实际长度。
    let status = unsafe {
        WinHttpQueryOption(
            handle,
            option,
            Some(buffer.as_mut_ptr() as *mut c_void),
            &mut size,
        )
    };
    if status.is_err() {
        return None;
    }
    decode_wide(&buffer, size)
}

fn decode_wide(buffer: &[u16], byte_size: u32) -> Option<String> {
    let units = &buffer[..(byte_size as usize / 2).min(buffer.len())];
    let text = String::from_utf16_lossy(units)
        .trim_end_matches('\0')
        .trim()
        .to_string();
    (!text.is_empty()).then_some(text)
}

/// 同步 GET 一个 https 地址（不跟随跳转，302 的 Location 由调用方看）。
fn https_get(host: &str, path: &str) -> Result<HttpResponse, String> {
    let agent = wide("rdp-launcher");
    let verb = wide("GET");
    let host_w = wide(host);
    let path_w = wide(path);
    let headers = wide("User-Agent: rdp-launcher\r\nAccept: */*\r\n");

    // SAFETY: 所有字符串在调用期间存活且以 NUL 结尾；句柄由 Internet 负责关闭。
    unsafe {
        let session = Internet::new(WinHttpOpen(
            PCWSTR(agent.as_ptr()),
            WINHTTP_ACCESS_TYPE_AUTOMATIC_PROXY,
            PCWSTR::null(),
            PCWSTR::null(),
            0,
        ))
        .ok_or("WinHTTP 初始化失败")?;
        let _ = WinHttpSetTimeouts(
            session.0,
            RESOLVE_TIMEOUT_MS,
            CONNECT_TIMEOUT_MS,
            SEND_TIMEOUT_MS,
            RECEIVE_TIMEOUT_MS,
        );

        let connect = Internet::new(WinHttpConnect(
            session.0,
            PCWSTR(host_w.as_ptr()),
            INTERNET_DEFAULT_HTTPS_PORT,
            0,
        ))
        .ok_or("连不上 GitHub（没网或被代理拦了）")?;

        let request = Internet::new(WinHttpOpenRequest(
            connect.0,
            PCWSTR(verb.as_ptr()),
            PCWSTR(path_w.as_ptr()),
            PCWSTR::null(),
            PCWSTR::null(),
            std::ptr::null(),
            WINHTTP_FLAG_SECURE,
        ))
        .ok_or("创建 HTTPS 请求失败")?;

        let _ = WinHttpAddRequestHeaders(request.0, &headers, WINHTTP_ADDREQ_FLAG_ADD);

        WinHttpSendRequest(request.0, None, None, 0, 0, 0).map_err(|_| "请求发送失败")?;
        WinHttpReceiveResponse(request.0, std::ptr::null_mut())
            .map_err(|_| "没收到 GitHub 的响应")?;

        let mut status = 0u32;
        let mut status_size = std::mem::size_of::<u32>() as u32;
        WinHttpQueryHeaders(
            request.0,
            WINHTTP_QUERY_STATUS_CODE | WINHTTP_QUERY_FLAG_NUMBER,
            PCWSTR::null(),
            Some(&mut status as *mut u32 as *mut c_void),
            &mut status_size,
            std::ptr::null_mut(),
        )
        .map_err(|_| "读不到 HTTP 状态码")?;

        let location = read_header(request.0, WINHTTP_QUERY_LOCATION);
        let final_url = read_option_string(request.0, WINHTTP_OPTION_URL);

        let mut body = Vec::new();
        loop {
            let mut available = 0u32;
            if WinHttpQueryDataAvailable(request.0, &mut available).is_err() || available == 0 {
                break;
            }
            let mut chunk = vec![0u8; available as usize];
            let mut read = 0u32;
            WinHttpReadData(
                request.0,
                chunk.as_mut_ptr() as *mut c_void,
                available,
                &mut read,
            )
            .map_err(|_| "读取响应失败")?;
            if read == 0 {
                break;
            }
            body.extend_from_slice(&chunk[..read as usize]);
            // 只是防无聊的服务器无限推流，正常页面几十 KB 就结束
            if body.len() > 1024 * 1024 {
                break;
            }
        }

        Ok(HttpResponse {
            status,
            location,
            final_url,
            body: String::from_utf8_lossy(&body).into_owned(),
        })
    }
}

/// 从 `https://github.com/qlings/rdp-launcher/releases/tag/v0.4.4` 里取 `v0.4.4`。
fn tag_from_location(url: &str) -> Option<String> {
    let (_, rest) = url.split_once(TAG_PATH_MARKER)?;
    let tag = rest
        .split(['?', '#'])
        .next()
        .unwrap_or("")
        .trim_end_matches('/')
        .trim();
    (!tag.is_empty()).then(|| tag.to_string())
}

/// 兜底：直接在 Release 页 HTML 里找第一个 `/releases/tag/<tag>`。
fn tag_from_html(body: &str) -> Option<String> {
    let (_, rest) = body.split_once(TAG_PATH_MARKER)?;
    let tag: String = rest
        .chars()
        .take_while(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_' | '+'))
        .collect();
    (!tag.is_empty()).then_some(tag)
}

/// 从 Releases API 的 JSON 里取 `tag_name`。
fn tag_from_json(body: &str) -> Option<String> {
    let value: serde_json::Value = serde_json::from_str(body).ok()?;
    let tag = value.get("tag_name")?.as_str()?.trim().to_string();
    (!tag.is_empty()).then_some(tag)
}

/// 主路：`releases/latest` 页面（不限流）。
///
/// WinHTTP 默认会跟 302 到版本页，这时看最终 URL；没跟的话看 `Location` 头；
/// 都不行就在页面 HTML 里找版本链接。
fn latest_tag_via_web() -> Result<String, String> {
    let response = https_get(WEB_HOST, WEB_LATEST_PATH)?;
    if response.status == 404 {
        return Err("GitHub 上还没有已发布的 Release".into());
    }
    let tag = response
        .final_url
        .as_deref()
        .and_then(tag_from_location)
        .or_else(|| response.location.as_deref().and_then(tag_from_location))
        .or_else(|| tag_from_html(&response.body));
    tag.ok_or_else(|| format!("GitHub 返回 HTTP {}（找不到版本号）", response.status))
}

/// 兜底：REST API 的 `tag_name`（同一出口 IP 每小时 60 次）。
fn latest_tag_via_api() -> Result<String, String> {
    let response = https_get(API_HOST, API_PATH)?;
    match response.status {
        200 => tag_from_json(&response.body).ok_or_else(|| "API 响应里没有版本号".to_string()),
        403 => Err("GitHub API 限流了".into()),
        code => Err(format!("GitHub API 返回 HTTP {code}")),
    }
}

/// 查 GitHub 最新 Release 的 tag（例如 `v0.4.5`）。阻塞，调用方放后台线程。
pub fn check_latest() -> Result<String, String> {
    match latest_tag_via_web() {
        Ok(tag) => Ok(tag),
        Err(web_error) => match latest_tag_via_api() {
            Ok(tag) => Ok(tag),
            Err(api_error) => Err(format!("{web_error}；{api_error}")),
        },
    }
}

/// 解析 `v0.4.5` / `0.4.5` / `0.5`；解析不出来返回 None。
fn version_parts(text: &str) -> Option<(u64, u64, u64)> {
    let text = text.trim();
    let digits_start = text.trim_start_matches(|c| c == 'v' || c == 'V');
    if !digits_start.starts_with(|c: char| c.is_ascii_digit()) {
        return None;
    }
    let mut parts = [0u64; 3];
    for (slot, piece) in digits_start.split('.').take(3).enumerate() {
        let digits: String = piece.chars().take_while(|c| c.is_ascii_digit()).collect();
        parts[slot] = digits.parse().ok()?;
    }
    Some((parts[0], parts[1], parts[2]))
}

/// 远端版本是否比本机新。
pub fn is_newer(current: &str, latest: &str) -> bool {
    match (version_parts(current), version_parts(latest)) {
        (Some(current), Some(latest)) => latest > current,
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_versions_with_and_without_prefix() {
        assert_eq!(version_parts("v0.4.5"), Some((0, 4, 5)));
        assert_eq!(version_parts("0.5.0"), Some((0, 5, 0)));
        assert_eq!(version_parts("0.5"), Some((0, 5, 0)));
        assert_eq!(version_parts("1.2.3-beta"), Some((1, 2, 3)));
        assert_eq!(version_parts("v"), None);
        assert_eq!(version_parts("latest"), None);
    }

    #[test]
    fn compares_versions_numerically() {
        assert!(is_newer("0.4.5", "v0.4.6"));
        assert!(is_newer("0.4.9", "v0.4.10"), "要按数字比，不能按字符串");
        assert!(is_newer("0.4.5", "v0.5.0"));
        assert!(!is_newer("0.5.0", "v0.5.0"));
        assert!(!is_newer("0.5.0", "v0.4.9"));
        assert!(!is_newer("0.5.0", "garbage"));
    }

    #[test]
    fn reads_tag_from_release_json() {
        let body = r#"{"tag_name":"v0.4.5","name":"v0.4.5"}"#;
        assert_eq!(tag_from_json(body).as_deref(), Some("v0.4.5"));
        assert_eq!(tag_from_json(r#"{"message":"Not Found"}"#), None);
        assert_eq!(tag_from_json("not json"), None);
    }

    #[test]
    fn reads_tag_from_redirect_location() {
        assert_eq!(
            tag_from_location("https://github.com/qlings/rdp-launcher/releases/tag/v0.4.4")
                .as_deref(),
            Some("v0.4.4")
        );
        assert_eq!(
            tag_from_location("https://github.com/qlings/rdp-launcher/releases/tag/v0.5.0?x=1")
                .as_deref(),
            Some("v0.5.0")
        );
        assert_eq!(tag_from_location("https://github.com/qlings/rdp-launcher"), None);
    }

    #[test]
    fn finds_tag_link_in_release_html() {
        let html = r#"<a href="/qlings/rdp-launcher/releases/tag/v0.4.4">v0.4.4</a>"#;
        assert_eq!(tag_from_html(html).as_deref(), Some("v0.4.4"));
        assert_eq!(tag_from_html("<html>no release here</html>"), None);
    }

    #[test]
    fn status_text_matches_state() {
        assert!(UpdateState::Checking.status_text().contains("检查中"));
        assert!(UpdateState::Latest("v0.5.0".into())
            .status_text()
            .contains("已是最新"));
        assert!(UpdateState::Newer("v0.6.0".into())
            .status_text()
            .contains("有新版"));
        assert!(UpdateState::Failed("没网".into()).status_text().contains("没网"));
    }

    /// 真联网测试，默认 ignore（`cargo test -- --ignored` 时才跑）。
    #[test]
    #[ignore]
    fn fetches_latest_release_from_github() {
        let tag = check_latest().expect("GitHub 应该能访问");
        assert!(version_parts(&tag).is_some(), "tag 应该是版本号：{tag}");
    }
}
