//! 主机的数据结构、地址校验、分组 / 排序 / 过滤。
//!
//! 主机地址拆成「主机 + 端口」两个字段：端口单独一个框，编辑时不会把端口写进
//! 主机名里。这里的函数全部是纯逻辑，不碰 UI、不碰 Windows API，方便单测。

use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

pub const DEFAULT_PORT: u16 = 3389;

/// 托盘右键「快速连接」最多列几台最近主机：只放最常用的，不把几十台全铺开。
pub const QUICK_CONNECT_LIMIT: usize = 3;

/// 设置里可选的探活自动刷新间隔（秒），0 = 只在启动 / 手动点时测。
pub const PROBE_INTERVALS: [u64; 4] = [30, 60, 300, 0];

fn default_port() -> u16 {
    DEFAULT_PORT
}

fn default_true() -> bool {
    true
}

fn default_probe_interval() -> u64 {
    60
}

pub fn new_id() -> String {
    use rand::Rng;
    let mut rng = rand::thread_rng();
    (0..16)
        .map(|_| format!("{:x}", rng.gen_range(0..16u8)))
        .collect()
}

// ---------------------------------------------------------------------------
// 设置
// ---------------------------------------------------------------------------

#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug, Default)]
#[serde(rename_all = "lowercase")]
pub enum SortMode {
    /// 按备注名称
    #[default]
    Name,
    /// 按主机地址（同地址再按端口）
    Address,
    /// 按用户填的序号
    Custom,
}

impl SortMode {
    pub fn label(self) -> &'static str {
        match self {
            SortMode::Name => "名称",
            SortMode::Address => "主机地址",
            SortMode::Custom => "自定义",
        }
    }
}

#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug, Default)]
#[serde(rename_all = "lowercase")]
pub enum ViewMode {
    #[default]
    List,
    Grid,
}

impl ViewMode {
    pub fn label(self) -> &'static str {
        match self {
            ViewMode::List => "列表",
            ViewMode::Grid => "方块",
        }
    }
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Group {
    pub name: String,
    #[serde(default)]
    pub order: i64,
    /// 只在设置草稿里用：记住改名前叫什么，保存时把主机里的分组引用一起改掉。
    #[serde(skip)]
    pub orig: String,
}

impl Group {
    pub fn new(name: impl Into<String>, order: i64) -> Self {
        let name = name.into();
        Self {
            orig: name.clone(),
            name,
            order,
        }
    }
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Settings {
    #[serde(default)]
    pub sort: SortMode,
    /// 启动后是否做 TCP 探活
    #[serde(default = "default_true")]
    pub probe: bool,
    /// 探活自动刷新间隔（秒）；0 = 只在启动 / 手动点时测
    #[serde(default = "default_probe_interval")]
    pub probe_interval_secs: u64,
    /// 是否显示顶部快速搜索栏
    #[serde(default = "default_true")]
    pub show_search: bool,
    #[serde(default)]
    pub view: ViewMode,
    /// 启动时先展示的分组名；空字符串表示「全部」
    #[serde(default)]
    pub default_group: String,
    #[serde(default)]
    pub dark: bool,
    /// 关闭主窗口后留在任务栏托盘，不退出进程
    #[serde(default)]
    pub close_to_tray: bool,
    /// 是否在顶部提示「Windows 新版 RDP 安全警告」；用户点过忽略后置 false
    #[serde(default = "default_true")]
    pub rdp_warning_hint: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            sort: SortMode::Name,
            probe: true,
            probe_interval_secs: default_probe_interval(),
            show_search: true,
            view: ViewMode::List,
            default_group: String::new(),
            dark: false,
            close_to_tray: false,
            rdp_warning_hint: true,
        }
    }
}

// ---------------------------------------------------------------------------
// 主机
// ---------------------------------------------------------------------------

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Host {
    /// 稳定 id：临时 .rdp 文件名、探活结果、UI 上的控件 id 都用它。
    pub id: String,
    #[serde(default)]
    pub name: String,
    /// 纯主机名 / IP / IPv6，不含端口、不含方括号。
    #[serde(default)]
    pub host: String,
    #[serde(default = "default_port")]
    pub port: u16,
    #[serde(default)]
    pub username: String,
    #[serde(default)]
    pub password: String,
    /// 可同时属于多个分组；不填就是未分组。
    #[serde(default)]
    pub groups: Vec<String>,
    /// 要重定向的盘符，大写单字母，例如 `["C", "D"]`。
    #[serde(default)]
    pub drives: Vec<String>,
    #[serde(default = "default_true")]
    pub clipboard: bool,
    /// true = 把远端声音带到本机；false = 不播放声音。
    #[serde(default = "default_true")]
    pub audio: bool,
    /// 自定义排序用的序号，越小越靠前。
    #[serde(default)]
    pub order: i64,
}

impl Default for Host {
    fn default() -> Self {
        Self {
            id: new_id(),
            name: String::new(),
            host: String::new(),
            port: DEFAULT_PORT,
            username: String::new(),
            password: String::new(),
            groups: Vec::new(),
            drives: Vec::new(),
            clipboard: true,
            audio: true,
            order: 0,
        }
    }
}

impl Host {
    /// `.rdp` 的 `full address:s:`：IPv6 必须加方括号。
    pub fn authority(&self) -> String {
        format_authority(&self.host, self.port)
    }

    pub fn display_address(&self) -> String {
        if self.host.trim().is_empty() {
            "(未填写地址)".to_string()
        } else {
            self.authority()
        }
    }

    pub fn display_name(&self) -> &str {
        let name = self.name.trim();
        if !name.is_empty() {
            name
        } else if !self.host.trim().is_empty() {
            self.host.trim()
        } else {
            "(未命名)"
        }
    }

    pub fn in_group(&self, group: &str) -> bool {
        self.groups.iter().any(|g| g == group)
    }

    /// 搜索匹配池：备注名 + 地址 + 端口 + 用户名 + 所有分组。
    pub fn haystack(&self) -> String {
        format!(
            "{} {} {} {} {}",
            self.display_name(),
            self.host,
            self.port,
            self.username,
            self.groups.join(" ")
        )
        .to_lowercase()
    }
}

/// 复制一份时用的名字：原备注名后面加「副本」；这个名字已经被占了就继续加 2、3……
///
/// 备注名为空时用 `display_name()`（也就是主机地址），复制出来仍然认得出是哪台。
pub fn copy_name(host: &Host, existing: &[Host]) -> String {
    let base = host.display_name().trim().to_string();
    let mut candidate = format!("{base}副本");
    let mut n = 2u32;
    while existing.iter().any(|h| h.name.trim() == candidate) && n <= 999 {
        candidate = format!("{base}副本{n}");
        n += 1;
    }
    candidate
}

/// 原样复制一台主机：只换 id、改名字，其余字段（地址、端口、账号、密码、分组、
/// 盘符、剪贴板 / 声音、序号）一个不动。
pub fn duplicate(host: &Host, existing: &[Host]) -> Host {
    let mut copy = host.clone();
    copy.id = new_id();
    copy.name = copy_name(host, existing);
    copy
}

/// 原样复制并插在原主机后面（列表里紧挨着），返回新主机在 `hosts` 里的下标。
pub fn duplicate_into(hosts: &mut Vec<Host>, index: usize) -> Option<usize> {
    let source = hosts.get(index)?.clone();
    let copy = duplicate(&source, hosts);
    hosts.insert(index + 1, copy);
    Some(index + 1)
}

/// 同地址同端口是否已经在列表里（地址大小写不敏感，忽略端口默认值差异由调用方处理）。
pub fn address_exists(hosts: &[Host], host: &str, port: u16) -> bool {
    hosts
        .iter()
        .any(|h| h.host.eq_ignore_ascii_case(host) && h.port == port)
}

/// 托盘右键「快速连接」要显示哪几台：最近连过的排前面，不够就按当前排序补齐。
///
/// 只返回还带地址的主机；`limit` 由 [`QUICK_CONNECT_LIMIT`] 决定。
pub fn quick_host_indices(
    hosts: &[Host],
    recent: &[String],
    sort: SortMode,
    limit: usize,
) -> Vec<usize> {
    let mut picked: Vec<usize> = Vec::new();
    for id in recent {
        if picked.len() >= limit {
            break;
        }
        if let Some(index) = hosts.iter().position(|h| &h.id == id) {
            if hosts[index].host.trim().is_empty() || picked.contains(&index) {
                continue;
            }
            picked.push(index);
        }
    }
    if picked.len() < limit {
        for index in visible_indices(hosts, "", None, sort) {
            if picked.len() >= limit {
                break;
            }
            if hosts[index].host.trim().is_empty() || picked.contains(&index) {
                continue;
            }
            picked.push(index);
        }
    }
    picked
}

/// 一次导入的结果：新建了哪些主机（id）、跳过了几条（已存在 / 地址非法）。
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct ImportReport {
    pub added: Vec<String>,
    pub skipped: usize,
}

impl ImportReport {
    pub fn added_count(&self) -> usize {
        self.added.len()
    }
}

/// 把历史记录导入成主机：同地址同端口的已经在列表里就跳过。
///
/// 导入的主机按「剪贴板开、声音关、映射全部盘符」的默认值来，备注名留空
/// （列表里直接显示地址），需要的话再逐台改。
pub fn import_hosts(
    hosts: &mut Vec<Host>,
    entries: &[(String, u16, String)],
    drives: &[String],
) -> ImportReport {
    let mut report = ImportReport::default();
    let mut order = hosts.iter().map(|h| h.order).max().unwrap_or(-1) + 1;

    for (host, port, username) in entries {
        let host = clean_host(host);
        if validate_host(&host).is_err() || *port == 0 {
            report.skipped += 1;
            continue;
        }
        if address_exists(hosts, &host, *port) {
            report.skipped += 1;
            continue;
        }
        let id = new_id();
        hosts.push(Host {
            id: id.clone(),
            name: String::new(),
            host,
            port: *port,
            username: username.trim().to_string(),
            password: String::new(),
            groups: Vec::new(),
            drives: drives.to_vec(),
            clipboard: true,
            audio: false,
            order,
        });
        order += 1;
        report.added.push(id);
    }
    report
}

pub fn format_authority(host: &str, port: u16) -> String {
    let host = host.trim();
    if host.contains(':') {
        format!("[{host}]:{port}")
    } else {
        format!("{host}:{port}")
    }
}

/// 去掉控制字符、BOM、首尾空白；`[fe80::1]` 这种整串方括号也剥掉。
pub fn clean_host(input: &str) -> String {
    let cleaned: String = input
        .chars()
        .filter(|c| !c.is_control() && *c != '\u{feff}')
        .collect();
    let mut host = cleaned.trim().to_string();
    if host.starts_with('[') && host.ends_with(']') && host.len() > 2 {
        host = host[1..host.len() - 1].to_string();
    }
    host
}

/// 拆掉 IPv6 的 zone id（`%` 后面只允许数字），返回地址本体。
fn split_ipv6_zone(host: &str) -> Result<&str, String> {
    match host.split_once('%') {
        None => Ok(host),
        Some((addr, zone)) => {
            if zone.is_empty() || !zone.chars().all(|c| c.is_ascii_digit()) {
                return Err("IPv6 地址的 % 后面只能是接口编号".into());
            }
            Ok(addr)
        }
    }
}

fn validate_ipv6_text(host: &str) -> Result<(), String> {
    let addr = split_ipv6_zone(host)?;
    if !addr
        .chars()
        .all(|c| c.is_ascii_hexdigit() || c == ':' || c == '.')
    {
        return Err("IPv6 地址含有非法字符".into());
    }
    Ok(())
}

/// 校验「主机」字段。端口必须走端口框，不在这里接受 `host:port`。
pub fn validate_host(host: &str) -> Result<(), String> {
    let host = host.trim();
    if host.is_empty() {
        return Err("主机地址不能为空".into());
    }
    if host.len() > 253 {
        return Err("主机地址过长".into());
    }
    if host
        .chars()
        .any(|c| c.is_whitespace() || c.is_control() || matches!(c, '/' | '\\' | '@' | '#' | '?' | '&'))
    {
        return Err("主机地址含有非法字符".into());
    }
    let colon_count = host.matches(':').count();
    if colon_count == 1 {
        return Err("端口请填到右边的端口框，主机地址里不要带冒号".into());
    }
    if colon_count >= 2 {
        // IPv6：只允许十六进制、冒号和点（IPv4 映射地址），可以带 %接口编号
        validate_ipv6_text(host)?;
    }
    Ok(())
}

fn parse_port(raw: &str) -> Result<u16, String> {
    let raw = raw.trim();
    if raw.is_empty() {
        return Err("端口不能为空".into());
    }
    if !raw.chars().all(|c| c.is_ascii_digit()) {
        return Err("端口只能是数字".into());
    }
    match raw.parse::<u32>() {
        Ok(p) if (1..=65535).contains(&p) => Ok(p as u16),
        Ok(_) => Err("端口必须在 1~65535 之间".into()),
        Err(_) => Err("端口数值不合法".into()),
    }
}

/// 解析旧版 / 导入时的完整地址：`host`、`host:port`、`[ipv6]:port`、裸 IPv6。
pub fn parse_host_port(input: &str) -> Result<(String, u16), String> {
    let cleaned: String = input
        .chars()
        .filter(|c| !c.is_control() && *c != '\u{feff}')
        .collect();
    let text = cleaned.trim();
    if text.is_empty() {
        return Err("服务器地址不能为空".into());
    }

    if let Some(rest) = text.strip_prefix('[') {
        let Some(close) = rest.find(']') else {
            return Err("IPv6 地址缺少右方括号".into());
        };
        let host = &rest[..close];
        if host.is_empty() {
            return Err("IPv6 地址不能为空".into());
        }
        if !host.contains(':') {
            return Err("方括号里不是合法的 IPv6 地址".into());
        }
        let after = &rest[close + 1..];
        let port = if after.is_empty() {
            DEFAULT_PORT
        } else if let Some(p) = after.strip_prefix(':') {
            parse_port(p)?
        } else {
            return Err("方括号后面只能是 :端口".into());
        };
        return Ok((host.to_string(), port));
    }

    let colon_count = text.matches(':').count();
    if colon_count >= 2 {
        validate_ipv6_text(text)?;
        return Ok((text.to_string(), DEFAULT_PORT));
    }

    if colon_count == 1 {
        let (host, port) = text.split_once(':').expect("colon_count == 1");
        let host = clean_host(host);
        validate_host(&host)?;
        return Ok((host, parse_port(port)?));
    }

    let host = clean_host(text);
    validate_host(&host)?;
    Ok((host, DEFAULT_PORT))
}

// ---------------------------------------------------------------------------
// 分组 / 排序 / 过滤
// ---------------------------------------------------------------------------

/// 只在导入旧数据时用：从主机里提取分组名，去重并按码点排序。
pub fn collect_groups(hosts: &[Host]) -> Vec<String> {
    let mut set = BTreeSet::new();
    for host in hosts {
        for group in &host.groups {
            let group = group.trim();
            if !group.is_empty() {
                set.insert(group.to_string());
            }
        }
    }
    set.into_iter().collect()
}

/// 过滤 + 排序。返回的是 `hosts` 里的原始下标。
pub fn visible_indices(
    hosts: &[Host],
    query: &str,
    group: Option<&str>,
    sort: SortMode,
) -> Vec<usize> {
    let q = query.trim().to_lowercase();
    let mut indices: Vec<usize> = hosts
        .iter()
        .enumerate()
        .filter(|(_, host)| {
            if let Some(g) = group {
                if !host.in_group(g) {
                    return false;
                }
            }
            q.is_empty() || host.haystack().contains(&q)
        })
        .map(|(i, _)| i)
        .collect();

    // 小写比较键每台主机只算一次（原来每次比较都要 to_lowercase 分配一次）。
    match sort {
        SortMode::Name => indices.sort_by_cached_key(|&a| {
            (
                hosts[a].display_name().to_lowercase(),
                hosts[a].host.to_lowercase(),
                hosts[a].port,
                hosts[a].id.clone(),
            )
        }),
        SortMode::Address => indices.sort_by_cached_key(|&a| {
            (
                hosts[a].host.to_lowercase(),
                hosts[a].port,
                hosts[a].display_name().to_string(),
                hosts[a].id.clone(),
            )
        }),
        SortMode::Custom => indices.sort_by_cached_key(|&a| {
            (
                hosts[a].order,
                hosts[a].display_name().to_lowercase(),
                hosts[a].id.clone(),
            )
        }),
    }

    indices
}

// ---------------------------------------------------------------------------
// 测试
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn host(name: &str, address: &str, groups: &[&str], order: i64) -> Host {
        let (h, p) = parse_host_port(address).unwrap();
        Host {
            id: format!("id-{name}-{address}"),
            name: name.into(),
            host: h,
            port: p,
            groups: groups.iter().map(|g| g.to_string()).collect(),
            order,
            ..Default::default()
        }
    }

    #[test]
    fn parses_domain_with_port() {
        let (host, port) = parse_host_port("rdp.example.com:3390").unwrap();
        assert_eq!(host, "rdp.example.com");
        assert_eq!(port, 3390);
        assert_eq!(format_authority(&host, port), "rdp.example.com:3390");
    }

    #[test]
    fn parses_domain_without_port() {
        let (host, port) = parse_host_port("rdp.example.com").unwrap();
        assert_eq!(host, "rdp.example.com");
        assert_eq!(port, DEFAULT_PORT);
    }

    #[test]
    fn parses_ipv4_and_ipv6() {
        assert_eq!(
            parse_host_port("192.168.1.10:33890").unwrap(),
            ("192.168.1.10".to_string(), 33890)
        );
        assert_eq!(
            parse_host_port("[fe80::1]:3390").unwrap(),
            ("fe80::1".to_string(), 3390)
        );
        assert_eq!(
            parse_host_port("fe80::1").unwrap(),
            ("fe80::1".to_string(), DEFAULT_PORT)
        );
        assert_eq!(format_authority("fe80::1", 3389), "[fe80::1]:3389");
    }

    #[test]
    fn accepts_ipv6_zone_id() {
        assert!(validate_host("fe80::1%12").is_ok());
        assert_eq!(
            parse_host_port("[fe80::1%12]:3390").unwrap(),
            ("fe80::1%12".to_string(), 3390)
        );
        assert_eq!(format_authority("fe80::1%12", 3389), "[fe80::1%12]:3389");
        assert!(validate_host("fe80::1%").is_err());
        assert!(validate_host("fe80::1%eth0").is_err());
    }

    #[test]
    fn same_ip_different_ports_are_distinct_authorities() {
        let a = host("A", "10.0.0.5:3389", &[], 0);
        let b = host("B", "10.0.0.5:3390", &[], 0);
        assert_eq!(a.authority(), "10.0.0.5:3389");
        assert_eq!(b.authority(), "10.0.0.5:3390");
        assert_ne!(a.authority(), b.authority());
    }

    #[test]
    fn rejects_port_inside_host_field() {
        assert!(validate_host("1.2.3.4:3389").is_err());
        assert!(validate_host("rdp.example.com:3390").is_err());
        assert!(validate_host("fe80::1").is_ok());
    }

    #[test]
    fn rejects_bad_ports() {
        assert!(parse_host_port("host:0").is_err());
        assert!(parse_host_port("host:65536").is_err());
        assert!(parse_host_port("host:abc").is_err());
        assert!(parse_host_port("host:").is_err());
    }

    #[test]
    fn strips_newlines_to_prevent_injection() {
        assert!(parse_host_port("192.168.1.1\r\nredirectclipboard:i:1").is_err());
    }

    #[test]
    fn filters_by_multi_group() {
        let hosts = vec![
            host("A", "10.0.0.1", &["公司", "测试"], 0),
            host("B", "10.0.0.2", &["公司"], 1),
            host("C", "10.0.0.3", &[], 2),
        ];
        assert_eq!(visible_indices(&hosts, "", None, SortMode::Name).len(), 3);
        assert_eq!(visible_indices(&hosts, "", Some("公司"), SortMode::Name).len(), 2);
        assert_eq!(visible_indices(&hosts, "", Some("测试"), SortMode::Name).len(), 1);
        assert!(visible_indices(&hosts, "", Some("不存在"), SortMode::Name).is_empty());
    }

    #[test]
    fn sorts_by_name_address_and_custom_order() {
        let hosts = vec![
            host("乙", "10.0.0.9:3390", &[], 2),
            host("甲", "10.0.0.2:3389", &[], 3),
            host("丙", "10.0.0.1:3389", &[], 1),
        ];
        let names: Vec<&str> = visible_indices(&hosts, "", None, SortMode::Custom)
            .iter()
            .map(|&i| hosts[i].display_name())
            .collect();
        assert_eq!(names, vec!["丙", "乙", "甲"]);

        let by_addr: Vec<&str> = visible_indices(&hosts, "", None, SortMode::Address)
            .iter()
            .map(|&i| hosts[i].host.as_str())
            .collect();
        assert_eq!(by_addr, vec!["10.0.0.1", "10.0.0.2", "10.0.0.9"]);

        let by_name: Vec<&str> = visible_indices(&hosts, "", None, SortMode::Name)
            .iter()
            .map(|&i| hosts[i].display_name())
            .collect();
        assert_eq!(by_name.len(), 3);
    }

    #[test]
    fn search_matches_group_and_port() {
        let hosts = vec![host("办公室", "10.0.0.5:3390", &["公司"], 0)];
        assert_eq!(visible_indices(&hosts, "办公", None, SortMode::Name), vec![0]);
        assert_eq!(visible_indices(&hosts, "公司", None, SortMode::Name), vec![0]);
        assert_eq!(visible_indices(&hosts, "3390", None, SortMode::Name), vec![0]);
        assert!(visible_indices(&hosts, "3391", None, SortMode::Name).is_empty());
    }

    #[test]
    fn duplicate_keeps_every_field_but_id_and_name() {
        let source = Host {
            id: "a".into(),
            name: "办公室主力机".into(),
            host: "10.0.0.5".into(),
            port: 3390,
            username: "Admin".into(),
            password: "secret".into(),
            groups: vec!["公司".into(), "测试".into()],
            drives: vec!["C".into(), "D".into()],
            clipboard: false,
            audio: false,
            order: 7,
        };
        let copy = duplicate(&source, std::slice::from_ref(&source));
        assert_eq!(copy.name, "办公室主力机副本");
        assert_ne!(copy.id, source.id);
        assert_eq!(copy.host, source.host);
        assert_eq!(copy.port, source.port);
        assert_eq!(copy.username, source.username);
        assert_eq!(copy.password, source.password);
        assert_eq!(copy.groups, source.groups);
        assert_eq!(copy.drives, source.drives);
        assert_eq!(copy.clipboard, source.clipboard);
        assert_eq!(copy.audio, source.audio);
        assert_eq!(copy.order, source.order);
    }

    #[test]
    fn duplicate_names_are_numbered_when_taken() {
        let source = Host {
            id: "a".into(),
            name: "A".into(),
            host: "10.0.0.1".into(),
            ..Default::default()
        };
        let copy1 = duplicate(&source, std::slice::from_ref(&source));
        assert_eq!(copy1.name, "A副本");

        let mut hosts = vec![source, copy1];
        let copy2 = duplicate(&hosts[0], &hosts);
        assert_eq!(copy2.name, "A副本2");
        hosts.push(copy2);
        let copy3 = duplicate(&hosts[0], &hosts);
        assert_eq!(copy3.name, "A副本3");
    }

    #[test]
    fn duplicate_of_unnamed_host_uses_address_as_base() {
        let source = Host {
            id: "a".into(),
            host: "10.0.0.9".into(),
            ..Default::default()
        };
        assert_eq!(duplicate(&source, &[]).name, "10.0.0.9副本");
    }

    #[test]
    fn duplicate_into_inserts_right_after_source() {
        let source = Host {
            id: "a".into(),
            name: "A".into(),
            host: "10.0.0.1".into(),
            ..Default::default()
        };
        let other = Host {
            id: "b".into(),
            name: "B".into(),
            host: "10.0.0.2".into(),
            ..Default::default()
        };
        let mut hosts = vec![source.clone(), other];
        let at = duplicate_into(&mut hosts, 0).unwrap();
        assert_eq!(at, 1);
        assert_eq!(hosts[1].name, "A副本");
        assert_eq!(hosts[1].host, "10.0.0.1");
        assert_ne!(hosts[1].id, source.id);
        assert_eq!(hosts[2].name, "B", "其它主机的位置不能被挤乱");
        assert!(duplicate_into(&mut hosts, 99).is_none());
    }

    #[test]
    fn import_hosts_skips_existing_and_applies_defaults() {
        let mut hosts = vec![Host {
            id: "a".into(),
            name: "旧主机".into(),
            host: "10.0.0.1".into(),
            port: 3389,
            ..Default::default()
        }];
        let entries = vec![
            ("10.0.0.1".to_string(), 3389, "admin".to_string()),
            ("10.0.0.2".to_string(), 3390, "ql".to_string()),
            ("10.0.0.2".to_string(), 3390, "重复".to_string()),
            (String::new(), 3389, String::new()),
        ];
        let drives = vec!["C".to_string(), "D".to_string()];
        let report = import_hosts(&mut hosts, &entries, &drives);
        assert_eq!((report.added_count(), report.skipped), (1, 3));
        assert_eq!(hosts.len(), 2);
        assert_eq!(report.added, vec![hosts[1].id.clone()]);

        let imported = &hosts[1];
        assert_eq!(imported.host, "10.0.0.2");
        assert_eq!(imported.port, 3390);
        assert_eq!(imported.username, "ql");
        assert!(imported.clipboard, "导入默认带剪贴板");
        assert!(!imported.audio, "导入默认不带声音");
        assert_eq!(imported.drives, drives, "导入默认映射全部盘符");
        assert!(imported.name.is_empty());
        assert_eq!(imported.order, 1, "序号接在现有主机后面");
    }

    #[test]
    fn quick_connect_prefers_recent_then_falls_back_to_sort_order() {
        let hosts = vec![
            host("甲", "10.0.0.1", &[], 0),
            host("乙", "10.0.0.2", &[], 1),
            host("丙", "10.0.0.3", &[], 2),
            host("丁", "10.0.0.4", &[], 3),
        ];
        // 没有任何连接记录：按名称排序取前三台（用的 host() 名字是 甲乙丙丁）
        let picked = quick_host_indices(&hosts, &[], SortMode::Name, QUICK_CONNECT_LIMIT);
        assert_eq!(picked.len(), 3);
        assert_eq!(hosts[picked[0]].name, "丁");

        // 最近连过「甲」：排最前，后面补齐，总共还是 3 台
        let recent = vec![hosts[0].id.clone()];
        let picked = quick_host_indices(&hosts, &recent, SortMode::Name, QUICK_CONNECT_LIMIT);
        assert_eq!(picked.len(), 3);
        assert_eq!(hosts[picked[0]].name, "甲");

        // 最近列表里有已删掉 / 重复的 id，不会占位也不会重复出现
        let recent = vec![
            "gone".to_string(),
            hosts[1].id.clone(),
            hosts[1].id.clone(),
        ];
        let picked = quick_host_indices(&hosts, &recent, SortMode::Name, QUICK_CONNECT_LIMIT);
        assert_eq!(picked.len(), 3);
        assert_eq!(hosts[picked[0]].name, "乙");

        // 没有地址的主机不占快速连接的位置
        let mut with_blank = hosts.clone();
        with_blank[0].host.clear();
        let picked = quick_host_indices(
            &with_blank,
            &[with_blank[0].id.clone()],
            SortMode::Name,
            QUICK_CONNECT_LIMIT,
        );
        assert!(!picked.contains(&0));
        assert_eq!(picked.len(), 3);
    }

    #[test]
    fn settings_defaults_keep_background_mode_off() {
        let settings = Settings::default();
        assert!(!settings.close_to_tray);
        assert!(settings.rdp_warning_hint);
        assert!(settings.probe);
        assert_eq!(settings.probe_interval_secs, 60);
        assert!(settings.show_search);
    }
}
