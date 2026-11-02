//! Compositor-owned accessory, manual menus and fully-painted native choice rows.
use crate::core::Snapshot;
use anyhow::Result;
use cairo::{Context, Format, ImageSurface};
use pango::prelude::*;
use zeroize::Zeroize;
#[derive(Clone, Debug)]
pub(super) struct Region {
    pub rect: [i32; 4],
    pub action: String,
    pub enabled: bool,
}
#[derive(Clone, Debug)]
pub(super) struct MenuItem {
    pub action: String,
    pub label: String,
    pub enabled: bool,
}
pub(super) struct Frame {
    pub image: ImageSurface,
    pub regions: Vec<Region>,
    pub scale: i32,
    pub choice_count: usize,
}
impl Drop for Frame {
    fn drop(&mut self) {
        if let Ok(mut pixels) = self.image.data() {
            pixels.zeroize();
        }
    }
}
pub(super) struct Panel {
    pub snapshot: Option<Snapshot>,
    pub paused: bool,
    pub menu: Option<Vec<MenuItem>>,
    pub page: usize,
    pub page_size: usize,
    pub selected: Option<usize>,
    pub pending: bool,
    pub regions: Vec<Region>,
    pub pressed: Option<(String, String)>,
    pub bounds: [i32; 2],
    pub scale: i32,
    pub dark: bool,
    pub choice_mode: bool,
    pub visible_choices: usize,
    pub bounds_known: bool,
}
impl Default for Panel {
    fn default() -> Self {
        Self {
            snapshot: None,
            paused: false,
            menu: None,
            page: 0,
            page_size: 1,
            selected: None,
            pending: false,
            regions: Vec::new(),
            pressed: None,
            bounds: [1024, 768],
            scale: 1,
            dark: false,
            choice_mode: false,
            visible_choices: 0,
            bounds_known: false,
        }
    }
}
impl Panel {
    pub fn clear(&mut self) -> bool {
        let menu = self.menu.is_some();
        if self.choice_mode {
            if let Some(rows) = &mut self.menu {
                for row in rows {
                    row.label.zeroize();
                }
            }
        }
        self.snapshot = None;
        self.menu = None;
        self.page = 0;
        self.selected = None;
        self.pending = false;
        self.regions.clear();
        self.pressed = None;
        self.choice_mode = false;
        self.visible_choices = 0;
        menu
    }
    pub fn hit(&self, x: i32, y: i32) -> Option<&str> {
        self.regions
            .iter()
            .find(|r| {
                let [l, t, w, h] = r.rect;
                r.enabled && x >= l && y >= t && x < l + w && y < t + h
            })
            .map(|r| r.action.as_str())
    }
    pub fn pointer_action(&mut self, anchor: &str, x: i32, y: i32, state: u32) -> Option<String> {
        let action = self.hit(x, y).map(str::to_owned);
        if state == 1 {
            self.pressed = action.map(|a| (a, anchor.into()));
            return None;
        }
        if state != 0 {
            return None;
        }
        let pressed = self.pressed.take();
        let action = action?;
        (pressed == Some((action.clone(), anchor.into()))).then_some(action)
    }
    pub fn navigate(&mut self, key: &str) -> bool {
        let Some(menu) = &self.menu else { return false };
        let menu = if self.choice_mode {
            &menu[..self.visible_choices.min(menu.len())]
        } else {
            menu
        };
        let mut enabled = menu
            .iter()
            .enumerate()
            .filter(|(_, v)| v.enabled)
            .map(|(i, _)| i);
        let first = enabled.next();
        let last = enabled.last().or(first);
        let next = match key {
            "Home" => first,
            "End" => last,
            "Down" => self
                .selected
                .and_then(|s| {
                    menu.iter()
                        .enumerate()
                        .skip(s + 1)
                        .find(|(_, v)| v.enabled)
                        .map(|(i, _)| i)
                })
                .or(first),
            "Up" => self
                .selected
                .and_then(|s| menu[..s].iter().rposition(|v| v.enabled))
                .or(last),
            _ => return false,
        };
        self.selected = next;
        if let Some(index) = next {
            self.page = index / self.page_size;
        }
        true
    }
    pub fn frame(&mut self) -> Result<Frame> {
        if self.choice_mode {
            return self.choices_frame();
        }
        let scale = self.scale.clamp(1, 4);
        let font = pango::FontDescription::from_string("Sans 13");
        let pango_context = pangocairo::FontMap::default().create_context();
        let metrics = pango_context.metrics(Some(&font), None);
        let row = 36.max(((metrics.ascent() + metrics.descent()) / pango::SCALE) + 14);
        let layout = pango::Layout::new(&pango_context);
        layout.set_font_description(Some(&font));
        layout.set_single_paragraph_mode(true);
        let mut width = row + 8;
        let mut height = row + 8;
        let mut offset = 0;
        let mut count = 0;
        let mut pages = 1;
        if let Some(menu) = &self.menu {
            self.page_size = 1.max(((self.bounds[1] - 24) / row - 2).max(1) as usize);
            pages = menu.len().div_ceil(self.page_size).max(1);
            self.page = self.page.min(pages - 1);
            offset = self.page * self.page_size;
            count = menu.len().saturating_sub(offset).min(self.page_size);
            let label_width = menu
                .iter()
                .map(|item| {
                    layout.set_text(&item.label);
                    layout.pixel_size().0
                })
                .max()
                .unwrap_or(140);
            width = (label_width + 32).clamp(210, 480);
            height = row * (count as i32 + 1 + i32::from(pages > 1)) + 8;
        }
        width = width.min((self.bounds[0] - 16).max(44));
        let image = ImageSurface::create(Format::ARgb32, width * scale, height * scale)?;
        let c = Context::new(&image)?;
        c.scale(scale as f64, scale as f64);
        let text = if self.dark {
            [0.95, 0.95, 0.96]
        } else {
            [0.12, 0.12, 0.13]
        };
        let bg = if self.dark {
            [0.13, 0.13, 0.14]
        } else {
            [0.98, 0.98, 0.99]
        };
        if self.menu.is_some() {
            c.set_source_rgb(bg[0], bg[1], bg[2]);
            rounded(&c, 0.5, 0.5, width as f64 - 1.0, height as f64 - 1.0, 9.0);
            c.fill_preserve()?;
            c.set_source_rgb(0.45, 0.45, 0.47);
            c.set_line_width(1.0);
            c.stroke()?;
        }
        let icon_bytes: &[u8] = if scale > 1 {
            include_bytes!("../../assets/app-24@2x.png")
        } else {
            include_bytes!("../../assets/app-24.png")
        };
        let icon = ImageSurface::create_from_png(&mut std::io::Cursor::new(icon_bytes))?;
        c.save()?;
        c.translate(10.0, ((row + 8 - 24) / 2) as f64);
        c.scale(24.0 / icon.width() as f64, 24.0 / icon.height() as f64);
        c.set_source_surface(&icon, 0.0, 0.0)?;
        c.paint_with_alpha(if self.paused { 0.45 } else { 1.0 })?;
        c.restore()?;
        let mut regions = vec![Region {
            rect: [4, 4, 38, row],
            action: "menu".into(),
            enabled: true,
        }];
        let draw = |label: &str, x: i32, y: i32, w: i32, color: [f64; 3]| -> Result<()> {
            layout.set_width(w.max(0) * pango::SCALE);
            layout.set_ellipsize(pango::EllipsizeMode::End);
            layout.set_text(label);
            c.set_source_rgb(color[0], color[1], color[2]);
            c.move_to(x as f64, (y + (row - layout.pixel_size().1) / 2) as f64);
            pangocairo::functions::show_layout(&c, &layout);
            Ok(())
        };
        if let Some(menu) = &self.menu {
            draw("BroTypist", 44, 4, width - 52, text)?;
            for (local, item) in menu.iter().skip(offset).take(count).enumerate() {
                let y = (local as i32 + 1) * row + 4;
                let selected = self.selected == Some(offset + local);
                if selected {
                    c.set_source_rgb(0.20, 0.40, 0.74);
                    c.rectangle(4.0, y as f64, (width - 8) as f64, row as f64);
                    c.fill()?;
                }
                draw(
                    &item.label,
                    14,
                    y,
                    width - 28,
                    if selected {
                        [1.0; 3]
                    } else if item.enabled {
                        text
                    } else {
                        [0.52; 3]
                    },
                )?;
                regions.push(Region {
                    rect: [4, y, width - 8, row],
                    action: item.action.clone(),
                    enabled: item.enabled,
                });
            }
            if pages > 1 {
                let y = (count as i32 + 1) * row + 4;
                for (left, action, title, enabled) in [
                    (4, "page:previous", "Previous", self.page > 0),
                    (width / 2, "page:next", "Next", self.page + 1 < pages),
                ] {
                    draw(
                        title,
                        left + 10,
                        y,
                        width / 2 - 18,
                        if enabled { text } else { [0.52; 3] },
                    )?;
                    regions.push(Region {
                        rect: [left, y, width / 2 - 4, row],
                        action: action.into(),
                        enabled,
                    });
                }
            }
        }
        drop(c);
        image.flush();
        Ok(Frame {
            image,
            regions,
            scale,
            choice_count: 0,
        })
    }
    fn choices_frame(&self) -> Result<Frame> {
        let scale = self.scale.clamp(1, 4);
        let width = self.bounds[0].saturating_sub(24).clamp(44, 720);
        let max_height = self.bounds[1].saturating_sub(24).max(0);
        let font = pango::FontDescription::from_string("Sans 13");
        let layout = pango::Layout::new(&pangocairo::FontMap::default().create_context());
        layout.set_font_description(Some(&font));
        layout.set_width((width - 48).max(0) * pango::SCALE);
        layout.set_wrap(pango::WrapMode::WordChar);
        layout.set_ellipsize(pango::EllipsizeMode::None);
        layout.set_single_paragraph_mode(false);
        let metrics = layout.context().metrics(Some(&font), None);
        let header = 36.max((metrics.ascent() + metrics.descent()) / pango::SCALE + 14);
        let mut heights = [0; 32];
        let mut count = 0;
        let mut height = header + 8;
        if self.bounds_known && width >= 96 && max_height >= height {
            for item in self.menu.as_deref().unwrap_or_default().iter().take(32) {
                layout.set_text(&item.label);
                let (text_width, text_height) = layout.pixel_size();
                let row = text_height.max(header - 14) + 14;
                if text_width > width - 48 || height + row > max_height {
                    break;
                }
                heights[count] = row;
                count += 1;
                height += row;
            }
        }
        let image = ImageSurface::create(Format::ARgb32, width * scale, height.max(1) * scale)?;
        let c = Context::new(&image)?;
        c.scale(scale as f64, scale as f64);
        let (fg, bg) = if self.dark {
            ([0.95, 0.95, 0.96], [0.13, 0.13, 0.14])
        } else {
            ([0.12, 0.12, 0.13], [0.98, 0.98, 0.99])
        };
        c.set_source_rgb(bg[0], bg[1], bg[2]);
        c.paint()?;
        layout.set_text("BroTypist");
        c.set_source_rgb(fg[0], fg[1], fg[2]);
        c.move_to(14.0, 11.0);
        pangocairo::functions::show_layout(&c, &layout);
        let mut regions = Vec::with_capacity(count);
        let mut y = header + 4;
        for (index, (item, row)) in self
            .menu
            .as_deref()
            .unwrap_or_default()
            .iter()
            .zip(&heights[..count])
            .enumerate()
        {
            let selected = self.selected == Some(index);
            if selected {
                c.set_source_rgb(0.20, 0.40, 0.74);
                c.rectangle(4.0, y as f64, (width - 8) as f64, *row as f64);
                c.fill()?;
            }
            let color = if selected { [1.0; 3] } else { fg };
            c.set_source_rgb(color[0], color[1], color[2]);
            layout.set_text(&item.label);
            c.move_to(14.0, (y + 7) as f64);
            pangocairo::functions::show_layout(&c, &layout);
            regions.push(Region {
                rect: [4, y, width - 8, *row],
                action: item.action.clone(),
                enabled: item.enabled,
            });
            y += row;
        }
        drop(c);
        image.flush();
        Ok(Frame {
            image,
            regions,
            scale,
            choice_count: count,
        })
    }
}
fn rounded(c: &Context, x: f64, y: f64, w: f64, h: f64, r: f64) {
    use std::f64::consts::{FRAC_PI_2, PI};
    c.new_sub_path();
    c.arc(x + w - r, y + r, r, -FRAC_PI_2, 0.0);
    c.arc(x + w - r, y + h - r, r, 0.0, FRAC_PI_2);
    c.arc(x + r, y + h - r, r, FRAC_PI_2, PI);
    c.arc(x + r, y + r, r, PI, PI + FRAC_PI_2);
    c.close_path();
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn native_choice_navigation_never_selects_hidden_rows() {
        let mut panel = Panel::default();
        panel.choice_mode = true;
        panel.visible_choices = 2;
        panel.menu = Some(
            (0..5)
                .map(|index| MenuItem {
                    action: format!("native-choice:7:{index}"),
                    label: format!("candidate {index}"),
                    enabled: true,
                })
                .collect(),
        );
        panel.selected = Some(0);
        assert!(panel.navigate("End"));
        assert_eq!(panel.selected, Some(1));
        assert!(panel.navigate("Down"));
        assert_eq!(panel.selected, Some(0));
        panel.clear();
        assert!(!panel.choice_mode);
        assert_eq!(panel.visible_choices, 0);
        assert!(panel.menu.is_none());
    }
    #[test]
    fn native_choice_frames_require_known_bounds_and_paint_only_complete_prefix_rows() {
        let mut panel = Panel::default();
        panel.choice_mode = true;
        panel.menu = Some(
            (0..4)
                .map(|index| MenuItem {
                    action: format!("native-choice:7:{index}"),
                    label:
                        "A wrapped candidate é😀 with a second line\nand an explicit third line."
                            .into(),
                    enabled: true,
                })
                .collect(),
        );
        assert_eq!(panel.frame().unwrap().choice_count, 0);
        panel.bounds_known = true;
        panel.bounds = [400, 4096];
        let full = panel.frame().unwrap();
        assert_eq!(full.choice_count, 4);
        let first = full.regions[0].rect;
        panel.bounds[1] = first[1] + first[3] + 28;
        let frame = panel.frame().unwrap();
        assert_eq!(frame.choice_count, 1);
        assert_eq!(frame.regions.len(), frame.choice_count);
        assert!(frame.image.height() <= panel.bounds[1] - 24);
        for (index, region) in frame.regions.iter().enumerate() {
            assert_eq!(region.action, format!("native-choice:7:{index}"));
            assert!(region.rect[1] + region.rect[3] <= frame.image.height());
        }
        panel.bounds = [80, 60];
        assert_eq!(panel.frame().unwrap().choice_count, 0);
    }
    #[test]
    fn menu_navigation_skips_disabled_and_changes_pages() {
        let mut p = Panel::default();
        p.page_size = 2;
        p.menu = Some(
            (0..7)
                .map(|i| MenuItem {
                    action: i.to_string(),
                    label: String::new(),
                    enabled: i == 1 || i == 6,
                })
                .collect(),
        );
        p.selected = Some(1);
        assert!(p.navigate("Down"));
        assert_eq!((p.selected, p.page), (Some(6), 3));
        p.navigate("Down");
        assert_eq!(p.selected, Some(1));
        p.navigate("End");
        assert_eq!(p.selected, Some(6));
        p.navigate("Up");
        assert_eq!(p.selected, Some(1));
    }
    #[test]
    fn busy_or_hidden_frames_have_no_actionable_hit_regions() {
        let mut p = Panel::default();
        p.regions.push(Region {
            rect: [0, 0, 20, 20],
            action: "manual".into(),
            enabled: true,
        });
        assert_eq!(p.hit(1, 1), Some("manual"));
        p.pending = true;
        p.regions.clear();
        assert_eq!(p.hit(1, 1), None);
        p.clear();
        assert!(p.snapshot.is_none() && !p.pending && p.pressed.is_none());
    }
    #[test]
    fn clicks_require_matching_press_release_action_and_anchor() {
        let mut p = Panel::default();
        p.regions = vec![
            Region {
                rect: [0, 0, 20, 20],
                action: "menu".into(),
                enabled: true,
            },
            Region {
                rect: [20, 0, 20, 20],
                action: "disabled".into(),
                enabled: false,
            },
        ];
        assert!(p.pointer_action("1:7", 1, 1, 1).is_none());
        assert_eq!(p.pointer_action("1:7", 1, 1, 0).as_deref(), Some("menu"));
        p.pointer_action("1:7", 1, 1, 1);
        assert!(p.pointer_action("2:7", 1, 1, 0).is_none());
        p.pointer_action("1:7", 1, 1, 1);
        assert!(p.pointer_action("1:7", 21, 1, 0).is_none());
        assert!(p.pointer_action("1:7", 1, 1, 0).is_none());
        p.pointer_action("1:7", 1, 1, 1);
        p.clear();
        assert!(p.pointer_action("1:7", 1, 1, 0).is_none());
    }
}
