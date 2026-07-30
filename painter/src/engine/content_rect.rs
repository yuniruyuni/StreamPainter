//! content rect の計算 (docs/protocol.md)。
//! 全画面プロジェクターはキャンバスをアスペクトフィット表示するため、
//! 「キャンバスが実際に表示されている矩形」を求め、その中で正規化する。

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Rect {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

impl Rect {
    pub fn contains(&self, x: f64, y: f64) -> bool {
        x >= self.x && x < self.x + self.width && y >= self.y && y < self.y + self.height
    }

    /// スクリーン物理座標 → 正規化座標 (範囲外も許容する)
    pub fn normalize(&self, x: f64, y: f64) -> (f64, f64) {
        ((x - self.x) / self.width, (y - self.y) / self.height)
    }
}

/// スクリーンサイズとキャンバスアスペクト比からアスペクトフィット矩形を求める
pub fn content_rect(screen_w: f64, screen_h: f64, aspect_w: f64, aspect_h: f64) -> Rect {
    let screen_aspect = screen_w / screen_h;
    let canvas_aspect = aspect_w / aspect_h;
    if screen_aspect > canvas_aspect {
        // スクリーンの方が横長 → 左右に黒帯
        let width = screen_h * canvas_aspect;
        Rect {
            x: (screen_w - width) / 2.0,
            y: 0.0,
            width,
            height: screen_h,
        }
    } else {
        // スクリーンの方が縦長 → 上下に黒帯
        let height = screen_w / canvas_aspect;
        Rect {
            x: 0.0,
            y: (screen_h - height) / 2.0,
            width: screen_w,
            height,
        }
    }
}

/// "16:9" 形式のアスペクト比指定をパースする
pub fn parse_aspect(s: &str) -> Option<(f64, f64)> {
    let (w, h) = s.split_once(':')?;
    let w: f64 = w.trim().parse().ok()?;
    let h: f64 = h.trim().parse().ok()?;
    if !w.is_finite() || !h.is_finite() || w <= 0.0 || h <= 0.0 {
        return None;
    }
    Some((w, h))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn same_aspect_fills_screen() {
        let r = content_rect(1920.0, 1080.0, 16.0, 9.0);
        assert_eq!(
            r,
            Rect {
                x: 0.0,
                y: 0.0,
                width: 1920.0,
                height: 1080.0
            }
        );
    }

    #[test]
    fn wider_screen_pillarboxes() {
        // 21:9 スクリーンに 16:9 キャンバス → 左右黒帯
        let r = content_rect(2560.0, 1080.0, 16.0, 9.0);
        assert_eq!(r.height, 1080.0);
        assert_eq!(r.width, 1920.0);
        assert_eq!(r.x, 320.0);
        assert_eq!(r.y, 0.0);
    }

    #[test]
    fn taller_screen_letterboxes() {
        // 16:10 スクリーンに 16:9 キャンバス → 上下黒帯
        let r = content_rect(1920.0, 1200.0, 16.0, 9.0);
        assert_eq!(r.width, 1920.0);
        assert_eq!(r.height, 1080.0);
        assert_eq!(r.x, 0.0);
        assert_eq!(r.y, 60.0);
    }

    #[test]
    fn normalize_maps_corners() {
        let r = content_rect(2560.0, 1080.0, 16.0, 9.0);
        assert_eq!(r.normalize(320.0, 0.0), (0.0, 0.0));
        assert_eq!(r.normalize(2240.0, 1080.0), (1.0, 1.0));
        // 黒帯上は範囲外の値になる
        let (u, _) = r.normalize(0.0, 0.0);
        assert!(u < 0.0);
    }

    #[test]
    fn parse_aspect_variants() {
        assert_eq!(parse_aspect("16:9"), Some((16.0, 9.0)));
        assert_eq!(parse_aspect("4:3"), Some((4.0, 3.0)));
        assert_eq!(parse_aspect("bogus"), None);
        assert_eq!(parse_aspect("0:9"), None);
        assert_eq!(parse_aspect("NaN:9"), None);
    }
}
