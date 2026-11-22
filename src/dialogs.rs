//! 两个模态弹窗：新增/编辑主机、设置（含分组编辑）。

use std::collections::{HashMap, HashSet};

use eframe::egui;
use egui::{
    Align, Color32, CornerRadius, Frame, Layout, Margin, RichText, Stroke, Vec2,
};

use crate::drives::DriveInfo;
use crate::models::{self, Group, Host, Settings, SortMode, ViewMode};
use crate::rdp_warning;
use crate::store::Store;
use crate::ui::{chip, segmented, Palette};

// ---------------------------------------------------------------------------
// 主机编辑
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum DialogMode {
    Add,
    Edit(usize),
}

pub struct HostForm {
    pub mode: DialogMode,
    pub name: String,
    pub host: String,
    pub port: u16,
    pub username: String,
    pub password: String,
    pub show_password: bool,
    /// 密码为空时是否显示「留空表示不保存密码」；编辑已保存主机时不再显示
    pub password_hint: bool,
    pub groups: Vec<String>,
    pub group_input: String,
    pub drives: Vec<String>,
    pub clipboard: bool,
    pub audio: bool,
    pub order: i64,
    pub delete_armed: bool,
    pub delete_credential: bool,
    pub error: Option<String>,
}

impl HostForm {
    /// `preselect`：当前正在过滤的分组。新增主机默认勾上它，
    /// 否则在「只看某个分组」时保存的新主机会直接从列表里消失。
    pub fn new_add(drives: &[DriveInfo], next_order: i64, preselect: Option<&str>) -> Self {
        // 默认只勾 C 盘（和旧版 sync_c=true 的行为一致），但驱动器列表是本机全部
        let default_drives = if drives.iter().any(|d| d.letter == 'C') {
            vec!["C".to_string()]
        } else {
            Vec::new()
        };
        let groups = preselect
            .map(|g| g.trim())
            .filter(|g| !g.is_empty())
            .map(|g| vec![g.to_string()])
            .unwrap_or_default();
        Self {
            mode: DialogMode::Add,
            name: String::new(),
            host: String::new(),
            port: models::DEFAULT_PORT,
            username: String::new(),
            password: String::new(),
            show_password: false,
            password_hint: true,
            groups,
            group_input: String::new(),
            drives: default_drives,
            clipboard: true,
            audio: true,
            order: next_order,
            delete_armed: false,
            delete_credential: true,
            error: None,
        }
    }

    pub fn from_host(index: usize, host: &Host) -> Self {
        Self {
            mode: DialogMode::Edit(index),
            name: host.name.clone(),
            host: host.host.clone(),
            port: host.port,
            username: host.username.clone(),
            password: host.password.clone(),
            show_password: false,
            password_hint: false,
            groups: host.groups.clone(),
            group_input: String::new(),
            drives: host.drives.clone(),
            clipboard: host.clipboard,
            audio: host.audio,
            order: host.order,
            delete_armed: false,
            delete_credential: true,
            error: None,
        }
    }

    pub fn editing_index(&self) -> Option<usize> {
        match self.mode {
            DialogMode::Edit(i) => Some(i),
            DialogMode::Add => None,
        }
    }

    /// 校验并落成 Host。校验失败时把原因写进 `error`，返回 None。
    pub fn to_host(&mut self, existing_id: Option<String>) -> Option<Host> {
        let host_name = models::clean_host(&self.host);
        if let Err(e) = models::validate_host(&host_name) {
            self.error = Some(e);
            return None;
        }
        if !(1..=65535).contains(&self.port) {
            self.error = Some("端口必须在 1~65535 之间".into());
            return None;
        }

        let mut groups: Vec<String> = Vec::new();
        for group in &self.groups {
            let name = group.trim().to_string();
            if !name.is_empty() && !groups.contains(&name) {
                groups.push(name);
            }
        }
        // 用户在新分组框里打了字但没点「新建」：保存时也当作选中
        let typed = self.group_input.trim();
        if !typed.is_empty() && !groups.iter().any(|g| g == typed) {
            groups.push(typed.to_string());
        }
        let mut drives: Vec<String> = Vec::new();
        for drive in &self.drives {
            if let Some(c) = drive.trim().chars().next() {
                let c = c.to_ascii_uppercase();
                if c.is_ascii_uppercase() && !drives.contains(&c.to_string()) {
                    drives.push(c.to_string());
                }
            }
        }

        Some(Host {
            id: existing_id.unwrap_or_else(models::new_id),
            name: self.name.trim().to_string(),
            host: host_name,
            port: self.port,
            username: self.username.trim().to_string(),
            password: self.password.clone(),
            groups,
            drives,
            clipboard: self.clipboard,
            audio: self.audio,
            order: self.order,
        })
    }

    fn toggle_group(&mut self, name: &str) {
        if let Some(pos) = self.groups.iter().position(|g| g == name) {
            self.groups.remove(pos);
        } else {
            self.groups.push(name.to_string());
        }
    }

    fn toggle_drive(&mut self, letter: char) {
        let letter = letter.to_ascii_uppercase().to_string();
        if let Some(pos) = self.drives.iter().position(|d| d == &letter) {
            self.drives.remove(pos);
        } else {
            self.drives.push(letter);
        }
    }
}

pub enum HostDialogOutcome {
    None,
    Cancel,
    Save(Host),
    Delete {
        index: usize,
        host_id: String,
        name: String,
        also_credential: bool,
    },
}

pub fn draw_host_dialog(
    ctx: &egui::Context,
    pal: &Palette,
    form: &mut HostForm,
    groups: &[Group],
    drives: &[DriveInfo],
    others: &[Host],
) -> HostDialogOutcome {
    let mut outcome = HostDialogOutcome::None;
    let title = match form.mode {
        DialogMode::Add => "新增主机",
        DialogMode::Edit(_) => "编辑主机",
    };
    let editing_index = form.editing_index();
    let existing_id = editing_index.and_then(|i| others.get(i).map(|h| h.id.clone()));

    egui::Modal::new(egui::Id::new("host_dialog"))
        .frame(
            Frame::new()
                .fill(pal.panel)
                .stroke(Stroke::new(1.0_f32, pal.border))
                .corner_radius(CornerRadius::same(14))
                .inner_margin(Margin::same(20)),
        )
        .backdrop_color(Color32::from_black_alpha(110))
        .show(ctx, |ui| {
            // 窗口可能被缩到 560（最小内宽），弹窗再写死 560 就把右边「保存」裁掉了。
            let width = (ctx.screen_rect().width() - 56.0).clamp(340.0, 560.0);
            ui.set_width(width);
            ui.label(RichText::new(title).size(18.0).strong().color(pal.text));
            ui.label(
                RichText::new("端口单独填，默认 3389；分组可多选。")
                    .size(11.5)
                    .color(pal.text_dim),
            );
            ui.add_space(8.0);
            // 标题和底部按钮固定，只让中间字段滚动，小窗口下也能点到「保存」
            let extra = if form.delete_armed { 142.0 } else { 0.0 }
                + if form.error.is_some() { 58.0 } else { 0.0 };
            let body_h = (ctx.screen_rect().height() - 210.0 - extra).max(170.0);
            // 用户一改输入就清掉上一次的校验红条，不然它会一直挂到关窗。
            let mut edited = false;
            egui::ScrollArea::vertical()
                .max_height(body_h)
                .scroll_bar_visibility(
                    egui::containers::scroll_area::ScrollBarVisibility::AlwaysVisible,
                )
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    egui::Grid::new("host_form_grid")
                        .num_columns(2)
                        .spacing([12.0, 9.0])
                        .min_col_width(82.0)
                        .show(ui, |ui| {
                            grid_label(ui, pal, "备注名称");
                            edited |= text_input(
                                ui,
                                pal,
                                &mut form.name,
                                "例如：办公室主力机",
                                f32::INFINITY,
                                false,
                            )
                            .changed();
                            ui.end_row();

                            grid_label(ui, pal, "主机地址");
                            ui.horizontal(|ui| {
                                let w = (ui.available_width() - 132.0).max(120.0);
                                let host_resp = text_input(
                                    ui,
                                    pal,
                                    &mut form.host,
                                    "IP 或域名，例如 192.168.1.10",
                                    w,
                                    false,
                                );
                                // 直接粘贴「地址:端口」/「域名:端口」/「[IPv6]:端口」时自动拆开
                                let pasted = ui.input(|i| {
                                    i.events
                                        .iter()
                                        .any(|e| matches!(e, egui::Event::Paste(_)))
                                });
                                if (host_resp.changed() && pasted) || host_resp.lost_focus() {
                                    if let Some((host, port)) = split_host_port(&form.host) {
                                        form.host = host;
                                        form.port = port;
                                    }
                                }
                                edited |= host_resp.changed();
                                ui.add_space(4.0);
                                ui.label(RichText::new("端口").size(12.0).color(pal.text_dim));
                                edited |= ui
                                    .add(
                                        egui::DragValue::new(&mut form.port)
                                            .range(1..=65535)
                                            .speed(1.0),
                                    )
                                    .changed();
                            });
                            ui.end_row();

                            grid_label(ui, pal, "用户名");
                            edited |= text_input(
                                ui,
                                pal,
                                &mut form.username,
                                "Windows 登录名",
                                f32::INFINITY,
                                false,
                            )
                            .changed();
                            ui.end_row();

                            grid_label(ui, pal, "密码");
                            ui.horizontal(|ui| {
                                let w = (ui.available_width() - 58.0).max(100.0);
                                let hint = if form.password_hint {
                                    "留空 = 用空密码登录"
                                } else {
                                    ""
                                };
                                edited |= text_input(
                                    ui,
                                    pal,
                                    &mut form.password,
                                    hint,
                                    w,
                                    !form.show_password,
                                )
                                .changed();
                                let label = if form.show_password { "隐藏" } else { "显示" };
                                if ui
                                    .add(
                                        egui::Button::new(RichText::new(label).size(11.5))
                                            .fill(pal.panel)
                                            .stroke(Stroke::new(1.0_f32, pal.border))
                                            .corner_radius(CornerRadius::same(6))
                                            .min_size(Vec2::new(48.0, 27.0)),
                                    )
                                    .clicked()
                                {
                                    form.show_password = !form.show_password;
                                }
                            });
                            ui.end_row();

                            grid_label(ui, pal, "序号");
                            ui.horizontal(|ui| {
                                edited |= ui
                                    .add(
                                        egui::DragValue::new(&mut form.order)
                                            .range(-999_999..=999_999)
                                            .speed(1.0),
                                    )
                                    .changed();
                                ui.label(
                                    RichText::new("自定义排序时按从小到大排")
                                        .size(11.0)
                                        .color(pal.text_dim),
                                );
                            });
                            ui.end_row();
                        });

                    ui.add_space(8.0);
                    section_title(ui, pal, "连接选项");
                    ui.horizontal_wrapped(|ui| {
                        ui.spacing_mut().item_spacing = Vec2::new(6.0, 6.0);
                        if chip(ui, pal, form.clipboard, "剪贴板", 0.0).clicked() {
                            form.clipboard = !form.clipboard;
                            edited = true;
                        }
                        if chip(ui, pal, form.audio, "带声音", 0.0).clicked() {
                            form.audio = !form.audio;
                            edited = true;
                        }
                        ui.label(
                            RichText::new("带声音 = 远端声音在本机播放")
                                .size(10.5)
                                .color(pal.text_dim),
                        );
                    });

                    ui.add_space(8.0);
                    // 分组和磁盘并排，节省高度：小窗口下不用滚动也能看到关键项
                    ui.columns(2, |cols| {
                        section_title(&mut cols[0], pal, "分组（可多选）");
                        let mut group_names: Vec<String> =
                            groups.iter().map(|g| g.name.clone()).collect();
                        for name in &form.groups {
                            if !group_names.contains(name) {
                                group_names.push(name.clone());
                            }
                        }
                        if group_names.is_empty() {
                            cols[0].label(
                                RichText::new("还没有分组，下面新建一个")
                                    .size(11.0)
                                    .color(pal.text_dim),
                            );
                        } else {
                            cols[0].horizontal_wrapped(|ui| {
                                ui.spacing_mut().item_spacing = Vec2::new(6.0, 6.0);
                                for name in &group_names {
                                    let selected = form.groups.iter().any(|g| g == name);
                                    if chip(ui, pal, selected, name, 0.0).clicked() {
                                        form.toggle_group(name);
                                        edited = true;
                                    }
                                }
                            });
                        }
                        cols[0].horizontal(|ui| {
                            let response = text_input(
                                ui,
                                pal,
                                &mut form.group_input,
                                "新分组名",
                                (ui.available_width() - 108.0).max(80.0),
                                false,
                            );
                            edited |= response.changed();
                            let add = ui
                                .add(
                                    egui::Button::new(
                                        RichText::new("+ 新建").size(11.5).color(Color32::WHITE),
                                    )
                                    .fill(pal.accent)
                                    .corner_radius(CornerRadius::same(7))
                                    .min_size(Vec2::new(0.0, 28.0)),
                                )
                                .clicked()
                                || (response.lost_focus()
                                    && ui.input(|i| i.key_pressed(egui::Key::Enter)));
                            if add {
                                let name = form.group_input.trim().to_string();
                                if !name.is_empty() {
                                    if !form.groups.contains(&name) {
                                        form.groups.push(name);
                                    }
                                    form.group_input.clear();
                                }
                            }
                        });

                        section_title(&mut cols[1], pal, "磁盘映射（本机全部驱动器）");
                        cols[1].horizontal_wrapped(|ui| {
                            ui.spacing_mut().item_spacing = Vec2::new(6.0, 6.0);
                            if drives.is_empty() {
                                ui.label(
                                    RichText::new("没有检测到可用的驱动器")
                                        .size(11.0)
                                        .color(pal.text_dim),
                                );
                            }
                            for drive in drives {
                                let letter = drive.letter.to_string();
                                let selected = form.drives.iter().any(|d| d == &letter);
                                if chip(ui, pal, selected, &drive.caption(), 104.0)
                                    .on_hover_text(drive.kind.label())
                                    .clicked()
                                {
                                    form.toggle_drive(drive.letter);
                                    edited = true;
                                }
                            }
                            // 已选但当前不存在的盘符也要显示出来，方便取消
                            for letter in form.drives.clone() {
                                if drives.iter().any(|d| d.letter.to_string() == letter) {
                                    continue;
                                }
                                let label = format!("{letter}:（不存在）");
                                if chip(ui, pal, true, &label, 104.0).clicked() {
                                    form.toggle_drive(letter.chars().next().unwrap_or('C'));
                                    edited = true;
                                }
                            }
                        });
                        cols[1].horizontal(|ui| {
                            if !drives.is_empty() && small_link(ui, pal, "全选").clicked() {
                                for drive in drives {
                                    let letter = drive.letter.to_string();
                                    if !form.drives.contains(&letter) {
                                        form.drives.push(letter);
                                    }
                                }
                                edited = true;
                            }
                            if !form.drives.is_empty() && small_link(ui, pal, "清空").clicked() {
                                form.drives.clear();
                                edited = true;
                            }
                        });
                    });

                    // 同地址不同端口：Windows 凭据按主机名存，提示用户
                    let warning = if !form.host.trim().is_empty() {
                        others.iter().enumerate().find(|(i, other)| {
                            Some(*i) != editing_index
                                && other
                                    .host
                                    .eq_ignore_ascii_case(form.host.trim())
                                && other.port != form.port
                                && !other.password.is_empty()
                                && other.password != form.password
                        })
                    } else {
                        None
                    };
                    if let Some((_, other)) = warning {
                        ui.add_space(8.0);
                        Frame::new()
                            .fill(pal.danger_soft)
                            .corner_radius(CornerRadius::same(7))
                            .inner_margin(Margin::same(8))
                            .show(ui, |ui| {
                                ui.label(
                                    RichText::new(format!(
                                        "同一地址的另一台主机使用端口 {}，密码不同。Windows 凭据只按主机名保存，\
                                         断开重连时可能拿错密码——本程序每次连接前都会重写凭据。",
                                        other.port
                                    ))
                                    .size(11.0)
                                    .color(pal.danger),
                                );
                            });
                    }

                });

            if let Some(err) = &form.error {
                        ui.add_space(8.0);
                        Frame::new()
                            .fill(pal.danger_soft)
                            .corner_radius(CornerRadius::same(7))
                            .inner_margin(Margin::same(8))
                            .show(ui, |ui| {
                                ui.label(RichText::new(err).size(11.5).color(pal.danger));
                            });
                    }

                    ui.add_space(10.0);

                    // 删除确认放在编辑界面里，列表上不再放删除按钮
                    if let Some(index) = editing_index {
                        if form.delete_armed {
                            Frame::new()
                                .fill(pal.danger_soft)
                                .stroke(Stroke::new(1.0_f32, pal.danger))
                                .corner_radius(CornerRadius::same(8))
                                .inner_margin(Margin::same(10))
                                .show(ui, |ui| {
                                    ui.label(
                                        RichText::new(format!(
                                            "确定删除「{}」？删除后不可恢复。",
                                            form.name.trim()
                                        ))
                                        .size(12.0)
                                        .color(pal.danger),
                                    );
                                    ui.checkbox(
                                        &mut form.delete_credential,
                                        RichText::new("同时清理 Windows 凭据").size(12.0),
                                    );
                                    ui.horizontal(|ui| {
                                        if ui
                                            .add(
                                                egui::Button::new(
                                                    RichText::new("确认删除")
                                                        .size(12.5)
                                                        .color(Color32::WHITE),
                                                )
                                                .fill(pal.danger)
                                                .corner_radius(CornerRadius::same(7))
                                                .min_size(Vec2::new(86.0, 32.0)),
                                            )
                                            .clicked()
                                        {
                                            outcome = HostDialogOutcome::Delete {
                                                index,
                                                host_id: existing_id.clone().unwrap_or_default(),
                                                name: form.name.trim().to_string(),
                                                also_credential: form.delete_credential,
                                            };
                                        }
                                        if ui
                                            .add(
                                                egui::Button::new(
                                                    RichText::new("取消")
                                                        .size(12.5)
                                                        .color(pal.text_dim),
                                                )
                                                .fill(pal.panel)
                                                .stroke(Stroke::new(1.0_f32, pal.border))
                                                .corner_radius(CornerRadius::same(7))
                                                .min_size(Vec2::new(64.0, 32.0)),
                                            )
                                            .clicked()
                                        {
                                            form.delete_armed = false;
                                        }
                                    });
                                });
                            ui.add_space(8.0);
                        }
                    }

                    if edited && form.error.is_some() {
                        form.error = None;
                    }

                    ui.horizontal(|ui| {
                        if editing_index.is_some() && !form.delete_armed {
                            if ui
                                .add(
                                    egui::Button::new(
                                        RichText::new("删除此主机…").size(12.0).color(pal.danger),
                                    )
                                    .fill(Color32::TRANSPARENT)
                                    .stroke(Stroke::new(1.0_f32, pal.danger))
                                    .corner_radius(CornerRadius::same(7))
                                    .min_size(Vec2::new(104.0, 34.0)),
                                )
                                .clicked()
                            {
                                form.delete_armed = true;
                            }
                        }

                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            if ui
                                .add(
                                    egui::Button::new(
                                        RichText::new("保存")
                                            .size(13.5)
                                            .color(Color32::WHITE)
                                            .strong(),
                                    )
                                    .fill(pal.accent)
                                    .corner_radius(CornerRadius::same(9))
                                    .min_size(Vec2::new(84.0, 36.0)),
                                )
                                .clicked()
                            {
                                if let Some(host) = form.to_host(existing_id.clone()) {
                                    if let Some(dup) =
                                        find_duplicate(others, editing_index, &host)
                                    {
                                        form.error = Some(format!(
                                            "同地址同端口的主机已经存在：{}",
                                            dup.display_name()
                                        ));
                                    } else {
                                        outcome = HostDialogOutcome::Save(host);
                                    }
                                }
                            }
                            if ui
                                .add(
                                    egui::Button::new(
                                        RichText::new("取消").size(13.0).color(pal.text_dim),
                                    )
                                    .fill(pal.panel)
                                    .stroke(Stroke::new(1.0_f32, pal.border))
                                    .corner_radius(CornerRadius::same(9))
                                    .min_size(Vec2::new(76.0, 36.0)),
                                )
                                .clicked()
                            {
                                outcome = HostDialogOutcome::Cancel;
                            }
                        });
                    });
        });

    outcome
}

// ---------------------------------------------------------------------------
// 设置（含分组编辑）
// ---------------------------------------------------------------------------

pub struct SettingsDraft {
    pub settings: Settings,
    pub groups: Vec<Group>,
    pub new_group: String,
    pub error: Option<String>,
    /// 待确认删除的分组：(下标, 当时的分组名)。两个都对上才删，避免重名删错行。
    pub delete_confirm: Option<(usize, String)>,
    /// 保存时顺手请求关闭 Windows 的 RDP 安全警告
    pub fix_rdp_warning: bool,
    /// 打开设置时读一次 HKLM，别每帧读注册表
    pub warning_suppressed: bool,
}

impl SettingsDraft {
    pub fn new(store: &Store) -> Self {
        Self {
            settings: store.settings.clone(),
            groups: store.groups.clone(),
            new_group: String::new(),
            error: None,
            delete_confirm: None,
            fix_rdp_warning: false,
            warning_suppressed: rdp_warning::is_suppressed(),
        }
    }

    /// 把草稿写回 store：改组名要同步改主机里的分组引用，删分组要把主机上的引用摘掉。
    ///
    /// 返回「旧分组名 -> 新分组名」的映射，调用方用它同步当前的分组过滤。
    pub fn apply(&self, store: &mut Store) -> Result<HashMap<String, String>, String> {
        let mut names: Vec<String> = Vec::new();
        let mut seen = HashSet::new();
        for group in &self.groups {
            let name = group.name.trim().to_string();
            if name.is_empty() {
                return Err("分组名不能为空".into());
            }
            if !seen.insert(name.clone()) {
                return Err(format!("分组名重复：{name}"));
            }
            names.push(name);
        }

        // 旧名 -> 新名（用 orig 识别改名）
        let mut rename: HashMap<String, String> = HashMap::new();
        for group in &self.groups {
            let old_name = group.orig.trim();
            let new_name = group.name.trim();
            if !old_name.is_empty() && old_name != new_name {
                rename.insert(old_name.to_string(), new_name.to_string());
            }
        }
        let keep: HashSet<String> = names.iter().cloned().collect();

        for host in &mut store.hosts {
            let mut updated: Vec<String> = Vec::new();
            for group in &host.groups {
                let mapped = rename.get(group).map(|s| s.as_str()).unwrap_or(group);
                if keep.contains(mapped) && !updated.iter().any(|g| g == mapped) {
                    updated.push(mapped.to_string());
                }
            }
            host.groups = updated;
        }

        let mut default_group = self.settings.default_group.trim().to_string();
        if let Some(new_name) = rename.get(&default_group) {
            default_group = new_name.clone();
        }
        // 真被删掉的分组不在这里清：交给 store.normalize() 统一收尾，
        // 免得「名字只差个空格」被当成删除，把默认分组静默清空。

        store.settings = self.settings.clone();
        store.settings.default_group = default_group;

        store.groups = names
            .into_iter()
            .enumerate()
            .map(|(i, name)| Group::new(name, i as i64))
            .collect();

        store.normalize();
        Ok(rename)
    }
}

pub enum SettingsOutcome {
    None,
    Cancel,
    Save,
}

pub fn draw_settings_dialog(
    ctx: &egui::Context,
    pal: &Palette,
    draft: &mut SettingsDraft,
) -> SettingsOutcome {
    let mut outcome = SettingsOutcome::None;

    egui::Modal::new(egui::Id::new("settings_dialog"))
        .frame(
            Frame::new()
                .fill(pal.panel)
                .stroke(Stroke::new(1.0_f32, pal.border))
                .corner_radius(CornerRadius::same(14))
                .inner_margin(Margin::same(20)),
        )
        .backdrop_color(Color32::from_black_alpha(110))
        .show(ctx, |ui| {
            let width = (ctx.screen_rect().width() - 56.0).clamp(340.0, 620.0);
            ui.set_width(width);
            ui.label(RichText::new("设置").size(18.0).strong().color(pal.text));
            ui.label(
                RichText::new("所有数据（含密码、设置）都加密存在 exe 自己身上，不生成 json 文件。")
                    .size(11.5)
                    .color(pal.text_dim),
            );
            ui.label(
                RichText::new("常规项在上；分组编辑在最下面，滚动到底即可。")
                    .size(11.0)
                    .color(pal.text_dim),
            );
            ui.add_space(6.0);
            let extra = if draft.error.is_some() { 58.0 } else { 0.0 };
            let body_h = (ctx.screen_rect().height() - 170.0 - extra).max(200.0);
            egui::ScrollArea::vertical()
                .max_height(body_h)
                .scroll_bar_visibility(
                    egui::containers::scroll_area::ScrollBarVisibility::AlwaysVisible,
                )
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    section_title(ui, pal, "Windows 兼容");
                    let suppressed = draft.warning_suppressed;
                    ui.label(
                        RichText::new(if suppressed {
                            "已关闭（连接时不再弹安全警告）"
                        } else {
                            "未关闭（新版 Windows 会弹「未知的远程连接」）"
                        })
                        .size(11.5)
                        .color(if suppressed { pal.online } else { pal.danger }),
                    );
                    if suppressed {
                        ui.label(
                            RichText::new("连接时不会再弹「未知的远程连接」，剪贴板/磁盘重定向按本软件设置生效。")
                                .size(11.0)
                                .color(pal.text_dim),
                        );
                    } else {
                        ui.label(
                            RichText::new(
                                "Windows 2026 更新后，打开带剪贴板/磁盘重定向的 .rdp 会弹「未知的远程连接」\
                                 并默认取消这些重定向。点下面按钮授权一次即可永久关闭。",
                            )
                            .size(11.0)
                            .color(pal.text_dim),
                        );
                        if ui
                            .add(
                                egui::Button::new(
                                    RichText::new("关闭 RDP 安全警告（管理员一次）")
                                        .size(12.0)
                                        .color(Color32::WHITE),
                                )
                                .fill(pal.accent)
                                .corner_radius(CornerRadius::same(7))
                                .min_size(Vec2::new(0.0, 31.0)),
                            )
                            .clicked()
                        {
                            draft.fix_rdp_warning = true;
                        }
                        if draft.fix_rdp_warning {
                            ui.label(
                                RichText::new("点「保存」后会弹出一次管理员授权，点「是」即可。")
                                    .size(11.0)
                                    .color(pal.accent),
                            );
                        }
                    }
                    if !draft.settings.rdp_warning_hint {
                        if small_link(ui, pal, "重新显示顶部提示").clicked() {
                            draft.settings.rdp_warning_hint = true;
                        }
                    }

                    ui.add_space(12.0);
                    section_title(ui, pal, "默认排序规则");
                    segmented(
                        ui,
                        pal,
                        &mut draft.settings.sort,
                        &[
                            (SortMode::Name, "名称"),
                            (SortMode::Address, "主机地址"),
                            (SortMode::Custom, "自定义（序号）"),
                        ],
                    );
                    ui.label(
                        RichText::new("自定义排序按编辑界面里的「序号」从小到大排。")
                            .size(11.0)
                            .color(pal.text_dim),
                    );

                    ui.add_space(12.0);
                    section_title(ui, pal, "启动行为");
                    ui.horizontal_wrapped(|ui| {
                        ui.spacing_mut().item_spacing = Vec2::new(6.0, 6.0);
                        if chip(ui, pal, draft.settings.probe, "探测可用性", 0.0).clicked() {
                            draft.settings.probe = !draft.settings.probe;
                        }
                        if chip(ui, pal, draft.settings.show_search, "显示快速搜索栏", 0.0).clicked()
                        {
                            draft.settings.show_search = !draft.settings.show_search;
                        }
                        if chip(
                            ui,
                            pal,
                            draft.settings.close_to_tray,
                            "关闭后保持后台（托盘）",
                            0.0,
                        )
                        .on_hover_text("关闭主界面时不退出，任务栏通知区域保留小图标")
                        .clicked()
                        {
                            draft.settings.close_to_tray = !draft.settings.close_to_tray;
                        }
                    });
                    if draft.settings.close_to_tray {
                        ui.label(
                            RichText::new(
                                "开启后关闭窗口不会退出：右键任务栏通知区域的小图标 → 直接连主机 / 打开主界面 / 退出。",
                            )
                            .size(11.0)
                            .color(pal.text_dim),
                        );
                    }

                    ui.add_space(12.0);
                    section_title(ui, pal, "显示方式");
                    segmented(
                        ui,
                        pal,
                        &mut draft.settings.view,
                        &[(ViewMode::List, "列表"), (ViewMode::Grid, "方块")],
                    );
                    ui.label(
                        RichText::new("方块模式下每台主机是一张大卡片，适合鼠标点选。")
                            .size(11.0)
                            .color(pal.text_dim),
                    );
                    ui.add_space(6.0);
                    ui.horizontal(|ui| {
                        ui.label(RichText::new("主题").size(12.0).color(pal.text_dim));
                        segmented(
                            ui,
                            pal,
                            &mut draft.settings.dark,
                            &[(false, "浅色"), (true, "深色")],
                        );
                    });

                    ui.add_space(12.0);
                    section_title(ui, pal, "默认分组（启动时先展示）");
                    let group_names: Vec<String> =
                        draft.groups.iter().map(|g| g.name.clone()).collect();
                    let selected_text = if draft.settings.default_group.trim().is_empty() {
                        "不指定（显示全部）".to_string()
                    } else {
                        draft.settings.default_group.clone()
                    };
                    egui::ComboBox::from_id_salt("default_group_combo")
                        .width(260.0)
                        .selected_text(selected_text)
                        .show_ui(ui, |ui| {
                            ui.selectable_value(
                                &mut draft.settings.default_group,
                                String::new(),
                                "不指定（显示全部）",
                            );
                            for name in &group_names {
                                ui.selectable_value(
                                    &mut draft.settings.default_group,
                                    name.clone(),
                                    name,
                                );
                            }
                        });

                    ui.add_space(16.0);
                    ui.separator();
                    ui.add_space(10.0);
                    section_title(ui, pal, "分组编辑");
                    ui.label(
                        RichText::new("可改名、调顺序、删除；勾「默认」的分组启动时优先展示。主机可以同时属于多个分组。")
                            .size(11.0)
                            .color(pal.text_dim),
                    );
                    ui.add_space(6.0);

                    let mut swap: Option<(usize, usize)> = None;
                    let mut remove: Option<usize> = None;
                    for i in 0..draft.groups.len() {
                        ui.horizontal(|ui| {
                            let default_now = draft.settings.default_group == draft.groups[i].name;
                            let radio = ui.radio(default_now, RichText::new("默认").size(11.5));
                            if radio.clicked() {
                                draft.settings.default_group = draft.groups[i].name.clone();
                            }
                            radio.on_hover_text("启动时先展示这个分组");

                            let old_name = draft.groups[i].name.clone();
                            text_input(
                                ui,
                                pal,
                                &mut draft.groups[i].name,
                                "",
                                250.0,
                                false,
                            );
                            if draft.settings.default_group == old_name
                                && !draft.groups[i].name.trim().is_empty()
                            {
                                draft.settings.default_group = draft.groups[i].name.clone();
                            }

                            ui.label(
                                RichText::new(format!("#{}", i + 1))
                                    .size(11.0)
                                    .color(pal.text_dim),
                            );

                            let up = ui
                                .add_enabled(
                                    i > 0,
                                    egui::Button::new(RichText::new("↑").size(13.0))
                                        .fill(pal.panel)
                                        .stroke(Stroke::new(1.0_f32, pal.border))
                                        .corner_radius(CornerRadius::same(6))
                                        .min_size(Vec2::new(30.0, 28.0)),
                                )
                                .on_hover_text("上移");
                            if up.clicked() {
                                swap = Some((i, i - 1));
                            }

                            let down = ui
                                .add_enabled(
                                    i + 1 < draft.groups.len(),
                                    egui::Button::new(RichText::new("↓").size(13.0))
                                        .fill(pal.panel)
                                        .stroke(Stroke::new(1.0_f32, pal.border))
                                        .corner_radius(CornerRadius::same(6))
                                        .min_size(Vec2::new(30.0, 28.0)),
                                )
                                .on_hover_text("下移");
                            if down.clicked() {
                                swap = Some((i, i + 1));
                            }

                            if ui
                                .add(
                                    egui::Button::new(
                                        RichText::new("删除").size(11.5).color(pal.danger),
                                    )
                                    .fill(Color32::TRANSPARENT)
                                    .stroke(Stroke::new(1.0_f32, pal.danger))
                                    .corner_radius(CornerRadius::same(6))
                                    .min_size(Vec2::new(52.0, 28.0)),
                                )
                                .clicked()
                            {
                                draft.delete_confirm =
                                    Some((i, draft.groups[i].name.clone()));
                            }
                        });
                    }
                    if let Some((a, b)) = swap {
                        draft.groups.swap(a, b);
                        // 行序变了，之前记下的下标不再可信
                        draft.delete_confirm = None;
                    }

                    if let Some((index, name)) = draft.delete_confirm.clone() {
                        // 名字被改过或者这一行已经不是当初点的那行了：作废，别删错分组
                        let still_same = draft
                            .groups
                            .get(index)
                            .is_some_and(|g| g.name == name);
                        if !still_same {
                            draft.delete_confirm = None;
                        } else {
                            Frame::new()
                                .fill(pal.danger_soft)
                                .stroke(Stroke::new(1.0_f32, pal.danger))
                                .corner_radius(CornerRadius::same(8))
                                .inner_margin(Margin::same(10))
                                .show(ui, |ui| {
                                    ui.label(
                                        RichText::new(format!(
                                            "删除分组「{name}」？主机不会被删除，只会从这个分组里移出。"
                                        ))
                                        .size(12.0)
                                        .color(pal.danger),
                                    );
                                    ui.horizontal(|ui| {
                                        if ui
                                            .add(
                                                egui::Button::new(
                                                    RichText::new("确认删除")
                                                        .size(12.5)
                                                        .color(Color32::WHITE),
                                                )
                                                .fill(pal.danger)
                                                .corner_radius(CornerRadius::same(7))
                                                .min_size(Vec2::new(86.0, 32.0)),
                                            )
                                            .clicked()
                                        {
                                            remove = Some(index);
                                        }
                                        if ui
                                            .add(
                                                egui::Button::new(
                                                    RichText::new("取消")
                                                        .size(12.5)
                                                        .color(pal.text_dim),
                                                )
                                                .fill(pal.panel)
                                                .stroke(Stroke::new(1.0_f32, pal.border))
                                                .corner_radius(CornerRadius::same(7))
                                                .min_size(Vec2::new(64.0, 32.0)),
                                            )
                                            .clicked()
                                        {
                                            draft.delete_confirm = None;
                                        }
                                    });
                                });
                        }
                    }
                    if let Some(index) = remove {
                        let removed = draft.groups.remove(index);
                        if draft.settings.default_group == removed.name {
                            draft.settings.default_group.clear();
                        }
                        draft.delete_confirm = None;
                    }

                    ui.add_space(8.0);
                    ui.horizontal(|ui| {
                        let response = text_input(
                            ui,
                            pal,
                            &mut draft.new_group,
                            "新分组名",
                            220.0,
                            false,
                        );
                        let add = ui
                            .add(
                                egui::Button::new(
                                    RichText::new("+ 新建分组")
                                        .size(12.0)
                                        .color(Color32::WHITE),
                                )
                                .fill(pal.accent)
                                .corner_radius(CornerRadius::same(7))
                                .min_size(Vec2::new(0.0, 30.0)),
                            )
                            .clicked()
                            || (response.lost_focus()
                                && ui.input(|i| i.key_pressed(egui::Key::Enter)));
                        if add {
                            let name = draft.new_group.trim().to_string();
                            if name.is_empty() {
                                draft.error = Some("分组名不能为空".into());
                            } else if draft.groups.iter().any(|g| g.name.trim() == name) {
                                draft.error = Some(format!("分组名重复：{name}"));
                            } else {
                                let order = draft.groups.len() as i64;
                                draft.groups.push(Group {
                                    name,
                                    order,
                                    orig: String::new(),
                                });
                                draft.new_group.clear();
                                draft.error = None;
                            }
                        }
                    });

                });

            if let Some(err) = &draft.error {
                        ui.add_space(8.0);
                        Frame::new()
                            .fill(pal.danger_soft)
                            .corner_radius(CornerRadius::same(7))
                            .inner_margin(Margin::same(8))
                            .show(ui, |ui| {
                                ui.label(RichText::new(err).size(11.5).color(pal.danger));
                            });
                    }

                    ui.add_space(12.0);
                    ui.horizontal(|ui| {
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            if ui
                                .add(
                                    egui::Button::new(
                                        RichText::new("保存")
                                            .size(13.5)
                                            .color(Color32::WHITE)
                                            .strong(),
                                    )
                                    .fill(pal.accent)
                                    .corner_radius(CornerRadius::same(9))
                                    .min_size(Vec2::new(84.0, 36.0)),
                                )
                                .clicked()
                            {
                                outcome = SettingsOutcome::Save;
                            }
                            if ui
                                .add(
                                    egui::Button::new(
                                        RichText::new("取消").size(13.0).color(pal.text_dim),
                                    )
                                    .fill(pal.panel)
                                    .stroke(Stroke::new(1.0_f32, pal.border))
                                    .corner_radius(CornerRadius::same(9))
                                    .min_size(Vec2::new(76.0, 36.0)),
                                )
                                .clicked()
                            {
                                outcome = SettingsOutcome::Cancel;
                            }
                        });
                    });
        });

    outcome
}

// ---------------------------------------------------------------------------
// 关于
// ---------------------------------------------------------------------------

/// 右下角版本号点开的「关于」弹窗。返回 true 表示关闭。
pub fn draw_about_dialog(ctx: &egui::Context, pal: &Palette, version: &str) -> bool {
    let mut close = false;
    egui::Modal::new(egui::Id::new("about_dialog"))
        .frame(
            Frame::new()
                .fill(pal.panel)
                .stroke(Stroke::new(1.0_f32, pal.border))
                .corner_radius(CornerRadius::same(14))
                .inner_margin(Margin::same(22)),
        )
        .backdrop_color(Color32::from_black_alpha(110))
        .show(ctx, |ui| {
            ui.set_width(440.0);
            ui.label(
                RichText::new("RDP 启动器")
                    .size(21.0)
                    .strong()
                    .color(pal.text),
            );
            ui.label(
                RichText::new(format!("版本 v{version}"))
                    .size(12.0)
                    .color(pal.text_dim),
            );
            ui.add_space(10.0);
            ui.label(
                RichText::new("轻量级 Windows 远程桌面启动器")
                    .size(14.0)
                    .color(pal.text),
            );
            ui.add_space(4.0);
            for line in [
                "单文件绿色版：主机、密码、分组、设置都加密存在 exe 里，不生成 json。",
                "支持多分组、端口单独配置、磁盘 / 声音 / 剪贴板映射、序号排序。",
                "支持列表 / 方块显示、关闭后保持后台、托盘右键快速连接。",
                "内置 Windows 2026 RDP 安全警告的关闭开关。",
            ] {
                ui.label(RichText::new(line).size(11.5).color(pal.text_dim));
            }

            ui.add_space(12.0);
            ui.label(
                RichText::new("QLing 制作")
                    .size(12.5)
                    .color(pal.text),
            );

            ui.add_space(14.0);
            ui.horizontal(|ui| {
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if ui
                        .add(
                            egui::Button::new(
                                RichText::new("关闭")
                                    .size(13.0)
                                    .color(Color32::WHITE),
                            )
                            .fill(pal.accent)
                            .corner_radius(CornerRadius::same(9))
                            .min_size(Vec2::new(84.0, 34.0)),
                        )
                        .clicked()
                    {
                        close = true;
                    }
                });
            });
        });
    close
}

// ---------------------------------------------------------------------------
// 小工具
// ---------------------------------------------------------------------------

fn grid_label(ui: &mut egui::Ui, pal: &Palette, text: &str) {
    ui.add_sized(
        Vec2::new(82.0, 28.0),
        egui::Label::new(RichText::new(text).size(12.0).color(pal.text_dim)),
    );
}

fn section_title(ui: &mut egui::Ui, pal: &Palette, text: &str) {
    ui.label(
        RichText::new(text)
            .size(12.5)
            .strong()
            .color(pal.accent),
    );
}

fn text_input(
    ui: &mut egui::Ui,
    pal: &Palette,
    value: &mut String,
    hint: &str,
    width: f32,
    password: bool,
) -> egui::Response {
    let mut edit = egui::TextEdit::singleline(value)
        .desired_width(width)
        .margin(Vec2::new(8.0, 6.0));
    if password {
        edit = edit.password(true);
    }
    let response = ui.add(edit);
    // egui 自带的 hint 颜色跟正文走，偏黑；这里自己画一层淡灰占位词。
    // 聚焦时（光标就在最左边）不画，免得盖住闪烁的光标。
    if value.is_empty() && !hint.is_empty() && !response.has_focus() {
        // 注意是「与当前裁剪区相交」，不是替换：直接 with_clip_rect(rect) 会让
        // 占位词画到滚动区外面去。
        let clip = response.rect.shrink(2.0).intersect(ui.clip_rect());
        if clip.is_positive() {
            let painter = ui.painter().with_clip_rect(clip);
            painter.text(
                egui::pos2(response.rect.left() + 8.0, response.rect.center().y),
                egui::Align2::LEFT_CENTER,
                hint,
                egui::FontId::proportional(14.0),
                pal.hint,
            );
        }
    }
    response
}

/// 同地址 + 同端口已经存在（地址大小写不敏感），返回那台重复的主机。
fn find_duplicate<'a>(
    others: &'a [Host],
    editing_index: Option<usize>,
    host: &Host,
) -> Option<&'a Host> {
    others.iter().enumerate().find_map(|(i, other)| {
        (Some(i) != editing_index
            && other.host.eq_ignore_ascii_case(host.host.trim())
            && other.port == host.port)
            .then_some(other)
    })
}

/// 把粘贴进来的 `host:port` / `[ipv6]:port` / `域名:端口` 拆成主机和端口。
/// 裸 IPv6（多个冒号）不拆。
fn split_host_port(text: &str) -> Option<(String, u16)> {
    let text = text.trim();
    if text.is_empty() {
        return None;
    }
    if let Some(rest) = text.strip_prefix('[') {
        let close = rest.find(']')?;
        let host = &rest[..close];
        let after = &rest[close + 1..];
        let port = parse_port_digits(after.strip_prefix(':')?)?;
        if host.is_empty() {
            return None;
        }
        return Some((host.to_string(), port));
    }
    if text.matches(':').count() != 1 {
        return None;
    }
    let (host, port) = text.split_once(':')?;
    let host = host.trim();
    let port = parse_port_digits(port)?;
    if host.is_empty() {
        return None;
    }
    // `fe80:1` 这种漏了一个冒号的 IPv6 长得就像「host:port」，
    // 静默拆成 host=fe80 / port=1 会让人一头雾水：整段都是十六进制且端口很小就不拆。
    if port < 1024 && host.chars().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    Some((host.to_string(), port))
}

fn parse_port_digits(text: &str) -> Option<u16> {
    let text = text.trim();
    if text.is_empty() || !text.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    let port: u32 = text.parse().ok()?;
    (1..=65535).contains(&port).then_some(port as u16)
}

fn small_link(ui: &mut egui::Ui, pal: &Palette, text: &str) -> egui::Response {
    ui.add(
        egui::Button::new(RichText::new(text).size(11.0).color(pal.accent))
            .fill(Color32::TRANSPARENT)
            .stroke(Stroke::NONE)
            .min_size(Vec2::new(0.0, 22.0)),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store_with_two_hosts() -> Store {
        let mut store = Store::default();
        store.groups = vec![Group::new("公司", 0), Group::new("测试", 1)];
        store.hosts = vec![
            Host {
                id: "a".into(),
                name: "A".into(),
                host: "10.0.0.1".into(),
                port: 3389,
                groups: vec!["公司".into()],
                ..Default::default()
            },
            Host {
                id: "b".into(),
                name: "B".into(),
                host: "10.0.0.2".into(),
                port: 3389,
                groups: vec!["公司".into(), "测试".into()],
                ..Default::default()
            },
        ];
        store
    }

    #[test]
    fn form_rejects_port_inside_host() {
        let mut form = HostForm::new_add(&[], 0, None);
        form.host = "1.2.3.4:3390".into();
        assert!(form.to_host(None).is_none());
        assert!(form.error.is_some());
    }

    #[test]
    fn form_keeps_multiple_groups_and_drives() {
        let mut form = HostForm::new_add(&[], 0, None);
        form.name = "办公".into();
        form.host = "192.168.1.10".into();
        form.port = 33890;
        form.groups = vec!["公司".into(), "测试".into()];
        form.drives = vec!["C".into(), "D".into()];
        form.audio = false;
        form.order = 7;
        let host = form.to_host(None).unwrap();
        assert_eq!(host.port, 33890);
        assert_eq!(host.groups, vec!["公司".to_string(), "测试".to_string()]);
        assert_eq!(host.drives, vec!["C".to_string(), "D".to_string()]);
        assert!(!host.audio);
        assert_eq!(host.order, 7);
    }

    #[test]
    fn form_supports_ipv6_and_domain_with_port() {
        let mut form = HostForm::new_add(&[], 0, None);
        form.host = "fe80::1".into();
        form.port = 3390;
        assert_eq!(form.to_host(None).unwrap().authority(), "[fe80::1]:3390");

        form.host = "rdp.example.com".into();
        form.port = 13389;
        assert_eq!(
            form.to_host(None).unwrap().authority(),
            "rdp.example.com:13389"
        );
    }

    #[test]
    fn typed_group_is_saved_even_without_add_button() {
        let mut form = HostForm::new_add(&[], 0, None);
        form.host = "10.0.0.1".into();
        form.group_input = "新分组".into();
        let host = form.to_host(None).unwrap();
        assert_eq!(host.groups, vec!["新分组".to_string()]);
    }

    #[test]
    fn splits_pasted_host_and_port() {
        assert_eq!(
            split_host_port("192.168.1.10:33890"),
            Some(("192.168.1.10".to_string(), 33890))
        );
        assert_eq!(
            split_host_port("rdp.example.com:13389"),
            Some(("rdp.example.com".to_string(), 13389))
        );
        assert_eq!(
            split_host_port("[fe80::1]:3390"),
            Some(("fe80::1".to_string(), 3390))
        );
        // 裸 IPv6 不拆
        assert_eq!(split_host_port("fe80::1"), None);
        // 漏了一个冒号的 IPv6 不当成 host:port 静默改写
        assert_eq!(split_host_port("fe80:1"), None);
        // 真正的地址 + 端口照拆（哪怕端口很小）
        assert_eq!(
            split_host_port("192.168.1.10:80"),
            Some(("192.168.1.10".to_string(), 80))
        );
        // 非数字端口 / 越界端口不拆
        assert_eq!(split_host_port("host:abc"), None);
        assert_eq!(split_host_port("host:0"), None);
        assert_eq!(split_host_port("host:65536"), None);
    }

    #[test]
    fn password_hint_only_for_new_host() {
        let add = HostForm::new_add(&[], 0, None);
        assert!(add.password_hint);
        let saved = HostForm::from_host(
            0,
            &Host {
                id: "a".into(),
                host: "10.0.0.1".into(),
                ..Default::default()
            },
        );
        assert!(!saved.password_hint);
    }

    #[test]
    fn settings_apply_renames_and_removes_group_refs() {
        let mut store = store_with_two_hosts();
        store.settings.default_group = "公司".into();
        let mut draft = SettingsDraft::new(&store);
        draft.groups[0].name = "总部".into();
        draft.groups.remove(1); // 删掉「测试」
        draft.apply(&mut store).unwrap();

        assert_eq!(store.groups.len(), 1);
        assert_eq!(store.groups[0].name, "总部");
        assert_eq!(store.hosts[0].groups, vec!["总部".to_string()]);
        assert_eq!(store.hosts[1].groups, vec!["总部".to_string()]);
        assert_eq!(store.settings.default_group, "总部");
    }

    #[test]
    fn new_add_preselects_current_group() {
        let form = HostForm::new_add(&[], 3, Some("公司"));
        assert_eq!(form.groups, vec!["公司".to_string()]);
        assert_eq!(form.order, 3);

        let blank = HostForm::new_add(&[], 0, Some("   "));
        assert!(blank.groups.is_empty());
    }

    #[test]
    fn find_duplicate_matches_address_and_port_ignoring_case() {
        let existing = Host {
            id: "a".into(),
            host: "Office-PC".into(),
            port: 3389,
            ..Default::default()
        };
        let mut candidate = Host {
            id: "b".into(),
            host: "office-pc".into(),
            port: 3389,
            ..Default::default()
        };
        assert!(find_duplicate(std::slice::from_ref(&existing), None, &candidate).is_some());
        candidate.port = 3390;
        assert!(find_duplicate(std::slice::from_ref(&existing), None, &candidate).is_none());
        candidate.port = 3389;
        // 编辑自己不算重复
        assert!(find_duplicate(std::slice::from_ref(&existing), Some(0), &candidate).is_none());
    }

    #[test]
    fn settings_apply_returns_rename_map() {
        let mut store = store_with_two_hosts();
        let mut draft = SettingsDraft::new(&store);
        draft.groups[0].name = "总部".into();
        let rename = draft.apply(&mut store).unwrap();
        assert_eq!(rename.get("公司").map(String::as_str), Some("总部"));
    }

    #[test]
    fn settings_rejects_duplicate_group_names() {
        let store = store_with_two_hosts();
        let mut draft = SettingsDraft::new(&store);
        draft.groups[1].name = "公司".into();
        let mut target = store.clone();
        assert!(draft.apply(&mut target).is_err());
    }
}

