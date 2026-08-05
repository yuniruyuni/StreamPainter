//! 描画モード中の右クリックコンテキストメニュー。
//! ツール切替 / カラーパレット / 元に戻す / 全消去 / 終了。

use windows::core::HSTRING;
use windows::Win32::Foundation::{HWND, POINT};
use windows::Win32::UI::WindowsAndMessaging::{
    AppendMenuW, CreatePopupMenu, DestroyMenu, GetCursorPos, SetForegroundWindow, TrackPopupMenu,
    HMENU, MF_CHECKED, MF_POPUP, MF_SEPARATOR, MF_STRING, TPM_NONOTIFY, TPM_RETURNCMD,
    TPM_RIGHTBUTTON,
};

use crate::config::StampConfig;
use crate::protocol::{CanvasLayer, MAX_LAYERS};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DrawTool {
    Select,
    Pen,
    Marker,
    Eraser,
    Line,
    Arrow,
    Rectangle,
    Ellipse,
    Stamp(String),
}

/// カラーパレット (デザインシステム準拠の視認性の高い色)
pub const COLORS: [(&str, &str); 10] = [
    ("ピンク", "#ff4d6d"),
    ("レッド", "#e5484d"),
    ("オレンジ", "#ff8a3d"),
    ("イエロー", "#ffd43b"),
    ("グリーン", "#51cf66"),
    ("スカイ", "#3bb2ed"),
    ("ブルー", "#4c6ef5"),
    ("パープル", "#9775fa"),
    ("ホワイト", "#ffffff"),
    ("ブラック", "#111111"),
];

#[derive(Debug, Clone, PartialEq)]
pub enum MenuAction {
    SelectTool(DrawTool),
    SelectColor(&'static str),
    SelectLayer(String),
    AddLayer,
    DeleteLayer(String),
    Undo,
    Redo,
    Clear,
    Exit,
}

const ID_TOOL_SELECT: usize = 9;
const ID_TOOL_PEN: usize = 10;
const ID_TOOL_MARKER: usize = 11;
const ID_TOOL_ERASER: usize = 12;
const ID_TOOL_LINE: usize = 13;
const ID_TOOL_ARROW: usize = 14;
const ID_TOOL_RECTANGLE: usize = 15;
const ID_TOOL_ELLIPSE: usize = 16;
const ID_UNDO: usize = 20;
const ID_REDO: usize = 21;
const ID_CLEAR: usize = 22;
const ID_EXIT: usize = 30;
const ID_COLOR_BASE: usize = 100;
const ID_STAMP_BASE: usize = 1000;
const ID_LAYER_ADD: usize = 2000;
const ID_LAYER_DELETE: usize = 2001;
const ID_LAYER_BASE: usize = 2100;

fn checked(flag: bool) -> windows::Win32::UI::WindowsAndMessaging::MENU_ITEM_FLAGS {
    if flag {
        MF_STRING | MF_CHECKED
    } else {
        MF_STRING
    }
}

fn enabled(flag: bool) -> windows::Win32::UI::WindowsAndMessaging::MENU_ITEM_FLAGS {
    if flag {
        MF_STRING
    } else {
        MF_STRING | windows::Win32::UI::WindowsAndMessaging::MF_GRAYED
    }
}

fn append(
    menu: HMENU,
    flags: windows::Win32::UI::WindowsAndMessaging::MENU_ITEM_FLAGS,
    id: usize,
    label: &str,
) {
    unsafe {
        let _ = AppendMenuW(menu, flags, id, &HSTRING::from(label));
    }
}

pub struct LayerMenuState<'a> {
    pub layers: &'a [CanvasLayer],
    pub item_counts: &'a [usize],
    pub active_layer_id: &'a str,
}

/// メニューを表示し、選択されたアクションを返す (選択なしは None)
pub fn show(
    hwnd: HWND,
    tool: &DrawTool,
    color: &str,
    stamps: &[StampConfig],
    layer_state: LayerMenuState<'_>,
    can_undo: bool,
    can_redo: bool,
    can_clear: bool,
) -> Option<MenuAction> {
    let LayerMenuState {
        layers,
        item_counts: layer_item_counts,
        active_layer_id,
    } = layer_state;
    // TrackPopupMenu の内部メッセージループ中に通常の projector timer が動いても、
    // overlay をこの popup より前へ移動させない。
    let _foreground_ui = crate::win::projector::ForegroundUiGuard::new();
    unsafe {
        let root = CreatePopupMenu().ok()?;
        append(
            root,
            checked(tool == &DrawTool::Select),
            ID_TOOL_SELECT,
            "選択・移動",
        );
        let _ = AppendMenuW(root, MF_SEPARATOR, 0, None);
        append(root, checked(tool == &DrawTool::Pen), ID_TOOL_PEN, "ペン");
        append(
            root,
            checked(tool == &DrawTool::Marker),
            ID_TOOL_MARKER,
            "マーカー",
        );
        append(
            root,
            checked(tool == &DrawTool::Eraser),
            ID_TOOL_ERASER,
            "消しゴム",
        );

        let shapes = CreatePopupMenu().ok()?;
        append(
            shapes,
            checked(tool == &DrawTool::Line),
            ID_TOOL_LINE,
            "直線",
        );
        append(
            shapes,
            checked(tool == &DrawTool::Arrow),
            ID_TOOL_ARROW,
            "矢印",
        );
        append(
            shapes,
            checked(tool == &DrawTool::Rectangle),
            ID_TOOL_RECTANGLE,
            "四角形",
        );
        append(
            shapes,
            checked(tool == &DrawTool::Ellipse),
            ID_TOOL_ELLIPSE,
            "楕円",
        );
        let _ = AppendMenuW(root, MF_POPUP, shapes.0 as usize, &HSTRING::from("図形"));

        if !stamps.is_empty() {
            let stamp_menu = CreatePopupMenu().ok()?;
            for (index, stamp) in stamps.iter().enumerate() {
                let label = stamp.name.replace('&', "&&");
                append(
                    stamp_menu,
                    checked(matches!(tool, DrawTool::Stamp(id) if id == &stamp.id)),
                    ID_STAMP_BASE + index,
                    &label,
                );
            }
            let _ = AppendMenuW(
                root,
                MF_POPUP,
                stamp_menu.0 as usize,
                &HSTRING::from("スタンプ"),
            );
        }
        let layer_menu = CreatePopupMenu().ok()?;
        for (index, layer) in layers.iter().enumerate().rev() {
            let count = layer_item_counts.get(index).copied().unwrap_or_default();
            let label = format!("{} ({count})", layer.name.replace('&', "&&"));
            append(
                layer_menu,
                checked(layer.layer_id == active_layer_id),
                ID_LAYER_BASE + index,
                &label,
            );
        }
        let _ = AppendMenuW(layer_menu, MF_SEPARATOR, 0, None);
        append(
            layer_menu,
            enabled(layers.len() < MAX_LAYERS),
            ID_LAYER_ADD,
            "新規レイヤー",
        );
        append(
            layer_menu,
            enabled(layers.len() > 1),
            ID_LAYER_DELETE,
            "現在のレイヤーを削除...",
        );
        let _ = AppendMenuW(
            root,
            MF_POPUP,
            layer_menu.0 as usize,
            &HSTRING::from("レイヤー"),
        );
        let _ = AppendMenuW(root, MF_SEPARATOR, 0, None);

        let palette = CreatePopupMenu().ok()?;
        for (i, (name, hex)) in COLORS.iter().enumerate() {
            append(
                palette,
                checked(hex.eq_ignore_ascii_case(color)),
                ID_COLOR_BASE + i,
                name,
            );
        }
        let _ = AppendMenuW(root, MF_POPUP, palette.0 as usize, &HSTRING::from("色"));
        let _ = AppendMenuW(root, MF_SEPARATOR, 0, None);

        append(root, enabled(can_undo), ID_UNDO, "元に戻す");
        append(root, enabled(can_redo), ID_REDO, "やり直す");
        append(root, enabled(can_clear), ID_CLEAR, "全消去...");
        let _ = AppendMenuW(root, MF_SEPARATOR, 0, None);
        append(root, MF_STRING, ID_EXIT, "終了");

        let mut pos = POINT::default();
        let _ = GetCursorPos(&mut pos);
        let _ = SetForegroundWindow(hwnd);
        let selected = TrackPopupMenu(
            root,
            TPM_RIGHTBUTTON | TPM_RETURNCMD | TPM_NONOTIFY,
            pos.x,
            pos.y,
            None,
            hwnd,
            None,
        );
        let _ = DestroyMenu(root);

        match selected.0 as usize {
            ID_TOOL_SELECT => Some(MenuAction::SelectTool(DrawTool::Select)),
            ID_TOOL_PEN => Some(MenuAction::SelectTool(DrawTool::Pen)),
            ID_TOOL_MARKER => Some(MenuAction::SelectTool(DrawTool::Marker)),
            ID_TOOL_ERASER => Some(MenuAction::SelectTool(DrawTool::Eraser)),
            ID_TOOL_LINE => Some(MenuAction::SelectTool(DrawTool::Line)),
            ID_TOOL_ARROW => Some(MenuAction::SelectTool(DrawTool::Arrow)),
            ID_TOOL_RECTANGLE => Some(MenuAction::SelectTool(DrawTool::Rectangle)),
            ID_TOOL_ELLIPSE => Some(MenuAction::SelectTool(DrawTool::Ellipse)),
            ID_UNDO => Some(MenuAction::Undo),
            ID_REDO => Some(MenuAction::Redo),
            ID_CLEAR => Some(MenuAction::Clear),
            ID_LAYER_ADD if layers.len() < MAX_LAYERS => Some(MenuAction::AddLayer),
            ID_LAYER_DELETE if layers.len() > 1 => {
                Some(MenuAction::DeleteLayer(active_layer_id.to_owned()))
            }
            ID_EXIT => Some(MenuAction::Exit),
            id if (ID_COLOR_BASE..ID_COLOR_BASE + COLORS.len()).contains(&id) => {
                Some(MenuAction::SelectColor(COLORS[id - ID_COLOR_BASE].1))
            }
            id if (ID_STAMP_BASE..ID_STAMP_BASE + stamps.len()).contains(&id) => Some(
                MenuAction::SelectTool(DrawTool::Stamp(stamps[id - ID_STAMP_BASE].id.clone())),
            ),
            id if (ID_LAYER_BASE..ID_LAYER_BASE + layers.len()).contains(&id) => Some(
                MenuAction::SelectLayer(layers[id - ID_LAYER_BASE].layer_id.clone()),
            ),
            _ => None,
        }
    }
}
