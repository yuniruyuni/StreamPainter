//! 描画モード中の右クリックコンテキストメニュー。
//! ツール切替 / カラーパレット / 元に戻す / 全消去 / 終了。

use windows::core::HSTRING;
use windows::Win32::Foundation::{HWND, POINT};
use windows::Win32::UI::WindowsAndMessaging::{
    AppendMenuW, CreatePopupMenu, DestroyMenu, GetCursorPos, SetForegroundWindow, TrackPopupMenu,
    HMENU, MF_CHECKED, MF_POPUP, MF_SEPARATOR, MF_STRING, TPM_NONOTIFY, TPM_RETURNCMD,
    TPM_RIGHTBUTTON,
};

use crate::protocol::Tool;

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
    SelectTool(Tool),
    SelectColor(&'static str),
    Undo,
    Clear,
    Exit,
}

const ID_TOOL_PEN: usize = 10;
const ID_TOOL_MARKER: usize = 11;
const ID_TOOL_ERASER: usize = 12;
const ID_UNDO: usize = 20;
const ID_CLEAR: usize = 21;
const ID_EXIT: usize = 30;
const ID_COLOR_BASE: usize = 100;

fn checked(flag: bool) -> windows::Win32::UI::WindowsAndMessaging::MENU_ITEM_FLAGS {
    if flag {
        MF_STRING | MF_CHECKED
    } else {
        MF_STRING
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

/// メニューを表示し、選択されたアクションを返す (選択なしは None)
pub fn show(hwnd: HWND, tool: &Tool, color: &str) -> Option<MenuAction> {
    unsafe {
        let root = CreatePopupMenu().ok()?;
        append(root, checked(*tool == Tool::Pen), ID_TOOL_PEN, "ペン");
        append(
            root,
            checked(*tool == Tool::Marker),
            ID_TOOL_MARKER,
            "マーカー",
        );
        append(
            root,
            checked(*tool == Tool::Eraser),
            ID_TOOL_ERASER,
            "消しゴム",
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

        append(root, MF_STRING, ID_UNDO, "元に戻す");
        append(root, MF_STRING, ID_CLEAR, "全消去");
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
            ID_TOOL_PEN => Some(MenuAction::SelectTool(Tool::Pen)),
            ID_TOOL_MARKER => Some(MenuAction::SelectTool(Tool::Marker)),
            ID_TOOL_ERASER => Some(MenuAction::SelectTool(Tool::Eraser)),
            ID_UNDO => Some(MenuAction::Undo),
            ID_CLEAR => Some(MenuAction::Clear),
            ID_EXIT => Some(MenuAction::Exit),
            id if (ID_COLOR_BASE..ID_COLOR_BASE + COLORS.len()).contains(&id) => {
                Some(MenuAction::SelectColor(COLORS[id - ID_COLOR_BASE].1))
            }
            _ => None,
        }
    }
}
