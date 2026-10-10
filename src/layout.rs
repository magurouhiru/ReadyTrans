//! OCR の行をまとまり（段落）にまとめる。行単位より段落単位の方が自然に訳せる。

#[derive(Debug, Clone, PartialEq)]
pub struct TextBlock {
    pub text: String,
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

impl TextBlock {
    pub fn right(&self) -> f32 {
        self.x + self.w
    }
    pub fn bottom(&self) -> f32 {
        self.y + self.h
    }
}

fn same_paragraph(prev: &TextBlock, line: &TextBlock, line_h: f32) -> bool {
    let gap = line.y - prev.bottom();
    if gap < -line_h * 0.5 || gap > line_h * 0.8 {
        return false;
    }
    // 横方向に重なっていること（別カラムの行は混ぜない）
    let overlap = prev.right().min(line.right()) - prev.x.max(line.x);
    overlap > 0.0 && (line.x - prev.x).abs() < line_h * 3.0
}

pub fn group_lines(lines: &[TextBlock]) -> Vec<TextBlock> {
    let mut lines: Vec<&TextBlock> = lines.iter().filter(|l| !l.text.trim().is_empty()).collect();
    lines.sort_by(|a, b| a.y.total_cmp(&b.y).then(a.x.total_cmp(&b.x)));
    let mut paragraphs: Vec<Vec<&TextBlock>> = Vec::new();
    'outer: for line in lines {
        for para in paragraphs.iter_mut() {
            let last = para[para.len() - 1];
            if same_paragraph(last, line, last.h.max(line.h)) {
                para.push(line);
                continue 'outer;
            }
        }
        paragraphs.push(vec![line]);
    }

    paragraphs
        .into_iter()
        .map(|para| {
            let x = para.iter().map(|l| l.x).fold(f32::INFINITY, f32::min);
            let y = para.iter().map(|l| l.y).fold(f32::INFINITY, f32::min);
            let right = para.iter().map(|l| l.right()).fold(f32::NEG_INFINITY, f32::max);
            let bottom = para.iter().map(|l| l.bottom()).fold(f32::NEG_INFINITY, f32::max);
            let text = para.iter().map(|l| l.text.trim()).collect::<Vec<_>>().join(" ");
            TextBlock { text, x, y, w: right - x, h: bottom - y }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tb(text: &str, x: f32, y: f32, w: f32, h: f32) -> TextBlock {
        TextBlock { text: text.into(), x, y, w, h }
    }

    #[test]
    fn groups_consecutive_lines() {
        let lines = [
            tb("Hello", 10.0, 10.0, 100.0, 20.0),
            tb("world", 10.0, 34.0, 80.0, 20.0),
            tb("Separate", 10.0, 120.0, 100.0, 20.0),
        ];
        let blocks = group_lines(&lines);
        assert_eq!(blocks.len(), 2);
        assert_eq!(blocks[0].text, "Hello world");
        assert_eq!(blocks[0].h, 44.0);
        assert_eq!(blocks[1].text, "Separate");
    }

    #[test]
    fn keeps_columns_apart() {
        let lines = [tb("Left", 0.0, 0.0, 100.0, 20.0), tb("Right", 400.0, 22.0, 100.0, 20.0)];
        assert_eq!(group_lines(&lines).len(), 2);
    }

    #[test]
    fn drops_blank_lines() {
        let lines = [tb("  ", 0.0, 0.0, 10.0, 10.0), tb("A", 0.0, 0.0, 10.0, 10.0)];
        assert_eq!(group_lines(&lines).len(), 1);
    }
}
