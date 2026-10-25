//! Overlay placement is resolved against the final Rust viewport and measured content.
use pi_plugin::{UiNode, UiOverlayAnchor, UiOverlayOptions, UiOverlayPosition};
use ratatui::layout::Rect;

use super::super::super::UiPalette;
use super::runtime;

pub(super) fn bounds(
    root: Rect,
    node: &UiNode,
    options: &UiOverlayOptions,
    palette: UiPalette,
) -> Rect {
    let left = options.margin.left.min(root.width);
    let top = options.margin.top.min(root.height);
    let available_width = root
        .width
        .saturating_sub(left)
        .saturating_sub(options.margin.right);
    let available_height = root
        .height
        .saturating_sub(top)
        .saturating_sub(options.margin.bottom);
    let width = options
        .width
        .unwrap_or(root.width.saturating_sub(4).max(1))
        .min(available_width);
    let height = runtime::measure(node, width.max(1), palette)
        .min(options.max_height.unwrap_or(available_height))
        .min(available_height);
    let (horizontal, vertical) = match options.anchor {
        UiOverlayAnchor::TopLeft => (0, 0),
        UiOverlayAnchor::TopCenter => (1, 0),
        UiOverlayAnchor::TopRight => (2, 0),
        UiOverlayAnchor::LeftCenter => (0, 1),
        UiOverlayAnchor::Center => (1, 1),
        UiOverlayAnchor::RightCenter => (2, 1),
        UiOverlayAnchor::BottomLeft => (0, 2),
        UiOverlayAnchor::BottomCenter => (1, 2),
        UiOverlayAnchor::BottomRight => (2, 2),
    };
    let col = position(
        options.col,
        left,
        available_width - width,
        horizontal,
        options.offset_x,
    );
    let row = position(
        options.row,
        top,
        available_height - height,
        vertical,
        options.offset_y,
    );
    Rect::new(
        root.x.saturating_add(col),
        root.y.saturating_add(row),
        width,
        height,
    )
}

fn position(
    value: Option<UiOverlayPosition>,
    margin: u16,
    travel: u16,
    anchor: u16,
    offset: i32,
) -> u16 {
    let start = i64::from(margin);
    let end = start + i64::from(travel);
    let value = match value {
        Some(UiOverlayPosition::Cells(value)) => i64::from(value),
        Some(UiOverlayPosition::Percent(value)) => {
            start + i64::from(travel) * i64::from(value.min(10_000)) / 10_000
        }
        None => start + i64::from(travel) * i64::from(anchor) / 2,
    };
    (value + i64::from(offset)).clamp(start, end) as u16
}

#[cfg(test)]
mod tests {
    use super::*;
    use pi_plugin::{UiNodeKind, UiOverlayMargin};

    fn content() -> UiNode {
        UiNode::new(
            "content",
            UiNodeKind::Custom {
                lines: vec!["row".into(); 5],
                interactive: true,
                mouse: false,
                instance_id: String::new(),
                key_release: false,
                cursor: None,
            },
        )
    }

    #[test]
    fn positioning_respects_content_margins_percentages_and_offsets() {
        let palette = UiPalette::from_background(None);
        let mut options = UiOverlayOptions {
            width: Some(20),
            anchor: UiOverlayAnchor::BottomRight,
            margin: UiOverlayMargin {
                top: 1,
                right: 3,
                bottom: 2,
                left: 4,
            },
            ..Default::default()
        };
        let root = Rect::new(0, 0, 80, 24);
        assert_eq!(
            bounds(root, &content(), &options, palette),
            Rect::new(57, 17, 20, 5)
        );
        options.row = Some(UiOverlayPosition::Percent(5_000));
        options.col = Some(UiOverlayPosition::Percent(0));
        options.offset_x = 2;
        assert_eq!(
            bounds(root, &content(), &options, palette),
            Rect::new(6, 9, 20, 5)
        );
        options.offset_x = i32::MAX;
        options.offset_y = i32::MIN;
        assert_eq!(
            bounds(root, &content(), &options, palette),
            Rect::new(57, 1, 20, 5)
        );
        let small = bounds(Rect::new(0, 0, 6, 3), &content(), &options, palette);
        assert_eq!(small.intersection(Rect::new(0, 0, 6, 3)), small);
    }
}
