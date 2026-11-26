//! 主界面绘制：配色、顶部栏、分组胶囊、列表 / 方块、Toast。
//!
//! 键盘优先级在 `main.rs` 里用 `ctx.input()` 抢在 TextEdit 之前处理，
//! 这里只负责把「选中项」「搜索」「分组」「显示方式」翻译成画面。

use eframe::egui;
use egui::{
    Align, Color32, CornerRadius, CursorIcon, FontId, Frame, Layout, Margin, Pos2, Rect, RichText,
    Sense, Stroke, StrokeKind, Vec2,
};

use crate::models::{Group, Host, SortMode, ViewMode};
use crate::probe::ProbeStatus;

// ---------------------------------------------------------------------------
// 配色
// ---------------------------------------------------------------------------

#[derive(Clone, Copy)]
pub struct Palette {
    pub bg: Color32,
    pub panel: Color32,
    pub card: Color32,
    pub card_hover: Color32,
    pub card_selected: Color32,
    pub input_bg: Color32,
    pub border: Color32,
    pub border_selected: Color32,
    pub text: Color32,
    pub text_dim: Color32,
    /// 输入框占位提示词，刻意比 text_dim 更淡
    pub hint: Color32,
    pub accent: Color32,
    pub accent_soft: Color32,
    pub online: Color32,
    pub offline: Color32,
    pub checking: Color32,
    pub danger: Color32,
    pub danger_soft: Color32,
}

pub fn palette(dark: bool) -> Palette {
    if !dark {
        Palette {
            bg: Color32::from_rgb(0xF6, 0xF7, 0xFA),
            panel: Color32::from_rgb(0xFF, 0xFF, 0xFF),
            card: Color32::from_rgb(0xFF, 0xFF, 0xFF),
            card_hover: Color32::from_rgb(0xEE, 0xF3, 0xFF),
            card_selected: Color32::from_rgb(0xE6, 0xEE, 0xFF),
            input_bg: Color32::from_rgb(0xF1, 0xF4, 0xF9),
            border: Color32::from_rgb(0xDD, 0xE2, 0xEC),
            border_selected: Color32::from_rgb(0x3B, 0x6D, 0xE8),
            text: Color32::from_rgb(0x1C, 0x21, 0x2B),
            text_dim: Color32::from_rgb(0x6E, 0x78, 0x8A),
            hint: Color32::from_rgb(0xB6, 0xBF, 0xCE),
            accent: Color32::from_rgb(0x3B, 0x6D, 0xE8),
            accent_soft: Color32::from_rgb(0xE4, 0xEC, 0xFD),
            online: Color32::from_rgb(0x22, 0xB5, 0x6B),
            offline: Color32::from_rgb(0xE5, 0x4C, 0x4C),
            checking: Color32::from_rgb(0xA9, 0xB2, 0xC0),
            danger: Color32::from_rgb(0xD9, 0x3B, 0x3B),
            danger_soft: Color32::from_rgb(0xFD, 0xE8, 0xE8),
        }
    } else {
        Palette {
            bg: Color32::from_rgb(0x16, 0x18, 0x1D),
            panel: Color32::from_rgb(0x1E, 0x21, 0x27),
            card: Color32::from_rgb(0x23, 0x27, 0x2E),
            card_hover: Color32::from_rgb(0x2A, 0x30, 0x3A),
            card_selected: Color32::from_rgb(0x2C, 0x35, 0x46),
            input_bg: Color32::from_rgb(0x1A, 0x1D, 0x23),
            border: Color32::from_rgb(0x33, 0x38, 0x42),
            border_selected: Color32::from_rgb(0x5B, 0x8A, 0xFF),
            text: Color32::from_rgb(0xE8, 0xEB, 0xF0),
            text_dim: Color32::from_rgb(0x99, 0xA3, 0xB4),
            hint: Color32::from_rgb(0x66, 0x6F, 0x7E),
            accent: Color32::from_rgb(0x5B, 0x8A, 0xFF),
            accent_soft: Color32::from_rgb(0x27, 0x31, 0x45),
            online: Color32::from_rgb(0x3D, 0xD5, 0x8A),
            offline: Color32::from_rgb(0xFF, 0x6B, 0x6B),
            checking: Color32::from_rgb(0x5A, 0x61, 0x6E),
            danger: Color32::from_rgb(0xFF, 0x6B, 0x6B),
            danger_soft: Color32::from_rgb(0x3A, 0x23, 0x26),
        }
    }
}

// ---------------------------------------------------------------------------
// 顶部栏
// ---------------------------------------------------------------------------

pub struct TopBarOutput {
    pub open_add: bool,
    pub open_settings: bool,
    pub toggle_theme: bool,
    /// 这一帧搜索词变了：列表要重算过滤 + 排序
    pub search_changed: bool,
    /// 点了「在线 x/y」小胶囊：立刻重新检测一轮
    pub refresh_probe: bool,
}

/// 顶部那颗「在线 x/y」胶囊要显示什么。
#[derive(Clone, Copy, Default)]
pub struct ProbeSummary {
    pub enabled: bool,
    pub online: usize,
    pub total: usize,
    /// 正在检测（有批次在跑）
    pub checking: bool,
    /// 距离上次检测结束过了多久（从没测过是 None）
    pub since_last: Option<std::time::Duration>,
}

pub fn draw_top_bar(
    ui: &mut egui::Ui,
    pal: &Palette,
    search: &mut String,
    show_search: bool,
    want_focus_search: &mut bool,
    dark: bool,
    probe: ProbeSummary,
) -> TopBarOutput {
    let mut out = TopBarOutput {
        open_add: false,
        open_settings: false,
        toggle_theme: false,
        search_changed: false,
        refresh_probe: false,
    };

    ui.horizontal(|ui| {
        if show_search {
            // 按钮区大概占 250px，剩下的都给搜索框；窄窗口下保底 120px
            let width = (ui.available_width() - 258.0).max(120.0);
            let text_edit = egui::TextEdit::singleline(search)
                .hint_text("搜索备注名 / 地址 / 端口 / 分组…")
                .desired_width(width)
                .margin(Vec2::new(12.0, 9.0))
                .font(FontId::proportional(15.0));
            let response = ui.add(text_edit);
            out.search_changed = response.changed();
            if *want_focus_search {
                response.request_focus();
                *want_focus_search = false;
            }
            ui.add_space(8.0);
        } else {
            ui.label(RichText::new("RDP 启动器").size(18.0).strong().color(pal.text));
            ui.add_space(8.0);
        }

        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            let theme_label = if dark { "☀" } else { "🌙" };
            let theme_btn = egui::Button::new(RichText::new(theme_label).size(16.0))
                .fill(pal.accent_soft)
                .corner_radius(CornerRadius::same(9))
                .min_size(Vec2::new(38.0, 38.0));
            if ui.add(theme_btn).on_hover_text("切换主题").clicked() {
                out.toggle_theme = true;
            }

            let settings_btn = egui::Button::new(
                RichText::new("设置")
                    .size(14.0)
                    .color(pal.accent)
                    .strong(),
            )
            .fill(pal.accent_soft)
            .corner_radius(CornerRadius::same(9))
            .min_size(Vec2::new(64.0, 38.0));
            if ui.add(settings_btn).clicked() {
                out.open_settings = true;
            }

            let add_btn = egui::Button::new(
                RichText::new("+ 新增主机")
                    .size(14.5)
                    .color(Color32::WHITE)
                    .strong(),
            )
            .fill(pal.accent)
            .corner_radius(CornerRadius::same(9))
            .min_size(Vec2::new(104.0, 38.0));
            if ui.add(add_btn).clicked() {
                out.open_add = true;
            }

            // 在线统计 + 手动刷新二合一：点一下就重新检测，比在设置里找开关快。
            let (label, color) = if !probe.enabled {
                ("探活已关".to_string(), pal.text_dim)
            } else if probe.checking {
                ("● 检测中".to_string(), pal.checking)
            } else {
                let color = if probe.online > 0 {
                    pal.online
                } else if probe.total > 0 {
                    pal.offline
                } else {
                    pal.text_dim
                };
                (format!("● {}/{}", probe.online, probe.total), color)
            };
            let probe_btn = egui::Button::new(RichText::new(label).size(12.5).color(color))
                .fill(pal.panel)
                .stroke(Stroke::new(1.0_f32, pal.border))
                .corner_radius(CornerRadius::same(9))
                .min_size(Vec2::new(0.0, 38.0));
            if ui
                .add(probe_btn)
                .on_hover_text(if probe.enabled {
                    let when = match probe.since_last {
                        Some(elapsed) if elapsed.as_secs() < 5 => "刚刚检测".to_string(),
                        Some(elapsed) if elapsed.as_secs() < 60 => {
                            format!("{} 秒前检测", elapsed.as_secs())
                        }
                        Some(elapsed) => format!("{} 分钟前检测", elapsed.as_secs() / 60),
                        None => "还没检测".to_string(),
                    };
                    format!("在线 {} / 共 {} 台 · {when} · 点一下重新检测（F5）", probe.online, probe.total)
                } else {
                    "探活已在设置里关闭 · 点一下去设置里开启".to_string()
                })
                .on_hover_cursor(CursorIcon::PointingHand)
                .clicked()
            {
                out.refresh_probe = true;
            }
        });
    });

    out
}

// ---------------------------------------------------------------------------
// 分组胶囊
// ---------------------------------------------------------------------------

pub fn draw_group_bar(
    ui: &mut egui::Ui,
    pal: &Palette,
    groups: &[Group],
    selected: &mut Option<String>,
) -> bool {
    let mut changed = false;
    // 分组再多也不许把主机列表挤没：固定一行，超出的横向滚动。
    egui::ScrollArea::horizontal()
        .id_salt("group_bar")
        .max_height(56.0)
        .scroll_bar_visibility(egui::containers::scroll_area::ScrollBarVisibility::VisibleWhenNeeded)
        .auto_shrink([false, true])
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing = Vec2::new(10.0, 10.0);
                if group_pill(ui, pal, "全部", selected.is_none()) {
                    *selected = None;
                    changed = true;
                }
                for group in groups {
                    let active = selected.as_deref() == Some(group.name.as_str());
                    if group_pill(ui, pal, &group.name, active) {
                        *selected = Some(group.name.clone());
                        changed = true;
                    }
                }
            });
        });
    changed
}

const GROUP_PILL_MAX_TEXT: f32 = 150.0;

fn group_pill(ui: &mut egui::Ui, pal: &Palette, label: &str, active: bool) -> bool {
    let text_color = if active { Color32::WHITE } else { pal.text };
    let fill = if active { pal.accent } else { pal.panel };
    let stroke = if active {
        Stroke::new(1.0_f32, pal.accent)
    } else {
        Stroke::new(1.0_f32, pal.border)
    };
    let font = FontId::proportional(15.5);
    let galley = truncated_galley_text(ui, label, font, text_color, GROUP_PILL_MAX_TEXT);
    let width = (galley.size().x + 34.0).max(52.0);
    let (rect, response) = ui.allocate_exact_size(Vec2::new(width, 40.0), Sense::click());
    let fill = if active {
        fill
    } else if response.hovered() {
        pal.card_hover
    } else {
        fill
    };
    ui.painter()
        .rect(rect, CornerRadius::same(20), fill, stroke, StrokeKind::Inside);
    ui.painter().galley(
        Pos2::new(
            rect.center().x - galley.size().x / 2.0,
            rect.center().y - galley.size().y / 2.0,
        ),
        galley,
        text_color,
    );
    let response = response.on_hover_cursor(CursorIcon::PointingHand);
    if label.len() > 12 {
        response.clone().on_hover_text(label);
    }
    response.clicked()
}

// ---------------------------------------------------------------------------
// 通用小控件
// ---------------------------------------------------------------------------

const ELLIPSIS: char = '…';

fn truncate_job(text: &str, font: FontId, color: Color32, max_width: f32) -> egui::text::LayoutJob {
    let mut job = egui::text::LayoutJob::single_section(
        text.to_owned(),
        egui::TextFormat {
            font_id: font,
            color,
            ..Default::default()
        },
    );
    job.wrap = egui::text::TextWrapping {
        max_width,
        max_rows: 1,
        break_anywhere: true,
        overflow_character: Some(ELLIPSIS),
    };
    job
}

/// 单行截断文本（超出用「…」），用当前 Ui 的字体系统排版。
fn truncated_galley_text(
    ui: &egui::Ui,
    text: &str,
    font: FontId,
    color: Color32,
    max_width: f32,
) -> std::sync::Arc<egui::Galley> {
    let job = truncate_job(text, font, color, max_width);
    ui.fonts(|f| f.layout_job(job))
}

/// 同上，但用 Painter 的字体系统（行内自绘用）。
fn truncated_galley(
    painter: &egui::Painter,
    text: &str,
    font: FontId,
    color: Color32,
    max_width: f32,
) -> std::sync::Arc<egui::Galley> {
    let job = truncate_job(text, font, color, max_width);
    painter.layout_job(job)
}

/// 同一台主机 350ms 内第二次单击才算双击。
///
/// egui 的 `double_clicked()` 只按全局时间窗判断、不看落点：先点 A 行、300ms 内再点 B 行，
/// B 会被判成双击直接连出去。这里按 host id 自己配对。
fn is_host_double_click(ui: &egui::Ui, host_id: &str) -> bool {
    let now = ui.input(|i| i.time);
    let key = egui::Id::new("last_host_click");
    let last: Option<(String, f64)> = ui.data(|d| d.get_temp(key));
    let double = last
        .as_ref()
        .is_some_and(|(id, t)| id == host_id && (now - *t).abs() <= 0.35);
    if double {
        // 清掉，避免第三次点击又算一次双击
        ui.data_mut(|d| d.insert_temp(key, (String::new(), 0.0)));
    } else {
        ui.data_mut(|d| d.insert_temp(key, (host_id.to_owned(), now)));
    }
    double
}

/// 带明显方框和勾的开关胶囊。用于分组、盘符、剪贴板、声音、导入勾选。
pub fn chip(
    ui: &mut egui::Ui,
    pal: &Palette,
    selected: bool,
    label: &str,
    min_width: f32,
) -> egui::Response {
    chip_core(ui, pal, selected, label, min_width, true)
}

/// 不可勾选的胶囊：导入列表里「已经在主机列表里」的那些，看得见但点不动。
pub fn chip_disabled(ui: &mut egui::Ui, pal: &Palette, label: &str) -> egui::Response {
    chip_core(ui, pal, false, label, 0.0, false)
}

fn chip_core(
    ui: &mut egui::Ui,
    pal: &Palette,
    selected: bool,
    label: &str,
    min_width: f32,
    enabled: bool,
) -> egui::Response {
    let font = FontId::proportional(12.5);
    let text_color = if !enabled {
        pal.text_dim
    } else if selected {
        Color32::WHITE
    } else {
        pal.text
    };
    let galley = ui
        .painter()
        .layout_no_wrap(label.to_string(), font, text_color);
    let width = (galley.size().x + 46.0).max(min_width);
    let sense = if enabled { Sense::click() } else { Sense::hover() };
    let (rect, response) = ui.allocate_exact_size(Vec2::new(width, 31.0), sense);

    let fill = if !enabled {
        pal.input_bg
    } else if selected {
        pal.accent
    } else if response.hovered() {
        pal.card_hover
    } else {
        pal.panel
    };
    let stroke = if selected && enabled {
        Stroke::new(1.0_f32, pal.accent)
    } else {
        Stroke::new(1.0_f32, pal.border)
    };
    let painter = ui.painter();
    painter.rect(rect, CornerRadius::same(8), fill, stroke, StrokeKind::Inside);

    let box_rect = Rect::from_min_size(
        Pos2::new(rect.left() + 9.0, rect.center().y - 7.0),
        Vec2::splat(14.0),
    );
    if selected && enabled {
        painter.rect_filled(box_rect, CornerRadius::same(4), Color32::WHITE);
        let c = box_rect.center();
        painter.line_segment(
            [Pos2::new(c.x - 3.5, c.y), Pos2::new(c.x - 0.8, c.y + 3.0)],
            Stroke::new(2.0_f32, pal.accent),
        );
        painter.line_segment(
            [
                Pos2::new(c.x - 0.8, c.y + 3.0),
                Pos2::new(c.x + 4.0, c.y - 3.0),
            ],
            Stroke::new(2.0_f32, pal.accent),
        );
    } else {
        painter.rect(
            box_rect,
            CornerRadius::same(4),
            pal.input_bg,
            Stroke::new(1.6_f32, if enabled { pal.text_dim } else { pal.border }),
            StrokeKind::Inside,
        );
    }

    painter.galley(
        Pos2::new(
            rect.left() + 31.0,
            rect.center().y - galley.size().y / 2.0,
        ),
        galley,
        text_color,
    );

    if enabled {
        response.on_hover_cursor(CursorIcon::PointingHand)
    } else {
        response
    }
}

/// 分段选择器（排序方式 / 显示方式 / 主题）。
pub fn segmented<T: PartialEq + Copy>(
    ui: &mut egui::Ui,
    pal: &Palette,
    value: &mut T,
    options: &[(T, &str)],
) -> bool {
    let mut changed = false;
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing = Vec2::new(6.0, 6.0);
        for (option, label) in options {
            let selected = *value == *option;
            let text_color = if selected { Color32::WHITE } else { pal.text };
            let btn = egui::Button::new(RichText::new(*label).size(13.0).color(text_color))
                .fill(if selected { pal.accent } else { pal.panel })
                .corner_radius(CornerRadius::same(7))
                .stroke(Stroke::new(
                    1.0_f32,
                    if selected { pal.accent } else { pal.border },
                ))
                .min_size(Vec2::new(0.0, 32.0));
            if ui.add(btn).on_hover_cursor(CursorIcon::PointingHand).clicked() {
                *value = *option;
                changed = true;
            }
        }
    });
    changed
}

// ---------------------------------------------------------------------------
// 主机列表
// ---------------------------------------------------------------------------

pub enum RowAction {
    None,
    Select(usize),
    Connect(usize),
    Edit(usize),
}

/// 行高（不含 item spacing）。虚拟化按这个高度算行，改这里就是改两处。
pub const LIST_ROW_HEIGHT: f32 = 78.0;
/// 方块卡片高度（不含 item spacing）。
pub const CARD_HEIGHT: f32 = 196.0;

pub fn draw_host_row(
    ui: &mut egui::Ui,
    pal: &Palette,
    host: &Host,
    index: usize,
    selected: bool,
    status: ProbeStatus,
    sort: SortMode,
    ensure_visible: bool,
) -> RowAction {
    let mut action = RowAction::None;
    // 不能用 ui.available_width()：在 ScrollArea 里它会把内容框算到窗口外
    // （实测返回 809.6，而可视区只有 700），按钮会被切掉。
    let clip = ui.clip_rect();
    let row_left = ui.cursor().min.x;
    let row_width = (clip.right() - row_left - 10.0).max(300.0);
    let (rect, response) = ui.allocate_exact_size(Vec2::new(row_width, LIST_ROW_HEIGHT), Sense::click());
    let hovered = response.hovered();

    if ensure_visible {
        response.scroll_to_me(Some(Align::Center));
    }

    let fill = if selected {
        pal.card_selected
    } else if hovered {
        pal.card_hover
    } else {
        pal.card
    };
    let stroke = if selected {
        Stroke::new(1.8_f32, pal.border_selected)
    } else {
        Stroke::new(1.0_f32, pal.border)
    };
    ui.painter().rect(
        rect,
        CornerRadius::same(12),
        fill,
        stroke,
        StrokeKind::Inside,
    );

    // 在线状态圆点
    let dot = Pos2::new(rect.left() + 22.0, rect.center().y);
    let dot_color = status_color(status, pal);
    ui.painter().circle_filled(dot, 5.5, dot_color);
    if status == ProbeStatus::Online {
        ui.painter()
            .circle_filled(dot, 9.0, dot_color.gamma_multiply(0.18));
    }
    // 圆点太小、又没有文字，鼠标停在上面时把状态说清楚。
    // 用 show_tooltip_at_pointer 而不是再加一个可交互矩形：后者会把整行的点击抢走。
    if let Some(pos) = ui.ctx().pointer_hover_pos() {
        if Rect::from_center_size(dot, Vec2::splat(24.0)).contains(pos) {
            egui::show_tooltip_at_pointer(
                ui.ctx(),
                ui.layer_id(),
                ui.id().with(("status-tip", index)),
                |ui| {
                    ui.label(status_hint(status));
                },
            );
        }
    }

    // 右侧按钮：只留「连接」「编辑」
    let connect_size = Vec2::new(86.0, 37.0);
    let edit_size = Vec2::new(56.0, 32.0);
    let right = rect.right() - 12.0;
    let connect_rect = Rect::from_min_size(
        Pos2::new(
            right - connect_size.x,
            rect.center().y - connect_size.y / 2.0,
        ),
        connect_size,
    );
    let edit_rect = Rect::from_min_size(
        Pos2::new(
            connect_rect.left() - 8.0 - edit_size.x,
            rect.center().y - edit_size.y / 2.0,
        ),
        edit_size,
    );

    let connect_id = ui.id().with(("connect", index));
    let edit_id = ui.id().with(("edit", index));
    let connect_resp = ui
        .interact(connect_rect, connect_id, Sense::click())
        .on_hover_cursor(CursorIcon::PointingHand);
    let edit_resp = ui
        .interact(edit_rect, edit_id, Sense::click())
        .on_hover_cursor(CursorIcon::PointingHand);

    let connect_fill = if connect_resp.hovered() {
        pal.accent.gamma_multiply(0.85)
    } else {
        pal.accent
    };
    ui.painter().rect(
        connect_rect,
        CornerRadius::same(9),
        connect_fill,
        Stroke::NONE,
        StrokeKind::Inside,
    );
    ui.painter().text(
        connect_rect.center(),
        egui::Align2::CENTER_CENTER,
        "连接",
        FontId::proportional(13.5),
        Color32::WHITE,
    );

    let edit_fill = if edit_resp.hovered() {
        pal.accent_soft
    } else {
        pal.panel
    };
    ui.painter().rect(
        edit_rect,
        CornerRadius::same(8),
        edit_fill,
        Stroke::new(1.0_f32, pal.border),
        StrokeKind::Inside,
    );
    ui.painter().text(
        edit_rect.center(),
        egui::Align2::CENTER_CENTER,
        "编辑",
        FontId::proportional(12.0),
        pal.text_dim,
    );

    // 文字区：裁到按钮左边，长名字不会压到按钮上
    let text_left = rect.left() + 42.0;
    let max_badges_right = edit_rect.left() - 12.0;
    let text_clip = Rect::from_min_max(
        Pos2::new(text_left, rect.top() + 2.0),
        Pos2::new(max_badges_right, rect.bottom() - 2.0),
    );
    let painter = ui.painter().with_clip_rect(text_clip);

    let name = host.display_name();
    // 名字超长时截断并留出 70px 给分组徽章，长名字的主机也不会「看起来没分组」。
    let name_max = (max_badges_right - text_left - 70.0).max(60.0);
    let name_galley = truncated_galley(
        &painter,
        name,
        FontId::proportional(15.5),
        pal.text,
        name_max,
    );
    painter.galley(Pos2::new(text_left, rect.top() + 13.0), name_galley.clone(), pal.text);

    // 分组徽章，最多两个，多出来的显示 +N
    let mut badge_x = text_left + name_galley.size().x + 8.0;
    let mut shown = 0usize;
    for group in &host.groups {
        if shown >= 2 {
            break;
        }
        let width = badge_width(&painter, group);
        if badge_x + width > max_badges_right {
            break;
        }
        draw_badge(&painter, pal, Pos2::new(badge_x, rect.top() + 12.0), group);
        badge_x += width + 6.0;
        shown += 1;
    }
    if host.groups.len() > shown {
        let more = format!("+{}", host.groups.len() - shown);
        let width = badge_width(&painter, &more);
        if badge_x + width <= max_badges_right {
            draw_muted_badge(&painter, pal, Pos2::new(badge_x, rect.top() + 12.0), &more);
        }
    }

    // 第二行：地址 · 用户名 ·（自定义排序时）序号
    let mut subtitle = host.display_address();
    if !host.username.trim().is_empty() {
        subtitle.push_str("  ·  ");
        subtitle.push_str(host.username.trim());
    }
    if sort == SortMode::Custom {
        subtitle.push_str(&format!("  ·  序号 {}", host.order));
    }
    let sub_galley = truncated_galley(
        &painter,
        &subtitle,
        FontId::proportional(12.5),
        pal.text_dim,
        (max_badges_right - text_left).max(40.0),
    );
    painter.galley(Pos2::new(text_left, rect.top() + 40.0), sub_galley, pal.text_dim);

    if connect_resp.clicked() {
        action = RowAction::Connect(index);
    } else if edit_resp.clicked() {
        action = RowAction::Edit(index);
    } else if response.clicked() {
        if is_host_double_click(ui, &host.id) {
            action = RowAction::Connect(index);
        } else {
            action = RowAction::Select(index);
        }
    }

    action
}

/// 方块视图的一张卡片。
pub fn draw_host_card(
    ui: &mut egui::Ui,
    pal: &Palette,
    host: &Host,
    index: usize,
    card_width: f32,
    selected: bool,
    status: ProbeStatus,
    sort: SortMode,
    ensure_visible: bool,
) -> RowAction {
    let mut action = RowAction::None;
    let (rect, response) =
        ui.allocate_exact_size(Vec2::new(card_width, CARD_HEIGHT), Sense::click());
    let hovered = response.hovered();
    if ensure_visible {
        response.scroll_to_me(Some(Align::Center));
    }

    let fill = if selected {
        pal.card_selected
    } else if hovered {
        pal.card_hover
    } else {
        pal.card
    };
    let stroke = if selected {
        Stroke::new(1.8_f32, pal.border_selected)
    } else {
        Stroke::new(1.0_f32, pal.border)
    };
    ui.painter().rect(
        rect,
        CornerRadius::same(14),
        fill,
        stroke,
        StrokeKind::Inside,
    );

    let painter = ui.painter().with_clip_rect(rect.shrink(10.0));

    // 顶部：状态 + 序号
    let dot = Pos2::new(rect.left() + 20.0, rect.top() + 22.0);
    let dot_color = status_color(status, pal);
    painter.circle_filled(dot, 5.5, dot_color);
    if status == ProbeStatus::Online {
        painter.circle_filled(dot, 9.0, dot_color.gamma_multiply(0.18));
    }
    painter.text(
        Pos2::new(rect.left() + 32.0, rect.top() + 15.0),
        egui::Align2::LEFT_TOP,
        status.label(),
        FontId::proportional(11.5),
        pal.text_dim,
    );
    if sort == SortMode::Custom {
        painter.text(
            Pos2::new(rect.right() - 12.0, rect.top() + 14.0),
            egui::Align2::RIGHT_TOP,
            format!("#{}", host.order),
            FontId::proportional(11.5),
            pal.text_dim,
        );
    }

    let text_width = (rect.width() - 44.0).max(60.0);
    let name_galley = truncated_galley(
        &painter,
        host.display_name(),
        FontId::proportional(16.5),
        pal.text,
        text_width,
    );
    painter.galley(Pos2::new(rect.left() + 18.0, rect.top() + 44.0), name_galley, pal.text);

    let addr_galley = truncated_galley(
        &painter,
        &host.display_address(),
        FontId::proportional(13.0),
        pal.text,
        text_width,
    );
    painter.galley(Pos2::new(rect.left() + 18.0, rect.top() + 72.0), addr_galley, pal.text);

    if !host.username.trim().is_empty() {
        let user_galley = truncated_galley(
            &painter,
            host.username.trim(),
            FontId::proportional(12.0),
            pal.text_dim,
            text_width,
        );
        painter.galley(Pos2::new(rect.left() + 18.0, rect.top() + 93.0), user_galley, pal.text_dim);
    }

    // 分组徽章
    let mut badge_x = rect.left() + 18.0;
    let max_right = rect.right() - 14.0;
    let mut shown = 0usize;
    for group in &host.groups {
        if shown >= 2 {
            break;
        }
        let width = badge_width(&painter, group);
        if badge_x + width > max_right {
            break;
        }
        draw_badge(
            &painter,
            pal,
            Pos2::new(badge_x, rect.top() + 116.0),
            group,
        );
        badge_x += width + 6.0;
        shown += 1;
    }
    if host.groups.len() > shown {
        let more = format!("+{}", host.groups.len() - shown);
        let width = badge_width(&painter, &more);
        if badge_x + width <= max_right {
            draw_muted_badge(
                &painter,
                pal,
                Pos2::new(badge_x, rect.top() + 116.0),
                &more,
            );
        }
    }

    // 底部按钮
    let connect_rect = Rect::from_min_size(
        Pos2::new(rect.left() + 16.0, rect.bottom() - 50.0),
        Vec2::new(100.0, 36.0),
    );
    let edit_rect = Rect::from_min_size(
        Pos2::new(connect_rect.right() + 8.0, rect.bottom() - 46.0),
        Vec2::new(58.0, 30.0),
    );

    let connect_resp = ui
        .interact(
            connect_rect,
            ui.id().with(("gconnect", index)),
            Sense::click(),
        )
        .on_hover_cursor(CursorIcon::PointingHand);
    let edit_resp = ui
        .interact(edit_rect, ui.id().with(("gedit", index)), Sense::click())
        .on_hover_cursor(CursorIcon::PointingHand);

    ui.painter().rect(
        connect_rect,
        CornerRadius::same(9),
        if connect_resp.hovered() {
            pal.accent.gamma_multiply(0.85)
        } else {
            pal.accent
        },
        Stroke::NONE,
        StrokeKind::Inside,
    );
    ui.painter().text(
        connect_rect.center(),
        egui::Align2::CENTER_CENTER,
        "连接",
        FontId::proportional(13.5),
        Color32::WHITE,
    );

    ui.painter().rect(
        edit_rect,
        CornerRadius::same(8),
        if edit_resp.hovered() {
            pal.accent_soft
        } else {
            pal.panel
        },
        Stroke::new(1.0_f32, pal.border),
        StrokeKind::Inside,
    );
    ui.painter().text(
        edit_rect.center(),
        egui::Align2::CENTER_CENTER,
        "编辑",
        FontId::proportional(12.0),
        pal.text_dim,
    );

    if connect_resp.clicked() {
        action = RowAction::Connect(index);
    } else if edit_resp.clicked() {
        action = RowAction::Edit(index);
    } else if response.clicked() {
        if is_host_double_click(ui, &host.id) {
            action = RowAction::Connect(index);
        } else {
            action = RowAction::Select(index);
        }
    }

    action
}

fn status_color(status: ProbeStatus, pal: &Palette) -> Color32 {
    match status {
        ProbeStatus::Online => pal.online,
        ProbeStatus::Offline => pal.offline,
        ProbeStatus::Checking | ProbeStatus::Disabled => pal.checking,
    }
}

fn status_hint(status: ProbeStatus) -> &'static str {
    match status {
        ProbeStatus::Online => "在线：远程桌面端口能连上",
        ProbeStatus::Offline => "离线：端口连不上（点「连接」仍可尝试）",
        ProbeStatus::Checking => "正在检测…",
        ProbeStatus::Disabled => "探测已关闭（设置 → 启动行为 → 探测可用性）",
    }
}

fn badge_width(painter: &egui::Painter, text: &str) -> f32 {
    painter
        .layout_no_wrap(
            text.to_string(),
            FontId::proportional(11.0),
            Color32::WHITE,
        )
        .size()
        .x
        + 13.0
}

fn draw_badge(painter: &egui::Painter, pal: &Palette, pos: Pos2, text: &str) {
    let galley = painter.layout_no_wrap(
        text.to_string(),
        FontId::proportional(11.0),
        pal.accent,
    );
    let rect = Rect::from_min_size(pos, Vec2::new(galley.size().x + 13.0, 19.0));
    painter.rect(
        rect,
        CornerRadius::same(6),
        pal.accent_soft,
        Stroke::NONE,
        StrokeKind::Inside,
    );
    painter.galley(
        Pos2::new(rect.left() + 6.5, rect.center().y - galley.size().y / 2.0),
        galley,
        pal.accent,
    );
}

fn draw_muted_badge(painter: &egui::Painter, pal: &Palette, pos: Pos2, text: &str) {
    let galley = painter.layout_no_wrap(
        text.to_string(),
        FontId::proportional(11.0),
        pal.text_dim,
    );
    let rect = Rect::from_min_size(pos, Vec2::new(galley.size().x + 12.0, 19.0));
    painter.rect(
        rect,
        CornerRadius::same(6),
        pal.input_bg,
        Stroke::new(1.0_f32, pal.border),
        StrokeKind::Inside,
    );
    painter.galley(
        Pos2::new(rect.left() + 6.0, rect.center().y - galley.size().y / 2.0),
        galley,
        pal.text_dim,
    );
}

// ---------------------------------------------------------------------------
// 底部 / 空状态 / Toast
// ---------------------------------------------------------------------------

pub fn draw_footer(
    ui: &mut egui::Ui,
    pal: &Palette,
    total: usize,
    visible: usize,
    sort: SortMode,
    view: ViewMode,
    version: &str,
) -> bool {
    let mut about_clicked = false;
    ui.horizontal(|ui| {
        ui.add_space(2.0);
        let hint = RichText::new("↑/↓ 选择   ·   Enter 连接   ·   双击直连   ·   F5 重新检测")
            .size(11.5)
            .color(pal.text_dim);
        ui.label(hint);
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            let version_btn = egui::Button::new(
                RichText::new(format!("v{version}"))
                    .size(11.0)
                    .color(pal.text_dim),
            )
            .fill(Color32::TRANSPARENT)
            .stroke(Stroke::NONE)
            .min_size(Vec2::new(0.0, 22.0));
            if ui
                .add(version_btn)
                .on_hover_text("关于 RDP 启动器")
                .clicked()
            {
                about_clicked = true;
            }

            let info = format!(
                "{}/{} 台   ·   {}排序   ·   {}",
                visible,
                total,
                sort.label(),
                view.label()
            );
            ui.label(RichText::new(info).size(11.5).color(pal.text_dim));
        });
    });
    about_clicked
}

/// 空状态：没有主机时同时提示「新增」和「导入本机历史」。
/// 返回 true 表示用户点了「导入本机远程桌面历史」。
pub fn draw_empty_state(
    ui: &mut egui::Ui,
    pal: &Palette,
    filtered: bool,
    has_hosts: bool,
) -> bool {
    let mut import_clicked = false;
    ui.vertical_centered(|ui| {
        ui.add_space(56.0);
        if !has_hosts {
            ui.label(
                RichText::new("还没有主机")
                    .size(16.0)
                    .strong()
                    .color(pal.text),
            );
            ui.add_space(6.0);
            ui.label(
                RichText::new("点右上角「+ 新增主机」新建，或导入本机远程桌面里连过的机器：")
                    .size(12.5)
                    .color(pal.text_dim),
            );
            ui.add_space(14.0);
            if ui
                .add(
                    egui::Button::new(
                        RichText::new("导入本机远程桌面历史")
                            .size(12.5)
                            .color(Color32::WHITE),
                    )
                    .fill(pal.accent)
                    .corner_radius(CornerRadius::same(8))
                    .min_size(Vec2::new(0.0, 34.0)),
                )
                .on_hover_text("读 mstsc 的最近连接记录，一次导入成主机")
                .clicked()
            {
                import_clicked = true;
            }
        } else if filtered {
            ui.label(
                RichText::new("没有匹配的主机\n换个关键词，或点「全部」清掉分组过滤")
                    .size(13.5)
                    .color(pal.text_dim),
            );
        } else {
            ui.label(
                RichText::new("这个分组里还没有主机\n编辑主机时可以同时勾选多个分组")
                    .size(13.5)
                    .color(pal.text_dim),
            );
        }
    });
    import_clicked
}

pub struct Toast {
    pub message: String,
    pub is_error: bool,
    pub created_at: std::time::Instant,
}

impl Toast {
    pub fn new(message: impl Into<String>, is_error: bool) -> Self {
        Self {
            message: message.into(),
            is_error,
            created_at: std::time::Instant::now(),
        }
    }

    pub fn expired(&self) -> bool {
        self.created_at.elapsed().as_secs_f32() > 4.5
    }

    pub fn alpha(&self) -> f32 {
        let age = self.created_at.elapsed().as_secs_f32();
        if age > 3.7 {
            ((4.5 - age) / 0.8).clamp(0.0, 1.0)
        } else {
            1.0
        }
    }
}

/// 右下角竖排显示所有未过期提示（新的在最下面）。
///
/// 以前固定一条并锚在顶部正中，会压住搜索框；同帧产生两条时后一条还会直接顶掉前一条。
pub fn draw_toasts(ctx: &egui::Context, pal: &Palette, toasts: &[Toast]) {
    if toasts.is_empty() {
        return;
    }
    egui::Area::new(egui::Id::new("toast"))
        .anchor(egui::Align2::RIGHT_BOTTOM, Vec2::new(-18.0, -46.0))
        .interactable(false)
        .show(ctx, |ui| {
            ui.set_max_width(420.0);
            ui.with_layout(Layout::top_down(Align::Max), |ui| {
                for toast in toasts {
                    let alpha = toast.alpha();
                    let fill = if toast.is_error {
                        pal.danger
                    } else {
                        pal.accent
                    }
                    .gamma_multiply(0.95 * alpha);
                    let text_color = Color32::WHITE.gamma_multiply(alpha);

                    Frame::new()
                        .fill(fill)
                        .corner_radius(CornerRadius::same(10))
                        .inner_margin(Margin::symmetric(16, 10))
                        .show(ui, |ui| {
                            ui.label(RichText::new(&toast.message).size(13.0).color(text_color));
                        });
                }
            });
        });
}

/// 给主界面套一层统一背景
pub fn apply_style(ctx: &egui::Context, pal: &Palette) {
    let mut style = (*ctx.style()).clone();
    style.visuals.override_text_color = Some(pal.text);
    style.visuals.panel_fill = pal.bg;
    style.visuals.window_fill = pal.panel;
    style.visuals.extreme_bg_color = pal.input_bg;
    style.visuals.widgets.inactive.bg_fill = pal.panel;
    style.visuals.widgets.inactive.fg_stroke = Stroke::new(1.0_f32, pal.border);
    style.visuals.widgets.hovered.bg_fill = pal.card_hover;
    style.visuals.selection.bg_fill = pal.accent_soft;
    style.visuals.selection.stroke = Stroke::new(1.0_f32, pal.accent);
    style.spacing.item_spacing = Vec2::new(8.0, 8.0);
    style.spacing.button_padding = Vec2::new(12.0, 6.0);
    style.spacing.interact_size.y = 30.0;
    ctx.set_style(style);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn palette_differs_between_themes() {
        assert_ne!(palette(true).bg, palette(false).bg);
    }

    #[test]
    fn toast_expires() {
        let t = Toast::new("x", false);
        assert!(!t.expired());
        assert!((t.alpha() - 1.0).abs() < 0.01);
    }
}
