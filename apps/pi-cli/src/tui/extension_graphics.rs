//! Terminal-only image and link painting from Rust-resolved component rectangles.
use base64::{Engine as _, engine::general_purpose::STANDARD};
use crossterm::{
    cursor::{MoveTo, RestorePosition, SavePosition},
    queue,
    style::Print,
};
use ratatui::{
    backend::Backend,
    buffer::{Buffer, Cell},
    layout::Rect,
};
use std::{
    collections::{BTreeMap, BTreeSet},
    hash::{Hash, Hasher},
    io::{self, Write},
};
use unicode_width::UnicodeWidthStr;

#[derive(Clone, PartialEq, Eq)]
struct Image {
    area: Rect,
    x: u16,
    y: u16,
    fingerprint: u64,
    data: String,
    kitty_id: Option<u32>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum ImageProtocol {
    Kitty,
    Iterm,
}

impl ImageProtocol {
    fn detect() -> Option<Self> {
        let requested = std::env::var("PI_IMAGE_PROTOCOL").unwrap_or_default();
        match requested.as_str() {
            "kitty" => return Some(Self::Kitty),
            "iterm2" | "iterm" => return Some(Self::Iterm),
            "none" => return None,
            _ => {}
        }
        let program = std::env::var("TERM_PROGRAM").unwrap_or_default();
        let term = std::env::var("TERM").unwrap_or_default();
        if term.contains("kitty") || matches!(program.as_str(), "kitty" | "ghostty") {
            Some(Self::Kitty)
        } else if matches!(program.as_str(), "iTerm.app" | "WezTerm") {
            Some(Self::Iterm)
        } else {
            None
        }
    }
}

#[derive(Default)]
pub(super) struct Graphics {
    disabled: bool,
    links: BTreeMap<(u16, u16), String>,
    images: Vec<Image>,
    previous_links: BTreeMap<(u16, u16), String>,
    previous_images: Vec<Image>,
}

pub(super) struct Paint {
    cells: Vec<(u16, u16, Cell, Option<String>)>,
    images: Vec<Image>,
    removed_image_ids: BTreeSet<u32>,
}

impl Graphics {
    pub(super) fn set_enabled(&mut self, enabled: bool) {
        self.disabled = !enabled;
    }
    pub(super) fn reset(&mut self) {
        self.previous_links.clear();
        self.previous_images.clear();
    }
    pub(super) fn begin(&mut self) {
        self.links.clear();
        self.images.clear();
    }
    pub(super) fn occlude(&mut self, area: Rect) {
        self.links
            .retain(|&(x, y), _| !area.contains((x, y).into()));
        self.images
            .retain(|image| image.area.intersection(area).is_empty());
    }
    pub(super) fn record_link(&mut self, url: &str, area: Rect) {
        if url.chars().any(char::is_control) {
            return;
        }
        for y in area.y..area.bottom() {
            for x in area.x..area.right() {
                self.links.insert((x, y), url.to_owned());
            }
        }
    }
    /// The caller has already clipped layout; unsupported terminals keep the node's alt text.
    pub(super) fn record_image(
        &mut self,
        data: &str,
        mime_type: &str,
        _alt: &str,
        area: Rect,
    ) -> bool {
        if self.disabled {
            return false;
        }
        let Some(protocol) = ImageProtocol::detect() else {
            return false;
        };
        self.image(data, mime_type, area, protocol)
    }
    fn image(&mut self, data: &str, mime_type: &str, area: Rect, protocol: ImageProtocol) -> bool {
        if area.is_empty()
            || !matches!(
                mime_type,
                "image/png" | "image/jpeg" | "image/gif" | "image/webp" | "image/bmp"
            )
        {
            return false;
        }
        let mut hash = std::hash::DefaultHasher::new();
        (
            data,
            mime_type,
            area.x,
            area.y,
            area.width,
            area.height,
            protocol == ImageProtocol::Kitty,
        )
            .hash(&mut hash);
        let fingerprint = hash.finish();
        if let Some(image) = self
            .previous_images
            .iter()
            .find(|image| image.fingerprint == fingerprint)
        {
            self.images.push(image.clone());
            return true;
        }
        let Ok(bytes) = STANDARD.decode(data) else {
            return false;
        };
        let Ok(decoded) = image::load_from_memory(&bytes) else {
            return false;
        };
        let mut png = io::Cursor::new(Vec::new());
        if decoded.write_to(&mut png, image::ImageFormat::Png).is_err() {
            return false;
        }
        let encoded = STANDARD.encode(png.get_ref());
        let id = (fingerprint as u32).max(1);
        let (data, kitty_id) = match protocol {
            ImageProtocol::Kitty => {
                let mut output = String::new();
                let chunks: Vec<_> = encoded.as_bytes().chunks(4096).collect();
                for (index, chunk) in chunks.iter().enumerate() {
                    let more = u8::from(index + 1 != chunks.len());
                    let header = if index == 0 {
                        format!(
                            "a=T,f=100,q=2,C=1,c={},r={},i={id},m={more}",
                            area.width, area.height
                        )
                    } else {
                        format!("m={more}")
                    };
                    output.push_str(&format!(
                        "\x1b_G{header};{}\x1b\\",
                        std::str::from_utf8(chunk).expect("base64 is ASCII")
                    ));
                }
                (output, Some(id))
            }
            ImageProtocol::Iterm => (
                format!(
                    "\x1b]1337;File=inline=1;size={};width={};height={};preserveAspectRatio=1:{encoded}\x07",
                    png.get_ref().len(),
                    area.width,
                    area.height
                ),
                None,
            ),
        };
        self.images.push(Image {
            area,
            x: area.x,
            y: area.y,
            fingerprint,
            data,
            kitty_id,
        });
        true
    }
    pub(super) fn prepare(&mut self, buffer: &Buffer) -> Paint {
        let images_changed = self.images != self.previous_images;
        let mut positions: BTreeSet<_> = self
            .links
            .keys()
            .chain(self.previous_links.keys())
            .copied()
            .collect();
        let mut removed_image_ids = BTreeSet::new();
        if images_changed {
            for image in &self.previous_images {
                if let Some(id) = image.kitty_id {
                    removed_image_ids.insert(id);
                }
                let area = image.area.intersection(buffer.area);
                for y in area.y..area.bottom() {
                    for x in area.x..area.right() {
                        positions.insert((x, y));
                    }
                }
            }
        }
        let mut cells = Vec::new();
        let mut covered = None;
        // Row-major order is needed to skip the continuation cells of wide graphemes.
        let positions: BTreeSet<_> = positions.into_iter().map(|(x, y)| (y, x)).collect();
        for (y, x) in positions {
            if !buffer.area.contains((x, y).into())
                || covered.is_some_and(|(row, end)| row == y && x < end)
            {
                continue;
            }
            let cell = buffer[(x, y)].clone();
            covered = Some((y, x.saturating_add(cell.symbol().width().max(1) as u16)));
            cells.push((x, y, cell, self.links.get(&(x, y)).cloned()));
        }
        self.previous_links.clone_from(&self.links);
        self.previous_images.clone_from(&self.images);
        Paint {
            cells,
            images: if images_changed {
                self.images.clone()
            } else {
                Vec::new()
            },
            removed_image_ids,
        }
    }
}

impl Paint {
    pub(super) fn emit(self, backend: &mut super::TuiTerminal) -> io::Result<()> {
        if self.cells.is_empty() && self.images.is_empty() && self.removed_image_ids.is_empty() {
            return Ok(());
        }
        let mut output = io::stdout();
        queue!(output, SavePosition)?;
        for id in self.removed_image_ids {
            queue!(output, Print(format!("\x1b_Ga=d,d=I,i={id},q=2\x1b\\")))?;
        }
        for (x, y, cell, link) in self.cells {
            if let Some(link) = &link {
                queue!(output, Print(format!("\x1b]8;;{link}\x1b\\")))?;
            }
            backend.backend_mut().draw(std::iter::once((x, y, &cell)))?;
            if link.is_some() {
                queue!(output, Print("\x1b]8;;\x1b\\"))?;
            }
        }
        for image in self.images {
            queue!(output, MoveTo(image.x, image.y), Print(image.data))?;
        }
        queue!(output, RestorePosition)?;
        output.flush()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn png() -> String {
        STANDARD.encode(pi_media::image::encode_rgba_png(1, 1, vec![255, 0, 0, 255]).unwrap())
    }
    #[test]
    fn images_are_encoded_by_rust_and_removed_when_an_overlay_occludes_them() {
        let mut graphics = Graphics::default();
        let area = Rect::new(2, 1, 3, 2);
        assert!(graphics.image(&png(), "image/png", area, ImageProtocol::Kitty));
        assert!(
            graphics.images[0]
                .data
                .starts_with("\x1b_Ga=T,f=100,q=2,C=1,c=3,r=2,i=")
        );
        let buffer = Buffer::empty(Rect::new(0, 0, 8, 4));
        assert_eq!(graphics.prepare(&buffer).images.len(), 1);
        graphics.occlude(Rect::new(0, 0, 2, 1));
        assert_eq!(graphics.images.len(), 1);
        graphics.occlude(Rect::new(3, 2, 1, 1));
        let paint = graphics.prepare(&buffer);
        assert_eq!(paint.removed_image_ids.len(), 1);
        assert_eq!(paint.cells.len(), 6);
        assert!(!graphics.image("\x1b[2J", "image/png", area, ImageProtocol::Kitty));
        assert!(!graphics.image(
            &STANDARD.encode(b"not an image"),
            "image/png",
            area,
            ImageProtocol::Kitty
        ));
        assert!(graphics.image(&png(), "image/png", area, ImageProtocol::Iterm));
        assert!(
            graphics.images[0]
                .data
                .starts_with("\x1b]1337;File=inline=1;")
        );
    }
    #[test]
    fn links_follow_native_rectangles_and_are_cleared_on_unmount() {
        let mut graphics = Graphics::default();
        graphics.record_link("https://example.com", Rect::new(0, 0, 4, 1));
        graphics.record_link("https://example.com\x1b[2J", Rect::new(4, 0, 4, 1));
        assert_eq!(graphics.links.len(), 4);
        graphics.occlude(Rect::new(2, 0, 4, 1));
        let buffer = Buffer::empty(Rect::new(0, 0, 8, 1));
        assert_eq!(graphics.prepare(&buffer).cells.len(), 2);
        graphics.begin();
        let cleared = graphics.prepare(&buffer);
        assert_eq!(cleared.cells.len(), 2);
        assert!(cleared.cells.iter().all(|(_, _, _, link)| link.is_none()));
    }
}
