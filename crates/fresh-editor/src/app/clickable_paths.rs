//! Ctrl/Cmd-click opening for file paths and markdown links.
//!
//! This is intentionally isolated from the normal mouse editor flow so this
//! fork can keep the upstream mouse handler close to Fresh.

use super::*;
use anyhow::Result as AnyhowResult;
use crossterm::event::KeyModifiers;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use std::ops::Range;
use std::path::PathBuf;

use crate::view::overlay::{Overlay, OverlayFace};

pub(super) fn has_open_path_modifier(modifiers: KeyModifiers) -> bool {
    modifiers.intersects(KeyModifiers::CONTROL | KeyModifiers::SUPER)
}

impl Editor {
    pub(super) fn update_clickable_path_hover(
        &mut self,
        col: u16,
        row: u16,
        modifiers: KeyModifiers,
    ) -> AnyhowResult<bool> {
        if !has_open_path_modifier(modifiers) {
            return Ok(self.clear_clickable_path_hover());
        }

        let split_areas = self.cached_layout.split_areas.clone();
        for (split_id, buffer_id, content_rect, _, _, _) in split_areas {
            if !rect_contains(content_rect, col, row) {
                continue;
            }

            let Some(target) = self.clickable_target_at_screen_position(
                col,
                row,
                split_id,
                buffer_id,
                content_rect,
            )?
            else {
                return Ok(self.clear_clickable_path_hover());
            };

            return Ok(self.set_clickable_path_hover(buffer_id, target.highlight_range));
        }

        Ok(self.clear_clickable_path_hover())
    }

    pub(super) fn open_click_target_at_screen_position(
        &mut self,
        col: u16,
        row: u16,
        split_id: crate::model::event::LeafId,
        buffer_id: crate::model::event::BufferId,
        content_rect: Rect,
    ) -> AnyhowResult<bool> {
        let Some(target) =
            self.clickable_target_at_screen_position(col, row, split_id, buffer_id, content_rect)?
        else {
            return Ok(false);
        };
        let Some(open_target) = resolve_open_target(&target.raw_target) else {
            return Ok(false);
        };

        let display_target = open_target.display();
        if let Err(e) = open_target.open() {
            self.set_status_message(format!("Failed to open path: {}", e));
        } else {
            self.set_status_message(format!("Opening: {}", display_target));
        }

        Ok(true)
    }

    fn clickable_target_at_screen_position(
        &self,
        col: u16,
        row: u16,
        split_id: crate::model::event::LeafId,
        buffer_id: crate::model::event::BufferId,
        content_rect: Rect,
    ) -> AnyhowResult<Option<ClickableTarget>> {
        let Some(state) = self.buffers.get(&buffer_id) else {
            return Ok(None);
        };
        let Some(text) = state.buffer.to_string() else {
            return Ok(None);
        };

        let cached_mappings = self
            .cached_layout
            .view_line_mappings
            .get(&split_id)
            .cloned();
        let gutter_width = state.margins.left_total_width() as u16;
        let fallback = self
            .split_view_states
            .get(&split_id)
            .map(|vs| vs.viewport.top_byte)
            .unwrap_or(0);
        let compose_width = self
            .split_view_states
            .get(&split_id)
            .and_then(|vs| vs.compose_width);

        let Some(byte_pos) = super::click_geometry::screen_to_buffer_position(
            col,
            row,
            content_rect,
            gutter_width,
            &cached_mappings,
            fallback,
            false,
            compose_width,
        ) else {
            return Ok(None);
        };

        Ok(clickable_target_with_range_at_byte(&text, byte_pos))
    }

    fn set_clickable_path_hover(
        &mut self,
        buffer_id: crate::model::event::BufferId,
        range: Range<usize>,
    ) -> bool {
        if range.is_empty() {
            return self.clear_clickable_path_hover();
        }

        if self
            .mouse_state
            .clickable_path_hover
            .as_ref()
            .is_some_and(|hover| hover.buffer_id == buffer_id && hover.range == range)
        {
            return false;
        }

        self.clear_clickable_path_hover();

        let Some(state) = self.buffers.get_mut(&buffer_id) else {
            return false;
        };
        let overlay = Overlay::with_priority(
            &mut state.marker_list,
            range.clone(),
            OverlayFace::Style {
                style: Style::default().add_modifier(Modifier::UNDERLINED),
            },
            10_000,
        );
        let handle = state.overlays.add(overlay);
        self.mouse_state.clickable_path_hover = Some(super::types::ClickablePathHoverOverlay {
            buffer_id,
            range,
            handle,
        });
        true
    }

    fn clear_clickable_path_hover(&mut self) -> bool {
        let Some(hover) = self.mouse_state.clickable_path_hover.take() else {
            return false;
        };

        if let Some(state) = self.buffers.get_mut(&hover.buffer_id) {
            state
                .overlays
                .remove_by_handle(&hover.handle, &mut state.marker_list);
        }
        true
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum OpenTarget {
    Url(String),
    Path(PathBuf),
}

impl OpenTarget {
    fn display(&self) -> String {
        match self {
            Self::Url(url) => url.clone(),
            Self::Path(path) => path.to_string_lossy().into_owned(),
        }
    }

    fn open(&self) -> std::io::Result<()> {
        match self {
            Self::Url(url) => open::that(url),
            Self::Path(path) => open::that(path),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ClickableTarget {
    raw_target: String,
    highlight_range: Range<usize>,
}

#[cfg(test)]
fn clickable_target_at_byte(text: &str, byte_pos: usize) -> Option<String> {
    clickable_target_with_range_at_byte(text, byte_pos).map(|target| target.raw_target)
}

fn clickable_target_with_range_at_byte(text: &str, byte_pos: usize) -> Option<ClickableTarget> {
    let byte_pos = floor_char_boundary(text, byte_pos.min(text.len()));
    let (line_start, line_end) = line_bounds(text, byte_pos);
    let line = &text[line_start..line_end];
    let line_byte = byte_pos.saturating_sub(line_start);

    markdown_link_target_at_byte(line, line_byte, line_start)
        .or_else(|| angle_target_at_byte(line, line_byte, line_start))
        .or_else(|| raw_path_target_at_byte(line, line_byte, line_start))
}

fn resolve_open_target(target: &str) -> Option<OpenTarget> {
    let target = strip_markdown_target(target);
    if target.is_empty() {
        return None;
    }

    if target.starts_with("http://") || target.starts_with("https://") {
        return Some(OpenTarget::Url(target.to_string()));
    }

    if let Some(path) = file_url_to_path(target) {
        return Some(OpenTarget::Path(path));
    }

    if target == "~" || target.starts_with("~/") {
        return Some(OpenTarget::Path(
            crate::primitives::path_utils::expand_tilde(target),
        ));
    }

    if target.starts_with('/') {
        return Some(OpenTarget::Path(PathBuf::from(target)));
    }

    None
}

fn markdown_link_target_at_byte(
    line: &str,
    line_byte: usize,
    line_start: usize,
) -> Option<ClickableTarget> {
    let mut search_from = 0;
    while search_from < line.len() {
        let open_rel = line[search_from..].find('[')?;
        let open = search_from + open_rel;
        let after_open = open + 1;
        let close_text_rel = line[after_open..].find("](")?;
        let close_text = after_open + close_text_rel;
        let target_start = close_text + 2;

        let (target_content_start, target_end, link_end) = if line[target_start..].starts_with('<')
        {
            let inner_start = target_start + 1;
            let close_angle_rel = line[inner_start..].find('>')?;
            let close_angle = inner_start + close_angle_rel;
            let close_paren = close_angle + 1;
            if !line[close_paren..].starts_with(')') {
                search_from = target_start;
                continue;
            }
            (inner_start, close_angle, close_paren + 1)
        } else {
            let close_paren_rel = line[target_start..].find(')')?;
            let close_paren = target_start + close_paren_rel;
            (target_start, close_paren, close_paren + 1)
        };

        if line_byte >= open && line_byte <= link_end {
            let raw = &line[target_content_start..target_end];
            return plausible_target(raw).map(|target| ClickableTarget {
                raw_target: target.to_string(),
                highlight_range: (line_start + open)..(line_start + link_end),
            });
        }

        search_from = link_end;
    }

    None
}

fn angle_target_at_byte(
    line: &str,
    line_byte: usize,
    line_start: usize,
) -> Option<ClickableTarget> {
    let before = &line[..line_byte.min(line.len())];
    let open = before.rfind('<')?;
    let close = line[line_byte.min(line.len())..].find('>')? + line_byte.min(line.len());
    if close <= open {
        return None;
    }
    let raw = &line[(open + 1)..close];
    plausible_target(raw).map(|target| ClickableTarget {
        raw_target: target.to_string(),
        highlight_range: (line_start + open)..(line_start + close + 1),
    })
}

fn raw_path_target_at_byte(
    line: &str,
    line_byte: usize,
    line_start: usize,
) -> Option<ClickableTarget> {
    if line.is_empty() {
        return None;
    }

    let pos = line_byte.min(line.len().saturating_sub(1));
    let pos = floor_char_boundary(line, pos);
    let bytes = line.as_bytes();
    let mut start = pos;
    while start > 0 && !is_raw_boundary(bytes[start - 1]) {
        start -= 1;
    }
    let mut end = pos;
    while end < line.len() && !is_raw_boundary(bytes[end]) {
        end += 1;
    }

    let (trimmed_start, trimmed_end) = trimmed_raw_target_bounds(line, start, end);
    if trimmed_start >= trimmed_end {
        return None;
    }

    let raw = &line[trimmed_start..trimmed_end];
    plausible_target(raw).map(|target| ClickableTarget {
        raw_target: target.to_string(),
        highlight_range: (line_start + trimmed_start)..(line_start + trimmed_end),
    })
}

fn plausible_target(raw: &str) -> Option<&str> {
    let target = strip_markdown_target(raw);
    if target.starts_with("file://")
        || target.starts_with("http://")
        || target.starts_with("https://")
        || target.starts_with('/')
        || target == "~"
        || target.starts_with("~/")
    {
        Some(target)
    } else {
        None
    }
}

fn strip_markdown_target(raw: &str) -> &str {
    let trimmed = raw.trim();
    trimmed
        .strip_prefix('<')
        .and_then(|s| s.strip_suffix('>'))
        .unwrap_or(trimmed)
        .trim()
}

fn trimmed_raw_target_bounds(line: &str, start: usize, end: usize) -> (usize, usize) {
    let mut trimmed_start = start;
    let mut trimmed_end = end;
    let bytes = line.as_bytes();

    while trimmed_start < trimmed_end && is_trimmed_raw_target_byte(bytes[trimmed_start]) {
        trimmed_start += 1;
    }
    while trimmed_end > trimmed_start && is_trimmed_raw_target_byte(bytes[trimmed_end - 1]) {
        trimmed_end -= 1;
    }

    (trimmed_start, trimmed_end)
}

fn is_trimmed_raw_target_byte(byte: u8) -> bool {
    byte.is_ascii_whitespace()
        || matches!(byte, b'"' | b'\'' | b'`' | b',' | b';' | b')' | b']' | b'}')
}

fn is_raw_boundary(byte: u8) -> bool {
    byte.is_ascii_whitespace()
        || matches!(
            byte,
            b'<' | b'>' | b'"' | b'\'' | b'`' | b'[' | b']' | b'{' | b'}'
        )
}

fn rect_contains(rect: Rect, col: u16, row: u16) -> bool {
    col >= rect.x
        && col < rect.x.saturating_add(rect.width)
        && row >= rect.y
        && row < rect.y.saturating_add(rect.height)
}

fn file_url_to_path(url: &str) -> Option<PathBuf> {
    let rest = url.strip_prefix("file://")?;
    let rest = rest.strip_prefix("localhost").unwrap_or(rest);
    Some(PathBuf::from(percent_decode(rest)))
}

fn percent_decode(input: &str) -> String {
    let bytes = input.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let (Some(hi), Some(lo)) = (hex_value(bytes[i + 1]), hex_value(bytes[i + 2])) {
                out.push((hi << 4) | lo);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn hex_value(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

fn floor_char_boundary(text: &str, mut pos: usize) -> usize {
    pos = pos.min(text.len());
    while pos > 0 && !text.is_char_boundary(pos) {
        pos -= 1;
    }
    pos
}

fn line_bounds(text: &str, byte_pos: usize) -> (usize, usize) {
    let start = text[..byte_pos].rfind('\n').map(|idx| idx + 1).unwrap_or(0);
    let end = text[byte_pos..]
        .find('\n')
        .map(|idx| byte_pos + idx)
        .unwrap_or(text.len());
    (start, end)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_angle_markdown_path_when_clicking_label() {
        let text = "[Image #1](</Users/madda/My Images/image.png>)";
        let target = clickable_target_at_byte(text, 2);
        assert_eq!(target.as_deref(), Some("/Users/madda/My Images/image.png"));
    }

    #[test]
    fn finds_angle_markdown_path_when_clicking_path() {
        let text = "[Image #1](</Users/madda/My Images/image.png>)";
        let target = clickable_target_at_byte(text, text.find("My Images").unwrap());
        assert_eq!(target.as_deref(), Some("/Users/madda/My Images/image.png"));
    }

    #[test]
    fn finds_plain_markdown_path() {
        let text = "[Image #2](/Users/madda/.tmp/.gte/images/image.png)";
        let target = clickable_target_at_byte(text, text.find(".gte").unwrap());
        assert_eq!(
            target.as_deref(),
            Some("/Users/madda/.tmp/.gte/images/image.png")
        );
    }

    #[test]
    fn highlights_full_markdown_link_for_openable_path() {
        let text = "[Image #1](/Users/madda/.tmp/.zapet/images/image-2026-05-22-23-48-38.png)";
        let target = clickable_target_with_range_at_byte(text, text.find("Image").unwrap())
            .expect("markdown image path should be openable");

        assert_eq!(
            target.raw_target,
            "/Users/madda/.tmp/.zapet/images/image-2026-05-22-23-48-38.png"
        );
        assert_eq!(&text[target.highlight_range], text);
    }

    #[test]
    fn finds_raw_absolute_path() {
        let text = "open /Users/madda/.tmp/.gte/images/image.png please";
        let target = clickable_target_at_byte(text, text.find(".gte").unwrap());
        assert_eq!(
            target.as_deref(),
            Some("/Users/madda/.tmp/.gte/images/image.png")
        );
    }

    #[test]
    fn highlights_raw_path_without_trailing_punctuation() {
        let text = "open /Users/madda/.tmp/.gte/images/image.png) please";
        let target = clickable_target_with_range_at_byte(text, text.find(".gte").unwrap())
            .expect("raw path should be openable");

        assert_eq!(
            &text[target.highlight_range],
            "/Users/madda/.tmp/.gte/images/image.png"
        );
    }

    #[test]
    fn decodes_file_url_path() {
        let target = resolve_open_target("file:///Users/madda/My%20Images/image.png");
        assert_eq!(
            target,
            Some(OpenTarget::Path(PathBuf::from(
                "/Users/madda/My Images/image.png"
            )))
        );
    }

    #[test]
    fn open_path_modifier_accepts_control_or_command() {
        assert!(has_open_path_modifier(KeyModifiers::CONTROL));
        assert!(has_open_path_modifier(KeyModifiers::SUPER));
        assert!(!has_open_path_modifier(KeyModifiers::ALT));
    }
}
