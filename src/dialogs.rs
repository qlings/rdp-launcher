//! 两个模态弹窗：新增/编辑主机、设置（含分组编辑）。

use std::collections::{HashMap, HashSet};

use eframe::egui;
use egui::{
    Align, Color32, CornerRadius, Frame, Layout, Margin, RichText, Stroke, Vec2,
};
use windows::core::PCWSTR;
use windows::Win32::UI::Shell::ShellExecuteW;
use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;

use crate::drives::DriveInfo;
use crate::history::HistoryEntry;
use crate::models::{self, Group, Host, Settings, SortMode, ViewMode};
use crate::rdp_warning;
use crate::store::Store;
use crate::transfer::{TransferDoc, TransferPick};
use crate::ui::{chip, chip_disabled, segmented, Palette};

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
        // 新增主机默认映射本机全部盘符、剪贴板开、声音关；不合适再逐项取消。
        let default_drives: Vec<String> = drives
            .iter()
            .map(|d| d.letter.to_ascii_uppercase().to_string())
            .collect();
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
            audio: false,
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
    /// 编辑界面里点了「复制一份」：调用方按这个下标把主机原样复制一份（名字加「副本」）。
    Copy {
        index: usize,
    },
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
                        if let Some(index) = editing_index.filter(|_| !form.delete_armed) {
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
                            ui.add_space(6.0);
                            // 原样复制一份：地址、端口、账号、密码、分组、盘符、开关、序号全带走，
                            // 名字后面加「副本」，插在原主机后面
                            if ui
                                .add(
                                    egui::Button::new(
                                        RichText::new("复制一份").size(12.0).color(pal.accent),
                                    )
                                    .fill(Color32::TRANSPARENT)
                                    .stroke(Stroke::new(1.0_f32, pal.accent))
                                    .corner_radius(CornerRadius::same(7))
                                    .min_size(Vec2::new(86.0, 34.0)),
                                )
                                .on_hover_text("按原样复制这台主机，名字后面加「副本」")
                                .clicked()
                            {
                                outcome = HostDialogOutcome::Copy { index };
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
    /// 点了「导入本机远程桌面历史」：由调用方真正去读注册表并落库，
    /// 设置弹窗保持打开。
    ImportHistory,
    /// 点了「导出配置…」：调用方弹文件保存框并写文件，设置弹窗保持打开。
    ExportConfig,
    /// 点了「导入配置…」：调用方弹文件选择框、读文件，再叠一层勾选弹窗。
    ImportConfig,
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
                    section_title(ui, pal, "导入 / 导出");
                    ui.label(
                        RichText::new(
                            "从本机远程桌面（mstsc）导入主机，或把整份配置导出成文件，带到另一台电脑再导回来。",
                        )
                        .size(11.0)
                        .color(pal.text_dim),
                    );
                    ui.add_space(4.0);
                    ui.horizontal_wrapped(|ui| {
                        ui.spacing_mut().item_spacing = Vec2::new(8.0, 6.0);
                        if ui
                            .add(
                                egui::Button::new(
                                    RichText::new("导入本机远程桌面历史…")
                                        .size(12.0)
                                        .color(Color32::WHITE),
                                )
                                .fill(pal.accent)
                                .corner_radius(CornerRadius::same(7))
                                .min_size(Vec2::new(0.0, 31.0)),
                            )
                            .on_hover_text("读 HKCU 下 mstsc 的最近连接记录，列出机器供勾选，不改动 Windows 自身设置")
                            .clicked()
                        {
                            outcome = SettingsOutcome::ImportHistory;
                        }
                        if ui
                            .add(
                                egui::Button::new(
                                    RichText::new("导出配置…")
                                        .size(12.0)
                                        .color(pal.accent),
                                )
                                .fill(pal.accent_soft)
                                .corner_radius(CornerRadius::same(7))
                                .min_size(Vec2::new(0.0, 31.0)),
                            )
                            .on_hover_text("主机（含密码）、分组、设置项全部写进一个加密的 .rdplcfg 文件")
                            .clicked()
                        {
                            outcome = SettingsOutcome::ExportConfig;
                        }
                        if ui
                            .add(
                                egui::Button::new(
                                    RichText::new("导入配置…")
                                        .size(12.0)
                                        .color(pal.accent),
                                )
                                .fill(pal.accent_soft)
                                .corner_radius(CornerRadius::same(7))
                                .min_size(Vec2::new(0.0, 31.0)),
                            )
                            .on_hover_text("选一个 .rdplcfg 文件，先列出里面的内容，勾选要导入的部分")
                            .clicked()
                        {
                            outcome = SettingsOutcome::ImportConfig;
                        }
                    });
                    ui.label(
                        RichText::new(
                            "历史导入：能导入的默认全选，已在列表里的标灰跳过，密码要自己填。\n\
                             导出配置：含全部主机和密码（文件已加密，但本软件都能解开，别随手传）。\n\
                             导入配置：同地址同端口的主机自动跳过，设置项勾了才会覆盖当前设置。",
                        )
                        .size(11.0)
                        .color(pal.text_dim),
                    );

                    ui.add_space(12.0);
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
                    if draft.settings.probe {
                        ui.add_space(6.0);
                        ui.horizontal(|ui| {
                            ui.label(RichText::new("自动刷新").size(12.0).color(pal.text_dim));
                            segmented(
                                ui,
                                pal,
                                &mut draft.settings.probe_interval_secs,
                                &[
                                    (30u64, "30 秒"),
                                    (60u64, "1 分钟"),
                                    (300u64, "5 分钟"),
                                    (0u64, "不自动"),
                                ],
                            );
                        });
                        ui.label(
                            RichText::new(
                                "启动时先测一轮；之后按这个间隔在后台重新检测，变化会自己更新。\
                                 顶部「● 在线 x/y」点一下可以立刻重测，隐藏到托盘时不刷新。",
                            )
                            .size(11.0)
                            .color(pal.text_dim),
                        );
                    }
                    if draft.settings.close_to_tray {
                        ui.label(
                            RichText::new(
                                "开启后关闭窗口不会退出：右键任务栏通知区域的小图标 → 最近连接 / 打开主界面 / 退出。",
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
                    ui.add_space(12.0);
                    section_title(ui, pal, "主题");
                    segmented(
                        ui,
                        pal,
                        &mut draft.settings.dark,
                        &[(false, "浅色"), (true, "深色")],
                    );
                    ui.label(
                        RichText::new("切主题只在设置里；点「保存」后整个界面立刻换。")
                            .size(11.0)
                            .color(pal.text_dim),
                    );

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
// 导入本机远程桌面历史（多选）
// ---------------------------------------------------------------------------

/// 一条待导入的历史 + 勾选状态。
pub struct ImportItem {
    pub entry: HistoryEntry,
    pub selected: bool,
    /// 同地址同端口已经在主机列表里：默认不勾、也不能勾（勾了也导不进来）。
    pub existing: bool,
}

impl ImportItem {
    pub fn label(&self) -> String {
        models::format_authority(&self.entry.host, self.entry.port)
    }
}

/// 「导入本机远程桌面历史」弹窗的草稿：先把找到的机器全列出来，让人勾。
pub struct ImportDraft {
    pub items: Vec<ImportItem>,
}

impl ImportDraft {
    /// 真正读注册表：设置 / 空状态里点「导入」时调用。
    pub fn from_local(store: &Store) -> Self {
        Self::from_entries(crate::history::local_history(), &store.hosts)
    }

    /// 纯函数版本（方便单测）：标记已存在的主机，能导入的默认全选。
    pub fn from_entries(entries: Vec<HistoryEntry>, hosts: &[Host]) -> Self {
        let items = entries
            .into_iter()
            .map(|entry| {
                let existing = models::address_exists(hosts, &entry.host, entry.port);
                ImportItem {
                    selected: !existing,
                    existing,
                    entry,
                }
            })
            .collect();
        Self { items }
    }

    /// 能不能导入（历史里没有、也不在现有列表里）。
    pub fn importable(&self) -> usize {
        self.items.iter().filter(|item| !item.existing).count()
    }

    pub fn selected_count(&self) -> usize {
        self.items
            .iter()
            .filter(|item| item.selected && !item.existing)
            .count()
    }

    pub fn all_selected(&self) -> bool {
        self.importable() > 0 && self.selected_count() == self.importable()
    }

    pub fn set_all(&mut self, on: bool) {
        for item in &mut self.items {
            if !item.existing {
                item.selected = on;
            }
        }
    }

    pub fn selected_entries(&self) -> Vec<(String, u16, String)> {
        self.items
            .iter()
            .filter(|item| item.selected && !item.existing)
            .map(|item| {
                (
                    item.entry.host.clone(),
                    item.entry.port,
                    item.entry.username.clone(),
                )
            })
            .collect()
    }
}

pub enum ImportOutcome {
    None,
    Cancel,
    /// 确认导入这些主机（已按勾选过滤好的）。
    Import(Vec<(String, u16, String)>),
}

/// 导入弹窗：列出所有能导入的主机，多选，默认全选。
pub fn draw_import_dialog(
    ctx: &egui::Context,
    pal: &Palette,
    draft: &mut ImportDraft,
) -> ImportOutcome {
    let mut outcome = ImportOutcome::None;

    egui::Modal::new(egui::Id::new("import_dialog"))
        .frame(
            Frame::new()
                .fill(pal.panel)
                .stroke(Stroke::new(1.0_f32, pal.border))
                .corner_radius(CornerRadius::same(14))
                .inner_margin(Margin::same(20)),
        )
        .backdrop_color(Color32::from_black_alpha(110))
        .show(ctx, |ui| {
            let width = (ctx.screen_rect().width() - 56.0).clamp(340.0, 540.0);
            ui.set_width(width);
            ui.label(
                RichText::new("导入本机远程桌面历史")
                    .size(18.0)
                    .strong()
                    .color(pal.text),
            );

            if draft.items.is_empty() {
                ui.label(
                    RichText::new(
                        "没有找到本机的远程桌面历史。\n\
                         在 mstsc 里连过机器之后才会留下记录，也可以直接「+ 新增主机」。",
                    )
                    .size(12.5)
                    .color(pal.text_dim),
                );
                ui.add_space(14.0);
                ui.horizontal(|ui| {
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if ui
                            .add(
                                egui::Button::new(
                                    RichText::new("关闭").size(13.0).color(Color32::WHITE),
                                )
                                .fill(pal.accent)
                                .corner_radius(CornerRadius::same(9))
                                .min_size(Vec2::new(84.0, 34.0)),
                            )
                            .clicked()
                        {
                            outcome = ImportOutcome::Cancel;
                        }
                    });
                });
                return;
            }

            let existing = draft.items.iter().filter(|item| item.existing).count();
            let mut subtitle = format!(
                "找到 {} 台，可导入 {} 台。勾选要导入的机器，默认全选。",
                draft.items.len(),
                draft.importable()
            );
            if existing > 0 {
                subtitle.push_str(&format!("（{existing} 台已在列表里，灰色不可选）"));
            }
            ui.label(RichText::new(subtitle).size(11.5).color(pal.text_dim));
            ui.add_space(6.0);

            ui.horizontal(|ui| {
                if ui
                    .add(
                        egui::Button::new(RichText::new("全选").size(11.5).color(pal.accent))
                            .fill(Color32::TRANSPARENT)
                            .stroke(Stroke::NONE)
                            .min_size(Vec2::new(0.0, 24.0)),
                    )
                    .clicked()
                {
                    draft.set_all(true);
                }
                if ui
                    .add(
                        egui::Button::new(RichText::new("全不选").size(11.5).color(pal.text_dim))
                            .fill(Color32::TRANSPARENT)
                            .stroke(Stroke::NONE)
                            .min_size(Vec2::new(0.0, 24.0)),
                    )
                    .clicked()
                {
                    draft.set_all(false);
                }
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    let suffix = if draft.all_selected() { "（全部）" } else { "" };
                    ui.label(
                        RichText::new(format!(
                            "已选 {}/{}{suffix}",
                            draft.selected_count(),
                            draft.importable()
                        ))
                        .size(11.0)
                        .color(pal.text_dim),
                    );
                });
            });

            ui.add_space(4.0);
            // 主机少的时候弹窗跟着变矮，多了才滚动
            let body_h = (ctx.screen_rect().height() - 250.0).max(120.0);
            egui::ScrollArea::vertical()
                .max_height(body_h)
                .scroll_bar_visibility(
                    egui::containers::scroll_area::ScrollBarVisibility::VisibleWhenNeeded,
                )
                .auto_shrink([false, true])
                .show(ui, |ui| {
                    for item in &mut draft.items {
                        ui.horizontal(|ui| {
                            let label = item.label();
                            if item.existing {
                                chip_disabled(ui, pal, &label);
                                ui.label(
                                    RichText::new("已在列表")
                                        .size(11.0)
                                        .color(pal.text_dim),
                                );
                            } else {
                                if chip(ui, pal, item.selected, &label, 0.0).clicked() {
                                    item.selected = !item.selected;
                                }
                                if !item.entry.username.trim().is_empty() {
                                    ui.label(
                                        RichText::new(format!(
                                            "账号 {}",
                                            item.entry.username.trim()
                                        ))
                                        .size(11.0)
                                        .color(pal.text_dim),
                                    );
                                }
                            }
                        });
                    }
                });

            ui.add_space(12.0);
            ui.label(
                RichText::new(
                    "导入的主机默认带剪贴板、不带声音、映射本机全部盘符，账号从历史里带过来，\
                     密码需要自己填。",
                )
                .size(11.0)
                .color(pal.text_dim),
            );
            ui.add_space(10.0);
            ui.horizontal(|ui| {
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    let count = draft.selected_count();
                    let can_import = count > 0;
                    let import_btn = egui::Button::new(
                        RichText::new(format!("导入 {count} 台"))
                            .size(13.0)
                            .color(Color32::WHITE),
                    )
                    .fill(if can_import { pal.accent } else { pal.checking })
                    .corner_radius(CornerRadius::same(9))
                    .min_size(Vec2::new(96.0, 34.0));
                    if ui
                        .add_enabled(can_import, import_btn)
                        .on_hover_text(if can_import {
                            "把这些主机加进列表"
                        } else {
                            "先勾选至少一台"
                        })
                        .clicked()
                    {
                        outcome = ImportOutcome::Import(draft.selected_entries());
                    }
                    if ui
                        .add(
                            egui::Button::new(
                                RichText::new("取消").size(13.0).color(pal.text_dim),
                            )
                            .fill(pal.panel)
                            .stroke(Stroke::new(1.0_f32, pal.border))
                            .corner_radius(CornerRadius::same(9))
                            .min_size(Vec2::new(76.0, 34.0)),
                        )
                        .clicked()
                    {
                        outcome = ImportOutcome::Cancel;
                    }
                });
            });
        });

    outcome
}

// ---------------------------------------------------------------------------
// 导入配置（从 .rdplcfg 文件，跨电脑搬配置）
// ---------------------------------------------------------------------------

/// 弹窗里的一台待导入主机。
pub struct TransferHostItem {
    /// 在 `doc.hosts` 里的下标
    pub index: usize,
    pub name: String,
    pub address: String,
    pub has_password: bool,
    /// 同地址同端口已经在列表里：标灰、不能勾
    pub existing: bool,
    pub selected: bool,
}

/// 「导入配置」弹窗的草稿：先把文件里的内容全列出来，让人挑要哪些。
pub struct TransferDraft {
    pub path: std::path::PathBuf,
    pub doc: TransferDoc,
    pub hosts: Vec<TransferHostItem>,
    pub take_groups: bool,
    pub take_settings: bool,
    pub take_recent: bool,
}

impl TransferDraft {
    /// `existing` 是导入前的主机列表，用来把「已经在列表里」的标灰。
    pub fn new(path: std::path::PathBuf, doc: TransferDoc, existing: &[Host]) -> Self {
        let hosts = doc
            .hosts
            .iter()
            .enumerate()
            .map(|(index, host)| {
                let exists = models::address_exists(existing, &host.host, host.port);
                TransferHostItem {
                    index,
                    name: if host.name.trim().is_empty() {
                        host.display_address()
                    } else {
                        host.name.clone()
                    },
                    address: host.display_address(),
                    has_password: !host.password.trim().is_empty(),
                    existing: exists,
                    selected: !exists,
                }
            })
            .collect();
        Self {
            path,
            doc,
            hosts,
            take_groups: true,
            // 设置项会盖掉当前的排序 / 显示 / 主题 / 探活，默认不勾，想要自己点
            take_settings: false,
            take_recent: true,
        }
    }

    pub fn importable(&self) -> usize {
        self.hosts.iter().filter(|item| !item.existing).count()
    }

    pub fn selected_count(&self) -> usize {
        self.hosts
            .iter()
            .filter(|item| item.selected && !item.existing)
            .count()
    }

    pub fn all_selected(&self) -> bool {
        self.importable() > 0 && self.selected_count() == self.importable()
    }

    pub fn set_all(&mut self, on: bool) {
        for item in &mut self.hosts {
            if !item.existing {
                item.selected = on;
            }
        }
    }

    /// 文件里带密码的台数，弹窗提示用（不按勾选算：标灰的那些也算「文件里有」）。
    pub fn with_password(&self) -> usize {
        self.hosts.iter().filter(|item| item.has_password).count()
    }

    pub fn file_name(&self) -> String {
        self.path
            .file_name()
            .map(|name| name.to_string_lossy().to_string())
            .unwrap_or_else(|| self.path.display().to_string())
    }

    pub fn pick(&self) -> TransferPick {
        TransferPick {
            hosts: self
                .hosts
                .iter()
                .filter(|item| item.selected && !item.existing)
                .map(|item| item.index)
                .collect(),
            groups: self.take_groups && !self.doc.groups.is_empty(),
            settings: self.take_settings,
            recent: self.take_recent && !self.doc.recent.is_empty(),
        }
    }
}

pub enum TransferOutcome {
    None,
    Cancel,
    /// 确认导入：只带勾中的内容。
    Apply(TransferPick),
}

/// 导入配置弹窗：文件里有什么先摆明白，主机逐台勾，分组 / 设置项 / 最近连接按类勾。
pub fn draw_transfer_dialog(
    ctx: &egui::Context,
    pal: &Palette,
    draft: &mut TransferDraft,
) -> TransferOutcome {
    let mut outcome = TransferOutcome::None;

    egui::Modal::new(egui::Id::new("transfer_dialog"))
        .frame(
            Frame::new()
                .fill(pal.panel)
                .stroke(Stroke::new(1.0_f32, pal.border))
                .corner_radius(CornerRadius::same(14))
                .inner_margin(Margin::same(20)),
        )
        .backdrop_color(Color32::from_black_alpha(110))
        .show(ctx, |ui| {
            let width = (ctx.screen_rect().width() - 56.0).clamp(340.0, 560.0);
            ui.set_width(width);
            ui.label(RichText::new("导入配置").size(18.0).strong().color(pal.text));

            let meta = if draft.doc.app_version.trim().is_empty() {
                String::new()
            } else {
                format!("导出自 v{}", draft.doc.app_version.trim())
            };
            let mut subtitle = draft.file_name();
            if !draft.doc.exported_at.trim().is_empty() {
                subtitle.push_str(&format!("  ·  {}", draft.doc.exported_at.trim()));
            }
            if !meta.is_empty() {
                subtitle.push_str(&format!("  ·  {meta}"));
            }
            ui.label(RichText::new(subtitle).size(11.5).color(pal.text_dim));
            ui.label(
                RichText::new(draft.doc.summary())
                    .size(11.0)
                    .color(pal.hint),
            );
            ui.add_space(6.0);

            ui.label(
                RichText::new("勾选要导入的内容：")
                    .size(12.0)
                    .color(pal.text_dim),
            );
            ui.horizontal_wrapped(|ui| {
                ui.spacing_mut().item_spacing = Vec2::new(6.0, 6.0);
                if !draft.doc.groups.is_empty() {
                    let label = format!("分组 {} 个", draft.doc.groups.len());
                    if chip(ui, pal, draft.take_groups, &label, 0.0)
                        .on_hover_text("缺的分组补进来，同名的不会重复")
                        .clicked()
                    {
                        draft.take_groups = !draft.take_groups;
                    }
                }
                let settings_label = "设置项（覆盖当前）".to_string();
                if chip(ui, pal, draft.take_settings, &settings_label, 0.0)
                    .on_hover_text("排序、显示方式、主题、探活间隔、托盘这些会被文件里的设置盖掉")
                    .clicked()
                {
                    draft.take_settings = !draft.take_settings;
                }
                if !draft.doc.recent.is_empty() {
                    let label = format!("最近连接 {} 条", draft.doc.recent.len());
                    if chip(ui, pal, draft.take_recent, &label, 0.0)
                        .on_hover_text("导进来的主机照样按原来的最近顺序排在托盘里")
                        .clicked()
                    {
                        draft.take_recent = !draft.take_recent;
                    }
                }
            });

            ui.add_space(6.0);
            if draft.hosts.is_empty() {
                ui.label(
                    RichText::new("这个文件里没有主机，只能导入分组 / 设置项。")
                        .size(12.0)
                        .color(pal.text_dim),
                );
            } else {
                ui.horizontal(|ui| {
                    if ui
                        .add(
                            egui::Button::new(
                                RichText::new("全选").size(11.5).color(pal.accent),
                            )
                            .fill(Color32::TRANSPARENT)
                            .stroke(Stroke::NONE)
                            .min_size(Vec2::new(0.0, 24.0)),
                        )
                        .clicked()
                    {
                        draft.set_all(true);
                    }
                    if ui
                        .add(
                            egui::Button::new(
                                RichText::new("全不选").size(11.5).color(pal.text_dim),
                            )
                            .fill(Color32::TRANSPARENT)
                            .stroke(Stroke::NONE)
                            .min_size(Vec2::new(0.0, 24.0)),
                        )
                        .clicked()
                    {
                        draft.set_all(false);
                    }
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        let suffix = if draft.all_selected() { "（全部）" } else { "" };
                        let text = if draft.importable() == 0 {
                            "都在列表里了".to_string()
                        } else {
                            format!(
                                "已选 {}/{}{suffix}",
                                draft.selected_count(),
                                draft.importable()
                            )
                        };
                        ui.label(RichText::new(text).size(11.0).color(pal.text_dim));
                    });
                });

                ui.add_space(2.0);
                let body_h = (ctx.screen_rect().height() - 330.0).max(110.0);
                egui::ScrollArea::vertical()
                    .max_height(body_h)
                    .scroll_bar_visibility(
                        egui::containers::scroll_area::ScrollBarVisibility::VisibleWhenNeeded,
                    )
                    .auto_shrink([false, true])
                    .show(ui, |ui| {
                        for item in &mut draft.hosts {
                            ui.horizontal(|ui| {
                                if item.existing {
                                    chip_disabled(ui, pal, &item.name);
                                    ui.label(
                                        RichText::new(format!("{}  · 已在列表", item.address))
                                            .size(11.0)
                                            .color(pal.text_dim),
                                    );
                                } else {
                                    if chip(ui, pal, item.selected, &item.name, 0.0).clicked() {
                                        item.selected = !item.selected;
                                    }
                                    let mut detail = item.address.clone();
                                    if item.has_password {
                                        detail.push_str("  · 带密码");
                                    }
                                    ui.label(
                                        RichText::new(detail).size(11.0).color(pal.text_dim),
                                    );
                                }
                            });
                        }
                    });
            }

            ui.add_space(10.0);
            let with_password = draft.with_password();
            let hint = if with_password > 0 {
                format!(
                    "文件里有 {with_password} 台带密码，导入后可以直接连；同地址同端口的主机不会重复导入。"
                )
            } else {
                "同地址同端口的主机不会重复导入；这个文件里的主机没有存密码。".to_string()
            };
            ui.label(RichText::new(hint).size(11.0).color(pal.text_dim));

            ui.add_space(10.0);
            ui.horizontal(|ui| {
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    let pick = draft.pick();
                    let can_apply = !pick.is_empty();
                    let label = if draft.selected_count() > 0 {
                        format!("导入 {} 台", draft.selected_count())
                    } else {
                        "导入".to_string()
                    };
                    let apply_btn = egui::Button::new(
                        RichText::new(label).size(13.0).color(Color32::WHITE),
                    )
                    .fill(if can_apply { pal.accent } else { pal.checking })
                    .corner_radius(CornerRadius::same(9))
                    .min_size(Vec2::new(96.0, 34.0));
                    if ui
                        .add_enabled(can_apply, apply_btn)
                        .on_hover_text(if can_apply {
                            "把勾中的内容合并进当前配置"
                        } else {
                            "先勾选至少一项"
                        })
                        .clicked()
                    {
                        outcome = TransferOutcome::Apply(pick);
                    }
                    if ui
                        .add(
                            egui::Button::new(
                                RichText::new("取消").size(13.0).color(pal.text_dim),
                            )
                            .fill(pal.panel)
                            .stroke(Stroke::new(1.0_f32, pal.border))
                            .corner_radius(CornerRadius::same(9))
                            .min_size(Vec2::new(76.0, 34.0)),
                        )
                        .clicked()
                    {
                        outcome = TransferOutcome::Cancel;
                    }
                });
            });
        });

    outcome
}

// ---------------------------------------------------------------------------
// 关于
// ---------------------------------------------------------------------------

pub enum AboutOutcome {
    None,
    Close,
    /// 点了「重新检查最新版」：调用方重新发一次 GitHub 查询。
    Recheck,
}

/// 右下角版本号点开的「关于」弹窗。
pub fn draw_about_dialog(
    ctx: &egui::Context,
    pal: &Palette,
    version: &str,
    update: &crate::update::UpdateState,
) -> AboutOutcome {
    let mut outcome = AboutOutcome::None;
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
            ui.set_width(470.0);
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

            // 最新版状态：检查在后台线程里跑，这里只负责显示
            let state_color = match update {
                crate::update::UpdateState::Latest(_) => pal.online,
                crate::update::UpdateState::Newer(_) => pal.accent,
                crate::update::UpdateState::Failed(_) => pal.danger,
                _ => pal.text_dim,
            };
            ui.horizontal(|ui| {
                ui.label(
                    RichText::new(update.status_text())
                        .size(12.0)
                        .color(state_color),
                );
                if update.can_recheck() && small_link(ui, pal, "重新检查").clicked() {
                    outcome = AboutOutcome::Recheck;
                }
            });

            ui.add_space(8.0);
            ui.horizontal(|ui| {
                ui.label(RichText::new("GitHub：").size(11.5).color(pal.text_dim));
                url_link(ui, pal, "github.com/qlings/rdp-launcher", crate::update::REPO_URL);
            });
            ui.horizontal(|ui| {
                ui.label(RichText::new("下载：").size(11.5).color(pal.text_dim));
                url_link(
                    ui,
                    pal,
                    "github.com/qlings/rdp-launcher/releases/latest",
                    crate::update::DOWNLOAD_URL,
                );
            });

            ui.add_space(12.0);
            ui.label(
                RichText::new("轻量级 Windows 远程桌面启动器")
                    .size(14.0)
                    .color(pal.text),
            );
            ui.add_space(4.0);
            for line in [
                "单文件绿色版：主机、密码、设置加密存在 exe 里，不生成 json。",
                "多分组、端口独立、盘符 / 声音 / 剪贴板映射、在线状态自动刷新。",
                "本机历史导入，配置可导出成文件带到另一台电脑。",
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
                        outcome = AboutOutcome::Close;
                    }
                });
            });
        });
    outcome
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

/// 新增主机时的查重：同地址 + 同端口已经存在（地址大小写不敏感），返回那台重复的主机。
///
/// 编辑已保存的主机时不查重：复制出来的主机本来就和原主机同地址同端口，
/// 还拦着的话连改个名字都保存不了。
fn find_duplicate<'a>(
    others: &'a [Host],
    editing_index: Option<usize>,
    host: &Host,
) -> Option<&'a Host> {
    if editing_index.is_some() {
        return None;
    }
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

fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(std::iter::once(0)).collect()
}

/// 用系统默认浏览器打开链接（egui 自带的链接要开 webbrowser feature，这里直接走 Shell）。
fn open_url(url: &str) {
    let operation = wide("open");
    let target = wide(url);
    // SAFETY: 两个字符串都以 NUL 结尾且在调用期间存活；窗口句柄为空也能打开。
    unsafe {
        let _ = ShellExecuteW(
            windows::Win32::Foundation::HWND(std::ptr::null_mut()),
            PCWSTR(operation.as_ptr()),
            PCWSTR(target.as_ptr()),
            PCWSTR::null(),
            PCWSTR::null(),
            SW_SHOWNORMAL,
        );
    }
}

/// 蓝色可点文字，点了用浏览器打开 `url`。
fn url_link(ui: &mut egui::Ui, pal: &Palette, text: &str, url: &str) {
    let response = ui
        .add(
            egui::Button::new(RichText::new(text).size(11.5).color(pal.accent))
                .fill(Color32::TRANSPARENT)
                .stroke(Stroke::NONE)
                .min_size(Vec2::new(0.0, 22.0)),
        )
        .on_hover_text(format!("用浏览器打开 {url}"))
        .on_hover_cursor(egui::CursorIcon::PointingHand);
    if response.clicked() {
        open_url(url);
    }
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
        // 编辑一台主机时，同地址同端口的另一台（复制出来的副本）也不再拦
        assert!(find_duplicate(std::slice::from_ref(&existing), Some(1), &candidate).is_none());
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

    fn history(host: &str, port: u16, username: &str) -> HistoryEntry {
        HistoryEntry {
            host: host.into(),
            port,
            username: username.into(),
        }
    }

    #[test]
    fn import_draft_defaults_to_all_importable_and_marks_existing() {
        let mut store = Store::default();
        store.hosts = vec![Host {
            id: "a".into(),
            host: "10.0.0.1".into(),
            port: 3389,
            ..Default::default()
        }];
        let draft = ImportDraft::from_entries(
            vec![
                history("10.0.0.1", 3389, ""),
                history("10.0.0.2", 3390, "ql"),
                history("10.0.0.3", 3389, ""),
            ],
            &store.hosts,
        );
        assert_eq!(draft.items.len(), 3);
        assert!(draft.items[0].existing, "已经在列表里的要标出来");
        assert!(!draft.items[0].selected);
        assert_eq!(draft.importable(), 2);
        assert_eq!(draft.selected_count(), 2, "能导入的默认全选");
        assert!(draft.all_selected());
        assert_eq!(
            draft.selected_entries(),
            vec![
                ("10.0.0.2".to_string(), 3390, "ql".to_string()),
                ("10.0.0.3".to_string(), 3389, String::new()),
            ]
        );
        // 显示用的地址带端口，IPv6 加方括号
        assert_eq!(draft.items[0].label(), "10.0.0.1:3389");
    }

    #[test]
    fn import_draft_can_clear_and_reselect_but_skips_existing() {
        let existing = Host {
            id: "a".into(),
            host: "10.0.0.1".into(),
            port: 3389,
            ..Default::default()
        };
        let mut draft = ImportDraft::from_entries(
            vec![history("10.0.0.1", 3389, ""), history("10.0.0.2", 3389, "")],
            std::slice::from_ref(&existing),
        );
        draft.set_all(false);
        assert_eq!(draft.selected_count(), 0);
        assert!(!draft.all_selected());
        assert!(draft.selected_entries().is_empty());

        draft.set_all(true);
        assert_eq!(draft.selected_count(), 1);
        assert!(draft.all_selected());
    }

    #[test]
    fn import_draft_with_only_existing_hosts_has_nothing_to_import() {
        let existing = Host {
            id: "a".into(),
            host: "10.0.0.1".into(),
            port: 3389,
            ..Default::default()
        };
        let mut draft =
            ImportDraft::from_entries(vec![history("10.0.0.1", 3389, "")], &[existing]);
        assert_eq!(draft.importable(), 0);
        assert_eq!(draft.selected_count(), 0);
        assert!(!draft.all_selected(), "没有可导入的就不算全选");
        draft.set_all(true);
        assert_eq!(draft.selected_count(), 0, "已存在的主机勾不上");
    }

    /// 不开窗口跑一帧，确认导入弹窗（含空列表分支）画得出来、不会 panic。
    #[test]
    fn import_dialog_renders_in_a_headless_context() {
        let pal = crate::ui::palette(false);
        for entries in [
            vec![
                history("10.0.0.2", 3390, "ql"),
                history("10.0.0.1", 3389, ""),
            ],
            Vec::new(),
        ] {
            let mut draft = ImportDraft::from_entries(entries, &[]);
            let ctx = egui::Context::default();
            let _ = ctx.run(Default::default(), |ctx| {
                let _ = draw_import_dialog(ctx, &pal, &mut draft);
            });
        }
    }
}

