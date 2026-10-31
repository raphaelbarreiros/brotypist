//! Complete retained AppKit settings. All service work is dispatched by the shell.
use super::macos_settings_contract::{ReadValue, UiAction};
use super::{Kit, Obj, add, frame, identify, obj, rect, set_string, string};
use crate::config::Config;
use anyhow::Result;
use objc2::rc::Retained;
use objc2::runtime::{AnyClass, AnyObject};
use objc2::{msg_send, sel};
use objc2_foundation::{NSPoint, NSRect, NSSize, NSString};
use serde_json::{Value, json};
use std::collections::BTreeMap;
#[path = "macos_settings_panes.rs"]
mod panes;
use panes::{Chart, Form, Table};
const REPOSITORY: &str = "https://github.com/renan-brasilio/brotypist";
const PANES: &[(&str, &str)] = &[
    ("setup", "Setup"),
    ("general", "General"),
    ("models", "AI Models"),
    ("context", "Context"),
    ("personalization", "Personalization"),
    ("emoji", "Emoji"),
    ("shortcuts", "Shortcuts"),
    ("battery", "Battery"),
    ("appSettings", "App Settings"),
    ("labs", "Labs"),
    ("statistics", "Statistics"),
    ("about", "About"),
    ("support", "Support"),
];
fn layout_status_action(container: &Obj, status: &Obj, button: &Obj) {
    let bounds: NSRect = unsafe { msg_send![&**container, bounds] };
    let button_width = 172.;
    let gap = 8.;
    frame(
        status,
        &rect(
            0.,
            (bounds.size.height - 20.) / 2.,
            bounds.size.width - button_width - gap,
            20.,
        ),
    );
    frame(
        button,
        &rect(
            bounds.size.width - button_width,
            0.,
            button_width,
            bounds.size.height,
        ),
    );
    unsafe {
        // Flexible status width and leading button margin keep the action trailing.
        // Flexible vertical margins keep both children centered in the row header.
        let _: () = msg_send![&**status,setAutoresizingMask:42usize];
        let _: () = msg_send![&**status,setMaximumNumberOfLines:1isize];
        let _: () = msg_send![&**status,setLineBreakMode:4usize];
        let _: () = msg_send![&**button,setAutoresizingMask:41usize];
    }
    add(container, status);
    add(container, button);
}
struct Control {
    view: Obj,
    field: String,
    options: Vec<Value>,
    kind: u8,
}
struct Instructions {
    editor: Obj,
    save: Obj,
    revert: Obj,
}
pub(super) struct Settings {
    kit: Kit,
    window: Option<Obj>,
    root: Option<Obj>,
    sidebar: Option<Obj>,
    sidebar_scroll: Option<Obj>,
    title: Option<Obj>,
    scroll: Option<Obj>,
    form: Option<Form>,
    current_pane: String,
    controls: Vec<Control>,
    live: BTreeMap<String, Obj>,
    editors: BTreeMap<String, Instructions>,
    draft_text: BTreeMap<String, String>,
    rule_table: Option<Table>,
    stats_table: Option<Table>,
    chart: Option<Chart>,
    rule_query: String,
    power_card: Option<Obj>,
    sidebar_material: Option<Obj>,
    sidebar_status: Option<Obj>,
    sidebar_badge: Option<Obj>,
    sidebar_selection: Option<Obj>,
    sidebar_buttons: BTreeMap<String, Obj>,
    config_values: Value,
    statistics_snapshot: Value,
    statistics_metric: String,
    layout_dirty: std::cell::Cell<bool>,
    pub(super) model_selection: Option<String>,
    pub(super) selected_rule: String,
    pub(super) advanced: bool,
    pub(super) stat_period: String,
    pub(super) stat_group: String,
    pub(super) stat_metric: String,
}
impl Settings {
    pub(super) fn new(kit: Kit) -> Self {
        Self {
            kit,
            window: None,
            root: None,
            sidebar: None,
            sidebar_scroll: None,
            title: None,
            scroll: None,
            form: None,
            current_pane: String::new(),
            controls: Vec::new(),
            live: BTreeMap::new(),
            editors: BTreeMap::new(),
            draft_text: BTreeMap::new(),
            rule_table: None,
            stats_table: None,
            chart: None,
            rule_query: String::new(),
            power_card: None,
            sidebar_material: None,
            sidebar_status: None,
            sidebar_badge: None,
            sidebar_selection: None,
            sidebar_buttons: BTreeMap::new(),
            config_values: Value::Null,
            statistics_snapshot: Value::Null,
            statistics_metric: String::new(),
            layout_dirty: std::cell::Cell::new(true),
            model_selection: None,
            selected_rule: String::new(),
            advanced: false,
            stat_period: "week".into(),
            stat_group: "day".into(),
            stat_metric: "completions".into(),
        }
    }
    fn build_window(&mut self) {
        let window = self
            .kit
            .window("BroTypist Settings", "inklow.settings", 900., 640., true);
        let root = self.kit.view(rect(0., 0., 900., 640.));
        let sidebar = self.kit.view(rect(0., 0., 210., 640.));
        unsafe {
            let _: () = msg_send![&*root,setAutoresizingMask:18usize];
        }
        let material = obj("NSVisualEffectView");
        frame(&material, &rect(0., 0., 210., 640.));
        unsafe {
            let _: () = msg_send![&*material,setMaterial:7isize];
            let _: () = msg_send![&*material,setBlendingMode:0isize];
            let _: () = msg_send![&*material,setState:1isize];
            let _: () = msg_send![&*material,setAutoresizingMask:18usize];
        }
        add(&sidebar, &material);
        self.sidebar_material = Some(material);
        // Match the source-list baseline: navigation starts below the traffic lights.
        let navigation = self.kit.view(rect(0., 0., 194., 0.));
        let sidebar_scroll = obj("NSScrollView");
        frame(&sidebar_scroll, &rect(8., 50., 194., 470.));
        unsafe {
            let _: () = msg_send![&*sidebar_scroll,setDrawsBackground:false];
            let _: () = msg_send![&*sidebar_scroll,setHasVerticalScroller:true];
            let _: () = msg_send![&*sidebar_scroll,setAutohidesScrollers:true];
            let _: () = msg_send![&*sidebar_scroll,setScrollerStyle:1isize];
            let _: () = msg_send![&*sidebar_scroll,setDocumentView:&*navigation];
        }
        add(&sidebar, &sidebar_scroll);
        self.sidebar_scroll = Some(sidebar_scroll);
        let mut y = 0.;
        let selection = obj("NSBox");
        unsafe {
            let _: () = msg_send![&*selection,setBoxType:4usize];
            let _: () = msg_send![&*selection,setBorderType:0usize];
            let _: () = msg_send![&*selection,setTitlePosition:0usize];
            let _: () = msg_send![&*selection,setCornerRadius:8.0f64];
            let color: *mut AnyObject = msg_send![
                AnyClass::get(c"NSColor").unwrap(),
                unemphasizedSelectedContentBackgroundColor
            ];
            let _: () = msg_send![&*selection,setFillColor:color];
        }
        identify(
            &selection,
            "inklow.sidebar.selection",
            "Selected settings category",
        );
        add(&navigation, &selection);
        self.sidebar_selection = Some(selection);
        for (key, title) in PANES {
            if ["general", "labs", "about"].contains(key) {
                y += 12.;
            }
            let action = if *key == "support" {
                UiAction::Url(format!("{REPOSITORY}/issues"))
            } else {
                UiAction::Navigate((*key).into())
            };
            let button = self.kit.button(
                title,
                &format!("inklow.sidebar.{key}"),
                rect(0., y, 194., 36.),
                action,
            );
            // Keep one native full-row button; the padded image supplies its content inset.
            sidebar_symbol(&button, key);
            unsafe {
                let _: () = msg_send![&*button,setBordered:false];
                let _: () = msg_send![&*button,setAlignment:0isize];
                let _: () = msg_send![&*button,setButtonType:1usize];
                let cell: *mut AnyObject = msg_send![&*button, cell];
                // State remains available to AX and actions, but only our rounded box draws it.
                let _: () = msg_send![cell,setHighlightsBy:0usize];
                let _: () = msg_send![cell,setShowsStateBy:0usize];
            }
            add(&navigation, &button);
            self.sidebar_buttons.insert((*key).into(), button);
            y += 36.;
        }
        frame(&navigation, &rect(0., 0., 194., y));
        let badge = self.kit.label("●", rect(171., 9., 12., 18.), 10., false);
        identify(
            &badge,
            "inklow.sidebar.setup.pending",
            "Setup has remaining steps",
        );
        unsafe {
            let color: *mut AnyObject =
                msg_send![AnyClass::get(c"NSColor").unwrap(), systemOrangeColor];
            let _: () = msg_send![&*badge,setTextColor:color];
        }
        add(&navigation, &badge);
        self.sidebar_badge = Some(badge);
        identify(&sidebar, "inklow.sidebar", "Settings categories");
        let status = self.kit.label("", rect(20., 574., 174., 48.), 11., false);
        identify(&status, "inklow.sidebar.status", "BroTypist status");
        unsafe {
            let _: () = msg_send![&*status,setAutoresizingMask:8usize];
            let _: () = msg_send![&*status,setMaximumNumberOfLines:1isize];
            let _: () = msg_send![&*status,setLineBreakMode:4usize];
        }
        add(&sidebar, &status);
        self.sidebar_status = Some(status);
        let title = self.kit.label(
            "",
            rect(
                210. + panes::CONTENT_INSET,
                26.,
                690. - 2. * panes::CONTENT_INSET,
                24.,
            ),
            15.,
            true,
        );
        identify(&title, "inklow.settings.paneTitle", "Settings pane");
        let scroll = obj("NSScrollView");
        frame(&scroll, &rect(210., 52., 690., 588.));
        unsafe {
            let _: () = msg_send![&*scroll,setHasVerticalScroller:true];
            let _: () = msg_send![&*scroll,setAutohidesScrollers:true];
            let _: () = msg_send![&*scroll,setDrawsBackground:false];
            let _: () = msg_send![&*scroll,setHasHorizontalScroller:false];
            let _: () = msg_send![&*scroll,setAutomaticallyAdjustsContentInsets:false];
            let _: () = msg_send![&*window,setContentView:&*root];
            let _: () = msg_send![&*window,setMinSize:NSSize::new(900.,640.)];
            let _: () = msg_send![&*window,setTitleVisibility:1isize];
            let _: () = msg_send![&*window,setAutorecalculatesKeyViewLoop:true];
        }
        add(&root, &sidebar);
        add(&root, &title);
        add(&root, &scroll);
        self.window = Some(window);
        self.root = Some(root);
        self.sidebar = Some(sidebar);
        self.title = Some(title);
        self.scroll = Some(scroll);
    }
    pub(super) fn show(
        &mut self,
        pane: &str,
        config: &Config,
        status: &Value,
        inventory: &Value,
        update: &Value,
    ) -> Result<()> {
        self.prepare(pane, config, status, inventory, update)?;
        if !pane.eq_ignore_ascii_case("support") {
            super::present_window(self.window.as_ref().unwrap(), false);
            self.reflow();
        }
        Ok(())
    }
    /// Build and refresh the canonical retained pane without ordering or activating its window.
    pub(super) fn prepare(
        &mut self,
        pane: &str,
        config: &Config,
        status: &Value,
        inventory: &Value,
        update: &Value,
    ) -> Result<()> {
        if pane.eq_ignore_ascii_case("support") {
            self.kit
                .queue
                .borrow_mut()
                .pending
                .push_back(UiAction::Url(format!("{REPOSITORY}/issues")));
            return Ok(());
        }
        if self.window.is_none() {
            self.build_window();
        }
        self.remember_drafts();
        self.config_values = serde_json::to_value(config)?;
        self.statistics_snapshot = Value::Null;
        self.statistics_metric.clear();
        self.layout_dirty.set(true);
        unsafe {
            let _: bool = msg_send![&**self.window.as_ref().unwrap(),makeFirstResponder:std::ptr::null::<AnyObject>()];
        }
        if let Some(form) = &self.form {
            self.kit.release_bindings(&form.root);
        }
        self.controls.clear();
        self.live.clear();
        self.editors.clear();
        self.rule_table = None;
        self.stats_table = None;
        self.chart = None;
        self.current_pane = PANES
            .iter()
            .find(|(key, _)| key.eq_ignore_ascii_case(pane))
            .map(|(key, _)| *key)
            .unwrap_or("general")
            .into();
        if let Some(title) = &self.title {
            set_string(
                title,
                PANES
                    .iter()
                    .find(|(key, _)| *key == self.current_pane)
                    .unwrap()
                    .1,
            );
        }
        let mut form = Form::new(&self.kit);
        form.width = self.content_width();
        self.form = Some(form);
        match self.current_pane.as_str() {
            "setup" => self.setup(config, status),
            "general" => self.general(config, status),
            "models" => self.models(config, inventory),
            "context" => self.context(config, status),
            "personalization" => self.personalization(config, status),
            "emoji" => self.emoji(config),
            "shortcuts" => self.shortcuts(config),
            "battery" => self.battery(config, inventory),
            "appSettings" => self.rules(config, status),
            "labs" => self.labs(config),
            "statistics" => self.statistics(config),
            _ => self.about(config),
        }
        unsafe {
            let scroll = self.scroll.as_ref().unwrap();
            let _: () = msg_send![&**scroll,setDocumentView:&*self.form.as_ref().unwrap().root];
            let clip: *mut AnyObject = msg_send![&**scroll, contentView];
            let _: () = msg_send![clip,scrollToPoint:NSPoint::new(0.,0.)];
            let _: () = msg_send![&**scroll,reflectScrolledClipView:clip];
        }
        self.refresh(config, status, inventory, update)?;
        Ok(())
    }
    pub(super) fn visible(&self) -> bool {
        self.window
            .as_ref()
            .is_some_and(|w| unsafe { msg_send![&**w, isVisible] })
    }
    pub(super) fn pane(&self) -> &str {
        &self.current_pane
    }
    #[cfg(feature = "native-smoke")]
    pub(super) fn alignment_report(&self) -> anyhow::Result<serde_json::Value> {
        self.form
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("Settings form has not been built"))?
            .alignment_report()
    }
    /// Inspect AppKit's retained cells and rendered image pixels, without activating the window.
    #[cfg(feature = "native-smoke")]
    pub(super) fn sidebar_layout_report(&self) -> Result<Value> {
        fn geometry(value: NSRect) -> Value {
            json!({
                "x": value.origin.x, "y": value.origin.y,
                "width": value.size.width, "height": value.size.height,
            })
        }
        let sidebar = self
            .sidebar
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("Sidebar not built"))?;
        let scroll = self
            .sidebar_scroll
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("Sidebar scroll not built"))?;
        let selection = self
            .sidebar_selection
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("Sidebar selection not built"))?;
        let mut rows = Vec::with_capacity(PANES.len());
        let mut selected_keys = Vec::new();
        let mut automatic_highlights = 0;
        unsafe {
            let document: *mut AnyObject = msg_send![&**scroll, documentView];
            let clip: *mut AnyObject = msg_send![&**scroll, contentView];
            let document_bounds: NSRect = msg_send![document, bounds];
            let viewport: NSRect = msg_send![clip, bounds];
            let selection_frame: NSRect = msg_send![&**selection, frame];
            let fill: *mut AnyObject = msg_send![&**selection, fillColor];
            let fill_alpha: f64 = msg_send![fill, alphaComponent];
            let radius: f64 = msg_send![&**selection, cornerRadius];
            let hidden: bool = msg_send![&**selection, isHidden];
            let children: *mut AnyObject = msg_send![document, subviews];
            let child_count: usize = msg_send![children, count];
            let mut background_count = 0usize;
            let mut native_button_count = 0usize;
            for index in 0..child_count {
                let child: *mut AnyObject = msg_send![children,objectAtIndex:index];
                let is_box: bool = msg_send![child,isKindOfClass:AnyClass::get(c"NSBox").unwrap()];
                let is_button: bool =
                    msg_send![child,isKindOfClass:AnyClass::get(c"NSButton").unwrap()];
                background_count += usize::from(is_box);
                native_button_count += usize::from(is_button);
            }
            for (key, _) in PANES {
                let button = self
                    .sidebar_buttons
                    .get(*key)
                    .ok_or_else(|| anyhow::anyhow!("Missing sidebar destination {key}"))?;
                let bounds: NSRect = msg_send![&**button, bounds];
                let button_frame: NSRect = msg_send![&**button, frame];
                let cell: *mut AnyObject = msg_send![&**button, cell];
                let image_rect: NSRect = msg_send![cell,imageRectForBounds:bounds];
                let title_rect: NSRect = msg_send![cell,titleRectForBounds:bounds];
                let image: *mut AnyObject = msg_send![&**button, image];
                anyhow::ensure!(!image.is_null(), "Missing sidebar image for {key}");
                let (canvas, ink, glyph, pixels) = sidebar_image_ink(&*image)?;
                let project = |ink: NSRect| {
                    rect(
                        image_rect.origin.x + ink.origin.x * image_rect.size.width / canvas.width,
                        image_rect.origin.y + ink.origin.y * image_rect.size.height / canvas.height,
                        ink.size.width * image_rect.size.width / canvas.width,
                        ink.size.height * image_rect.size.height / canvas.height,
                    )
                };
                let tile = project(ink);
                let tile_in_document: NSRect =
                    msg_send![&**button,convertRect:tile,toView:document];
                let state: isize = msg_send![&**button, state];
                let highlights: usize = msg_send![cell, highlightsBy];
                let shows_state: usize = msg_send![cell, showsStateBy];
                let bordered: bool = msg_send![&**button, isBordered];
                let selected = state == 1;
                if selected {
                    selected_keys.push(*key);
                }
                if highlights != 0 || shows_state != 0 || bordered {
                    automatic_highlights += 1;
                }
                let target: *mut AnyObject = msg_send![&**button, target];
                let action: Option<objc2::runtime::Sel> = msg_send![&**button, action];
                let font: *mut AnyObject = msg_send![&**button, font];
                let font_size: f64 = msg_send![font, pointSize];
                let identifier: *mut NSString = msg_send![&**button, identifier];
                let title: *mut NSString = msg_send![&**button, title];
                rows.push(json!({
                    "key": key,
                    "identifier": identifier.as_ref().map(ToString::to_string),
                    "title": title.as_ref().map(ToString::to_string),
                    "selected": selected,
                    "state": state,
                    "button_frame": geometry(button_frame),
                    "button_bounds": geometry(bounds),
                    "image_cell_rect": geometry(image_rect),
                    "title_cell_rect": geometry(title_rect),
                    "tile_rect": geometry(tile),
                    "glyph_ink_rect": geometry(project(glyph)),
                    "image_canvas": {"width": canvas.width, "height": canvas.height},
                    "image_pixels": {"width": pixels.width, "height": pixels.height},
                    "tile_leading_padding": tile.origin.x - bounds.origin.x,
                    "tile_top_padding": tile.origin.y - bounds.origin.y,
                    "tile_bottom_padding": bounds.origin.y + bounds.size.height - tile.origin.y - tile.size.height,
                    "tile_title_separation": title_rect.origin.x - tile.origin.x - tile.size.width,
                    "selection_leading_padding": if selected { Some(tile_in_document.origin.x - selection_frame.origin.x) } else { None },
                    "selection_top_padding": if selected { Some(tile_in_document.origin.y - selection_frame.origin.y) } else { None },
                    "selection_bottom_padding": if selected { Some(selection_frame.origin.y + selection_frame.size.height - tile_in_document.origin.y - tile_in_document.size.height) } else { None },
                    "font_size": font_size,
                    "selection_matches_row": !selected || selection_frame == button_frame,
                    "fully_visible_in_viewport": button_frame.origin.y >= viewport.origin.y
                        && button_frame.origin.y + button_frame.size.height <= viewport.origin.y + viewport.size.height,
                    "bordered": bordered,
                    "cell_highlights_by": highlights,
                    "cell_shows_state_by": shows_state,
                    "has_native_action_target": !target.is_null() && action.is_some(),
                    "image_identity": image as usize,
                }));
            }
            let sidebar_frame: NSRect = msg_send![&**sidebar, frame];
            let scroll_frame: NSRect = msg_send![&**scroll, frame];
            let badge_frame = self.sidebar_badge.as_ref().map(|badge| {
                let value: NSRect = msg_send![&**badge, frame];
                geometry(value)
            });
            let footer_frame = self.sidebar_status.as_ref().map(|footer| {
                let value: NSRect = msg_send![&**footer, frame];
                geometry(value)
            });
            Ok(json!({
                "coordinate_space": "row-local cell rectangles; button/selection frames in sidebar document",
                "image_ink_measurement": "NSBitmapImageRep TIFF pixels: alpha > 0.05; white glyph RGB > 0.9",
                "pane": self.current_pane,
                "selected_keys": selected_keys,
                "row_count": rows.len(),
                "native_button_count": native_button_count,
                "selection_background_count": background_count,
                "sidebar_frame": geometry(sidebar_frame),
                "scroll_frame": geometry(scroll_frame),
                "document_bounds": geometry(document_bounds),
                "viewport_bounds": geometry(viewport),
                "requires_scrolling": document_bounds.size.height > viewport.size.height,
                "selection": {
                    "identifier": "inklow.sidebar.selection",
                    "frame": geometry(selection_frame),
                    "corner_radius": radius,
                    "fill_alpha": fill_alpha,
                    "hidden": hidden,
                },
                "automatic_highlight_count": automatic_highlights,
                "single_styled_highlight": background_count == 1 && !hidden && selected_keys.len() == 1 && automatic_highlights == 0 && fill_alpha >= 0.99,
                "badge_frame": badge_frame,
                "footer_frame": footer_frame,
                "rows": rows,
            }))
        }
    }
    pub(super) fn close(&mut self) {
        self.remember_drafts();
        if let Some(window) = &self.window {
            unsafe {
                let _: () = msg_send![&**window,orderOut:std::ptr::null::<AnyObject>()];
            }
        }
    }
    pub(super) fn remember_drafts(&mut self) {
        for (key, value) in &self.editors {
            self.draft_text.insert(key.clone(), string(&value.editor));
        }
    }
    pub(super) fn drafts(&self) -> BTreeMap<String, String> {
        let mut drafts = self.draft_text.clone();
        for (key, value) in &self.editors {
            drafts.insert(key.clone(), string(&value.editor));
        }
        drafts
    }
    pub(super) fn set_draft(&mut self, key: &str, text: &str) {
        self.draft_text.insert(key.into(), text.into());
        if let Some(instructions) = self.editors.get(key) {
            set_string(&instructions.editor, text);
        }
    }
    pub(super) fn saved(&mut self, key: &str) {
        self.draft_text.remove(key);
    }
    pub(super) fn set_power_card(&mut self, card: Obj) {
        if let Some(old) = self.power_card.replace(card.clone()) {
            unsafe {
                let _: () = msg_send![&*old, removeFromSuperview];
            }
        }
        unsafe {
            let _: () = msg_send![&*card,setAutoresizingMask:18usize];
        }
        if let Some(slot) = self.live.get("powerCard") {
            add(slot, &card);
            let bounds: NSRect = unsafe { msg_send![&**slot, bounds] };
            frame(&card, &bounds);
        }
    }
    pub(super) fn apply_ui_action(&mut self, action: &UiAction) -> bool {
        match action {
            UiAction::ModelSelect(id) => {
                self.model_selection = Some(id.clone());
                false
            }
            UiAction::SelectRule(key) => {
                self.remember_drafts();
                self.selected_rule = key.clone();
                true
            }
            UiAction::FilterRules(query) => {
                self.rule_query = query.clone();
                false
            }
            UiAction::ModelAdvanced(value) => {
                self.advanced = *value;
                true
            }
            UiAction::Statistics {
                period,
                group,
                metric,
            } => {
                if let Some(v) = period {
                    self.stat_period = v.clone();
                }
                if let Some(v) = group {
                    self.stat_group = v.clone();
                }
                if let Some(v) = metric {
                    self.stat_metric = v.clone();
                }
                false
            }
            _ => false,
        }
    }
    fn content_width(&self) -> f64 {
        self.scroll
            .as_ref()
            .map(|scroll| unsafe {
                let clip: *mut AnyObject = msg_send![&**scroll, contentView];
                let bounds: NSRect = msg_send![clip, bounds];
                bounds.size.width
            })
            .unwrap_or(690.)
    }
    pub(super) fn reflow(&mut self) {
        let Some(root) = &self.root else { return };
        let bounds: NSRect = unsafe { msg_send![&**root, bounds] };
        let width = bounds.size.width;
        let height = bounds.size.height;
        let content = (width - 210.).max(450.);
        frame(self.sidebar.as_ref().unwrap(), &rect(0., 0., 210., height));
        if let Some(scroll) = &self.sidebar_scroll {
            let clip: *mut AnyObject = unsafe { msg_send![&**scroll, contentView] };
            let previous: NSRect = unsafe { msg_send![clip, bounds] };
            frame(scroll, &rect(8., 50., 194., (height - 120.).max(1.)));
            let viewport: NSRect = unsafe { msg_send![clip, bounds] };
            if previous.size != viewport.size {
                // Resizing may clip a previously visible selection; ordinary refreshes
                // must not pull the user's independent sidebar scrolling back to it.
                for button in self.sidebar_buttons.values() {
                    unsafe {
                        let state: isize = msg_send![&**button, state];
                        if state == 1 {
                            let bounds: NSRect = msg_send![&**button, bounds];
                            let _: bool = msg_send![&**button,scrollRectToVisible:bounds];
                            break;
                        }
                    }
                }
            }
        }
        if let Some(footer) = &self.sidebar_status {
            frame(footer, &rect(20., height - 66., 174., 48.));
        }
        frame(
            self.title.as_ref().unwrap(),
            &rect(
                210. + panes::CONTENT_INSET,
                26.,
                content - 2. * panes::CONTENT_INSET,
                24.,
            ),
        );
        frame(
            self.scroll.as_ref().unwrap(),
            &rect(210., 52., content, (height - 52.).max(100.)),
        );
        let scroll = self.scroll.as_ref().unwrap();
        let clip: *mut AnyObject = unsafe { msg_send![&**scroll, contentView] };
        let original: NSRect = unsafe { msg_send![clip, bounds] };
        if let Some(form) = &mut self.form {
            form.reflow(original.size.width, original.size.height);
            // Legacy scrollers can change the clip width when the document grows.
            let viewport: NSRect = unsafe { msg_send![clip, bounds] };
            if viewport.size != original.size {
                form.reflow(viewport.size.width, viewport.size.height);
            }
            unsafe {
                let document: NSRect = msg_send![&*form.root, frame];
                let maximum = (document.size.height - viewport.size.height).max(0.);
                let _: () = msg_send![clip,scrollToPoint:NSPoint::new(0.,original.origin.y.max(0.).min(maximum))];
                let _: () = msg_send![&**scroll,reflectScrolledClipView:clip];
            }
        }
        if let Some(chart) = &self.chart {
            chart.reflow();
        }
        if let (Some(card), Some(slot)) = (&self.power_card, self.live.get("powerCard")) {
            let bounds: NSRect = unsafe { msg_send![&**slot, bounds] };
            frame(card, &bounds);
        }
    }
    fn section(&mut self, title: &str) {
        self.form.as_mut().unwrap().section(&self.kit, title);
    }
    fn note(&mut self, text: &str) {
        self.form.as_mut().unwrap().note(&self.kit, text);
    }
    fn row(&mut self, title: &str, detail: &str, control: Option<Obj>) {
        self.form
            .as_mut()
            .unwrap()
            .row(&self.kit, title, detail, control);
    }
    fn custom(&mut self, view: Obj, height: f64) {
        self.form.as_mut().unwrap().custom(view, height);
    }
    fn live_label(&mut self, key: &str, id: &str, text: &str, height: f64) {
        let view = self.kit.label(text, rect(0., 0., 550., height), 13., false);
        identify(&view, id, text);
        self.live.insert(key.into(), view.clone());
        // Live messages grow from measured text instead of reserving stale multi-line padding.
        self.custom(view, 24.);
    }
    fn button(&mut self, title: &str, id: &str, action: UiAction) -> Obj {
        let view = self.kit.button(title, id, rect(0., 0., 530., 28.), action);
        self.live.insert(id.into(), view.clone());
        view
    }
    fn action_row(&mut self, title: &str, detail: &str, button: &str, id: &str, action: UiAction) {
        let view = self.button(button, id, action);
        self.row(title, detail, Some(view));
    }
    fn toggle(&mut self, _config: &Config, field: &str, title: &str, detail: &str) {
        let id = format!("inklow.{}.{}", self.current_pane, field);
        let view = obj("NSSwitch");
        let size: NSSize = unsafe { msg_send![&*view, intrinsicContentSize] };
        frame(&view, &rect(0., 0., size.width, size.height));
        unsafe {
            let _: () = msg_send![&*view,setState:isize::from(self.config_values[field].as_bool().unwrap_or(false))];
        }
        self.kit.rebind(
            &view,
            UiAction::Set(field.into(), Value::Null),
            ReadValue::Boolean,
        );
        identify(&view, &id, title);
        self.controls.push(Control {
            view: view.clone(),
            field: field.into(),
            options: Vec::new(),
            kind: 0,
        });
        self.row(title, detail, Some(view));
    }
    fn choice(
        &mut self,
        field: &str,
        title: &str,
        detail: &str,
        options: Vec<(String, Value)>,
        selected: Value,
        action: UiAction,
        config_field: bool,
    ) {
        let id = format!("inklow.{}.{}", self.current_pane, field);
        let view = self.kit.popup(
            &id,
            title,
            rect(0., 0., 530., 28.),
            &options,
            &selected,
            action,
        );
        self.live.insert(id, view.clone());
        if config_field {
            self.controls.push(Control {
                view: view.clone(),
                field: field.into(),
                options: options.iter().map(|(_, v)| v.clone()).collect(),
                kind: 1,
            });
        }
        self.row(title, detail, Some(view));
    }
    fn setting_choice(
        &mut self,
        _config: &Config,
        field: &str,
        title: &str,
        detail: &str,
        items: &[(&str, &str)],
    ) {
        self.choice(
            field,
            title,
            detail,
            items
                .iter()
                .map(|(label, value)| ((*label).into(), json!(value)))
                .collect(),
            self.config_values[field].clone(),
            UiAction::Set(field.into(), Value::Null),
            true,
        );
    }
    fn number(&mut self, _config: &Config, field: &str, title: &str, detail: &str) {
        self.text(
            field,
            title,
            detail,
            &self.config_values[field].to_string(),
            true,
        );
    }
    fn text(&mut self, field: &str, title: &str, detail: &str, value: &str, integer: bool) {
        let view = self.kit.text(
            &format!("inklow.{}.{}", self.current_pane, field),
            title,
            rect(0., 0., 530., 28.),
            value,
            integer,
            UiAction::Set(field.into(), Value::Null),
        );
        self.controls.push(Control {
            view: view.clone(),
            field: field.into(),
            options: Vec::new(),
            kind: 2,
        });
        self.row(title, detail, Some(view));
    }
    fn instructions(&mut self, key: &str, saved: &str, rule: bool) {
        let scope = if rule { "rule" } else { "global" };
        let text = self
            .draft_text
            .get(key)
            .map(String::as_str)
            .unwrap_or(saved);
        let editor = self.kit.editor(
            &format!("inklow.{}.{}Instructions", self.current_pane, scope),
            if rule {
                "App or website custom instructions"
            } else {
                "Global custom instructions"
            },
            rect(0., 0., 530., 152.),
            text,
        );
        let scroll = panes::scroll(&editor, 160.);
        self.custom(scroll, 160.);
        let save = self.button(
            "Save Changes",
            &format!("inklow.{}.{}.saveInstructions", self.current_pane, scope),
            UiAction::SaveInstructions(key.into()),
        );
        let revert = self.button(
            "Revert",
            &format!("inklow.{}.{}.revertInstructions", self.current_pane, scope),
            UiAction::RevertInstructions(key.into()),
        );
        let mut actions = Vec::new();
        if !rule {
            actions.push(self.button(
                "Reset to Default",
                &format!("inklow.{}.resetInstructions", self.current_pane),
                UiAction::ResetInstructions(key.into()),
            ));
        }
        actions.push(revert.clone());
        actions.push(save.clone());
        self.form.as_mut().unwrap().actions(&self.kit, actions);
        self.editors.insert(
            key.into(),
            Instructions {
                editor,
                save,
                revert,
            },
        );
    }
    fn setup(&mut self, config: &Config, status: &Value) {
        self.live_label(
            "setupSummary",
            "inklow.setup.summary",
            "Checking setup…",
            34.,
        );
        self.note("This checklist reads actual macOS permission, model and preference state. Nothing changes until you press a button.");
        self.section("Required");
        for (key, title, detail, action) in [
            (
                "accessibility",
                "Accessibility",
                "Reads the focused text field and inserts accepted text at its caret.",
                UiAction::Permission("accessibility".into()),
            ),
            (
                "input_monitoring",
                "Input Monitoring",
                "Detects typing and acceptance shortcuts.",
                UiAction::Permission("input_monitoring".into()),
            ),
            (
                "model",
                "AI model",
                "A local model or configured literal loopback server. Downloads start only when requested.",
                UiAction::Navigate("models".into()),
            ),
        ] {
            self.setup_row(key, title, detail, action);
        }
        self.section("Recommended");
        self.setup_row("inline_prediction","macOS inline predictive text","Its grey text competes with BroTypist. Turn off Keyboard → Text Input → Edit… → Show inline predictive text. BroTypist never changes it.",UiAction::Permission("keyboard".into()));
        self.section("Optional");
        for (field, title) in [
            ("screen_context", "Focused-window context"),
            ("clipboard_context", "Clipboard context"),
        ] {
            let on = if field == "screen_context" {
                config.screen_context
            } else {
                config.clipboard_context
            };
            self.setup_row(
                field,
                title,
                "Use local context for eligible requests. Enabling asks for confirmation.",
                if on {
                    UiAction::Navigate("context".into())
                } else {
                    UiAction::Set(field.into(), json!(true))
                },
            );
        }
        self.setup_row("screen_recording","Screen & System Audio Recording","Needed only for opt-in screenshot features. Images stay in memory and are discarded immediately.",screen_action(status));
        self.setup_row("terminal_completion","Terminal AI-prompt support (opt-in)","Supported AI-prompt fields only. Shell prompts remain excluded; custom exclusions remain hard blocks.",UiAction::Set("terminal_completion".into(),json!(!config.terminal_completion)));
        for (key, host, name) in [
            (
                "terminal_automation",
                "com.apple.Terminal",
                "Terminal Automation",
            ),
            (
                "iterm_automation",
                "com.googlecode.iterm2",
                "iTerm2 Automation",
            ),
        ] {
            let state = permission(status, key);
            let action = if ["granted", "denied"].contains(&state.as_str()) {
                UiAction::Permission("automation".into())
            } else {
                UiAction::RequestAutomation(host.into())
            };
            self.setup_row(
                key,
                name,
                "Open the terminal host first. Only Request Access asks macOS for permission.",
                action,
            );
        }
        self.note("Automation permission does not prove terminal input is supported. Suggestions require a safe AI-prompt field and verified same-line caret. Google Docs accessibility is enabled separately from its document accessory menu.");
        self.note("Permission changes may require restarting BroTypist.");
        self.live_label("nativeStatus", "inklow.setup.nativeStatus", "", 60.);
        self.action_row(
            "Welcome & tour",
            "Review setup and try a real local suggestion.",
            "Show Welcome…",
            "inklow.setup.welcome",
            UiAction::Welcome,
        );
    }
    fn setup_row(&mut self, key: &str, title: &str, detail: &str, action: UiAction) {
        let container = self.kit.view(rect(0., 0., 330., 28.));
        let status = self
            .kit
            .label("Checking…", rect(0., 5., 150., 20.), 12., false);
        identify(&status, &format!("inklow.setup.{key}.status"), title);
        self.live
            .insert(format!("setup.{key}.status"), status.clone());
        let button = self.button("Open Settings…", &format!("inklow.setup.{key}"), action);
        layout_status_action(&container, &status, &button);
        self.row(title, detail, Some(container));
    }
    fn general(&mut self, c: &Config, status: &Value) {
        self.section("Startup");
        self.toggle(
            c,
            "launch_at_login",
            "Launch BroTypist at login",
            "Start this menu utility when you sign in. The actual login-item state is shown.",
        );
        if let Some(control) = self.controls.last() {
            unsafe {
                let _: () = msg_send![&*control.view,setState:isize::from(status["launch_at_login"].as_bool().unwrap_or(c.launch_at_login))];
            }
        }
        self.toggle(c,"show_accessory","Show accessory button","A Ghost Caret beside the text field with app completion, recording and Tab controls. Option-click types a normal Tab.");
        self.row(
            "Menu bar icon",
            "Always shown so BroTypist can be paused, reopened or quit.",
            None,
        );
        self.section("Completions");
        self.toggle(
            c,
            "global_enabled",
            "Predictions everywhere",
            "Master switch, including apps and websites with their own rules.",
        );
        self.toggle(c,"enabled","Predictions in apps without a rule","Default for apps and websites not listed in App Settings. Rules never override the master switch.");
        self.toggle(
            c,
            "complete_inside_text",
            "Complete inside existing text",
            "Also suggest when there is text after the caret.",
        );
        self.setting_choice(
            c,
            "completion_length",
            "Maximum completion length",
            "Medium is recommended. Current word only never suggests the next word.",
            &[
                ("Current word only", "word"),
                ("Short (~1–2 words)", "short"),
                ("Medium (~2–4 words)", "medium"),
                ("Long (~5–8 words)", "long"),
            ],
        );
        self.section("Autocorrect");
        self.toggle(
            c,
            "typo_suppression",
            "Don’t show completions when a typo is suspected",
            "Checks the word being typed locally using macOS spelling for the detected language.",
        );
        let accept = c.shortcuts.get("word").map(String::as_str).unwrap_or("Tab");
        self.toggle(c,"typo_fixes","Show suggested fixes",&format!("Strikes through a likely typo and shows its fix. Press {accept} to replace the word."));
    }
    fn models(&mut self, c: &Config, inventory: &Value) {
        self.section("AI Models");
        if c.backend == "gguf" {
            let entries = model_entries(inventory);
            if let Some(recommended) = entries
                .iter()
                .find(|m| m["recommended"].as_bool() == Some(true))
                .or_else(|| inventory.get("recommended").filter(|m| m.is_object()))
            {
                self.row(
                    &format!("Recommended: {}", text(recommended, "label")),
                    &format!(
                        "{} · Chosen for this Mac’s memory. Downloads require approval.",
                        gb(recommended["size_bytes"].as_f64().unwrap_or(0.))
                    ),
                    None,
                );
            }
            let mut options = vec![("Imported local model".into(), json!(""))];
            for model in entries {
                options.push((model_title(model), model["id"].clone()));
            }
            let selected = selected_model_id(c, self.model_selection.as_deref(), inventory);
            self.choice("model_preset","Choose a model","✓ installed · ↓ download required. Installed selections are used immediately; other choices await approval.",options,json!(selected),UiAction::ModelSelect(String::new()),false);
            self.live_label("modelDescription", "inklow.models.description", "", 80.);
            let download = self.button(
                "Download Model",
                "inklow.models.download",
                UiAction::ModelDownload(selected),
            );
            let cancel = self.button(
                "Cancel Download",
                "inklow.models.cancelDownload",
                UiAction::ModelCancel,
            );
            let import = self.button(
                "Import Model…",
                "inklow.models.chooseModel",
                UiAction::ModelImport,
            );
            self.form
                .as_mut()
                .unwrap()
                .actions(&self.kit, vec![download, cancel, import]);
            let progress = self
                .kit
                .progress("inklow.models.downloadProgress", rect(0., 0., 550., 16.));
            self.live.insert("modelProgress".into(), progress.clone());
            self.custom(progress, 16.);
            self.live_label("downloadStatus", "inklow.models.downloadStatus", "", 58.);
        }
        self.section("Current Model");
        self.live_label("currentModel", "inklow.models.currentModel", "", 60.);
        self.action_row(
            "Model files",
            "Models stay on this Mac.",
            "Reveal in Finder",
            "inklow.models.revealModels",
            UiAction::RevealModels,
        );
        let advanced = self.kit.button(
            "",
            "inklow.models.advanced",
            rect(0., 0., 24., 24.),
            UiAction::ModelAdvanced(self.advanced),
        );
        unsafe {
            let _: () = msg_send![&*advanced,setButtonType:6usize];
            let _: () = msg_send![&*advanced,setBezelStyle:5usize];
            let _: () = msg_send![&*advanced,setState:isize::from(self.advanced)];
        }
        self.kit.rebind(
            &advanced,
            UiAction::ModelAdvanced(self.advanced),
            ReadValue::Boolean,
        );
        identify(
            &advanced,
            "inklow.models.advanced",
            "Advanced model settings",
        );
        self.row(
            "Advanced model settings",
            "Backend, file diagnostics, acceleration and prompt format.",
            Some(advanced),
        );
        if self.advanced {
            self.section("Advanced");
            self.setting_choice(
                c,
                "backend",
                "Inference backend",
                "On-device inference or literal loopback servers only.",
                &[
                    ("On-device GGUF / llama.cpp", "gguf"),
                    ("Local Ollama server", "ollama"),
                    ("Local OpenAI-compatible server", "openai"),
                ],
            );
            if c.backend == "gguf" {
                self.live_label(
                    "modelFile",
                    "inklow.models.modelFile",
                    &format!(
                        "Current GGUF file: {}",
                        if c.model_path.is_empty() {
                            "None selected"
                        } else {
                            &c.model_path
                        }
                    ),
                    60.,
                );
            } else {
                self.text(
                    "endpoint",
                    "Loopback endpoint",
                    "Press Return to save.",
                    &c.endpoint,
                    false,
                );
                self.text(
                    "model",
                    "Model name",
                    "Press Return to save.",
                    &c.model,
                    false,
                );
            }
            self.setting_choice(
                c,
                "acceleration",
                "Hardware acceleration",
                "",
                &[
                    ("Automatic (Metal when available)", "auto"),
                    ("CPU only", "cpu"),
                ],
            );
            self.setting_choice(
                c,
                "prompt_style",
                "Model prompt style",
                "Preset models select their format automatically.",
                &[
                    ("Text continuation", "continuation"),
                    ("Instruct writing model", "writing"),
                ],
            );
        }
    }
    fn context(&mut self, c: &Config, status: &Value) {
        self.section("Accessibility Context");
        self.toggle(c,"screen_context","Use focused-window text","Read relevant Accessibility text from the verified focused window. Inaccessible content is skipped.");
        self.toggle(
            c,
            "clipboard_context",
            "Use clipboard for context",
            "Read only for eligible local model requests, never stored in history.",
        );
        self.section("Screenshots (Opt-in)");
        self.toggle(c,"screenshot_context","Use screenshots for context","When Accessibility text is unavailable, recognize text around the field in memory on this Mac. Images are never saved or uploaded.");
        self.toggle(c,"screenshot_appearance","Use screenshots to improve suggestion appearance","Measure font and baseline in memory. Suggestions remain hidden if measurement cannot be verified.");
        let container = self.kit.view(rect(0., 0., 330., 28.));
        let status_label = self.kit.label("", rect(0., 5., 150., 20.), 12., false);
        identify(
            &status_label,
            "inklow.context.screenRecording.status",
            "Screen recording permission",
        );
        self.live
            .insert("screenRecording".into(), status_label.clone());
        let button = self.button(
            "Request Access…",
            "inklow.context.screenRecording",
            screen_action(status),
        );
        layout_status_action(&container, &status_label, &button);
        self.row("Screen & System Audio Recording","macOS permission required for screenshot options. macOS may show its recording indicator; granting access usually requires relaunch.",Some(container));
        self.note("Secure fields, excluded apps and blocked websites are skipped before reading any context. Context is never saved to history or sent to a remote service.");
        self.live_label(
            "contextStatus",
            "inklow.context.status",
            "Context: Not requested",
            60.,
        );
    }
    fn personalization(&mut self, c: &Config, status: &Value) {
        self.section("Local Writing Learning");
        self.toggle(c,"record_history","Learn locally from recorded writing","Opt-in encrypted history; key stays in OS secure storage. Short texts and likely secrets are skipped. Scoped rules take precedence.");
        self.toggle(c,"record_unaccepted","Also learn when you accept no suggestion","Only while history is enabled for this app or website. Otherwise only accepted-suggestion sessions are recorded.");
        let slider = obj("NSSlider");
        identify(
            &slider,
            "inklow.personalization.personalization_strength",
            "Personalize writing style and word choice",
        );
        frame(&slider, &rect(32., 2., 210., 24.));
        unsafe {
            let _: () = msg_send![&*slider,setMinValue:0.0f64];
            let _: () = msg_send![&*slider,setMaxValue:100.0f64];
            let _: () = msg_send![&*slider,setDoubleValue:c.personalization_strength as f64];
            let _: () = msg_send![&*slider,setContinuous:false];
            let _: () = msg_send![&*slider,setNumberOfTickMarks:6isize];
            let _: () = msg_send![&*slider,setAllowsTickMarkValuesOnly:false];
        }
        self.kit.rebind(
            &slider,
            UiAction::Set("personalization_strength".into(), Value::Null),
            ReadValue::Slider,
        );
        self.controls.push(Control {
            view: slider.clone(),
            field: "personalization_strength".into(),
            options: Vec::new(),
            kind: 3,
        });
        let container = self.kit.view(rect(0., 0., 280., 28.));
        let off = self.kit.label("Off", rect(0., 5., 30., 18.), 12., false);
        let max = self.kit.label("Max", rect(248., 5., 32., 18.), 12., false);
        unsafe {
            let _: () = msg_send![&*off,setAutoresizingMask:40usize];
            let _: () = msg_send![&*slider,setAutoresizingMask:42usize];
            let _: () = msg_send![&*max,setAutoresizingMask:41usize];
        }
        add(&container, &off);
        add(&container, &slider);
        add(&container, &max);
        self.row("Personalize style and word choice","Bounded evidence from observed edits guides local suggestions. Off ignores learned evidence; explicit instructions and the draft’s language win.",Some(container));
        self.note("Learning never retrains model weights. It matches the app or exact website and confidently detected English, Portuguese, Spanish, French or German. Ambiguous language means no learned context.");
        self.note("Only new observed edits establish authorship, not existing drafts, bulk pastes, older history or BroTypist-generated text. Deletion forgets derived style and cancels suggestions using it.");
        self.live_label("historyInfo", "inklow.personalization.historyInfo", "", 65.);
        self.section("Recorded Texts by App and Website");
        self.action_row(
            "Show counts",
            "Unlock encrypted history once on this Mac to count records by app and website.",
            "Show Counts…",
            "inklow.personalization.showCounts",
            UiAction::HistoryCounts,
        );
        let counts = &status["history_counts"];
        for (scope, kind) in [("apps", "app"), ("domains", "domain")] {
            if let Some(scopes) = counts[scope].as_object() {
                for (name, count) in scopes {
                    let key = format!("{kind}:{name}");
                    self.action_row(
                        &format!("{name} — {} texts", count.as_u64().unwrap_or(0)),
                        "Deletes only this scope’s encrypted history and learned style.",
                        "Delete…",
                        &format!("inklow.personalization.delete.{key}"),
                        UiAction::DeleteHistory {
                            app: (kind == "app").then(|| name.clone()),
                            domain: (kind == "domain").then(|| name.clone()),
                        },
                    );
                }
            }
        }
        self.action_row(
            "Forget all learned writing",
            "Deletes encrypted history and derived style. Aggregate statistics are unchanged.",
            "Delete All…",
            "inklow.personalization.deleteHistory",
            UiAction::DeleteHistory {
                app: None,
                domain: None,
            },
        );
        self.section("Custom AI Instructions");
        self.row("Your writing preferences","Pre-filled from Mac language and region. Scoped instructions are appended. Edits apply only after Save Changes.",None);
        self.instructions("global", &c.custom_instructions, false);
    }
    fn emoji(&mut self, c: &Config) {
        self.section("Emoji Suggestions");
        self.toggle(c,"emoji_enabled","Suggest emoji after typing a colon","At line start or after a space, type :sweat_smile in an eligible field. Choose a numbered result. macOS picker remains Control-Command-Space.");
        self.toggle(c,"emoticons_enabled","Suggest emoji from emoticons","Offer emoji for :-) ;-) :( or <3. Accept explicitly; text is never silently rewritten.");
        self.section("Emoji Preferences");
        let labels = [
            "Neutral 👋",
            "Light 👋🏻",
            "Medium-light 👋🏼",
            "Medium 👋🏽",
            "Medium-dark 👋🏾",
            "Dark 👋🏿",
        ];
        self.choice(
            "emoji_skin_tone",
            "Preferred skin tone",
            "",
            labels
                .iter()
                .enumerate()
                .map(|(i, l)| ((*l).into(), json!(i)))
                .collect(),
            json!(c.emoji_skin_tone),
            UiAction::Set("emoji_skin_tone".into(), Value::Null),
            true,
        );
        self.toggle(
            c,
            "emoji_include_neutral",
            "Include neutral variant",
            "Also show the unmodified emoji.",
        );
        self.setting_choice(
            c,
            "emoji_gender",
            "Preferred gender",
            "",
            &[
                ("Gender-Neutral", "neutral"),
                ("Female", "female"),
                ("Male", "male"),
            ],
        );
        self.note("The numbered chooser accepts keys 1–9 or clicks without taking focus from your field. Emoji lookup is local and needs no inference worker.");
    }
    fn shortcuts(&mut self, c: &Config) {
        self.live_label("secureInput", "inklow.shortcuts.secureInput", "", 60.);
        self.section("Accepting Completions");
        self.recorder(c,"word","Complete only the next word","Click then press a chord. Delete clears; Escape cancels. Press repeatedly to walk through a suggestion. Per-app Tab is in App Settings.");
        self.toggle(
            c,
            "trailing_space",
            "Include trailing space after a single word",
            "Accepting one word inserts the following space when available.",
        );
        self.recorder(c,"all","Trigger full completion","AboveTab is the physical key above Tab independent of keyboard layout. Default: no modifiers.");
        self.setting_choice(
            c,
            "punctuation_mode",
            "Include trailing punctuation",
            "Omitted punctuation remains in the suggestion for the next acceptance.",
            &[
                ("Only with full completion", "full"),
                ("With either shortcut", "always"),
                ("Never", "never"),
            ],
        );
        self.note("Option-Tab or Option-clicking the accessory types a normal Tab.");
        self.section("When You Press Escape");
        self.setting_choice(
            c,
            "escape_behavior",
            "Escape action",
            "Consumed only when dismissing a visible suggestion; a second Escape reaches the app.",
            &[
                ("Pause completions briefly", "pause"),
                ("Turn off until switching apps", "app"),
                ("Leave Escape to the app", "passthrough"),
            ],
        );
        self.number(
            c,
            "escape_pause_seconds",
            "Brief pause (seconds)",
            "1–3600 seconds.",
        );
        self.section("Activation & Toggles");
        self.recorder(
            c,
            "force",
            "Force-activate completions",
            "Request a suggestion at the current eligible caret.",
        );
        self.recorder(
            c,
            "app_toggle",
            "Temporarily toggle the current app",
            "Toggle the app or website for a few minutes without changing its saved rule.",
        );
        self.recorder(
            c,
            "toggle",
            "Toggle completions globally",
            "Optional global pause/resume shortcut.",
        );
        self.section("Word Tools");
        self.recorder(
            c,
            "alternatives",
            "Show alternative suggestions",
            "Optional shortcut for the alternatives chooser; configure in Labs.",
        );
        self.recorder(
            c,
            "synonyms",
            "Look up synonyms",
            "Optional shortcut for synonyms of an explicitly selected word; configure in Labs.",
        );
    }
    fn recorder(&mut self, c: &Config, key: &str, title: &str, detail: &str) {
        let value = c
            .shortcuts
            .get(key)
            .map(String::as_str)
            .filter(|s| !s.is_empty())
            .unwrap_or("Record Shortcut…");
        let button = self.button(
            value,
            &format!("inklow.shortcuts.{key}"),
            UiAction::RecordShortcut(key.into()),
        );
        self.row(title, detail, Some(button));
    }
    pub(super) fn recording(&mut self, action: Option<&str>) {
        if let Some(key) = action {
            if let Some(button) = self.live.get(&format!("inklow.shortcuts.{key}")) {
                set_title(button, "Type shortcut…");
            }
        }
    }
    fn battery(&mut self, c: &Config, inventory: &Value) {
        let slot = self.kit.view(rect(0., 0., 550., 84.));
        identify(&slot, "inklow.battery.powerCard", "Current power state");
        if let Some(card) = &self.power_card {
            add(&slot, card);
            let bounds: NSRect = unsafe { msg_send![&*slot, bounds] };
            frame(card, &bounds);
        }
        self.live.insert("powerCard".into(), slot.clone());
        self.custom(slot, 84.);
        self.section("On Battery Power");
        self.toggle(
            c,
            "battery_on_demand",
            "Only show completions on demand",
            "On battery, generate only after the force-activation shortcut.",
        );
        let mut options = vec![("Same as usual".into(), json!("same"))];
        for model in model_entries(inventory) {
            if model["installed"].as_bool() == Some(true) || model["id"] == c.battery_model {
                options.push((
                    format!(
                        "{}{}",
                        model_title(model),
                        if model["installed"].as_bool() == Some(true) {
                            ""
                        } else {
                            " — not downloaded"
                        }
                    ),
                    model["id"].clone(),
                ));
            }
        }
        self.choice("battery_model","Use a smaller model","Only downloaded catalogue models are offered. Nothing downloads automatically. Applies to local GGUF models.",options,json!(c.battery_model),UiAction::Set("battery_model".into(),Value::Null),true);
        self.toggle(
            c,
            "battery_shorter",
            "Generate slightly shorter completions",
            "Reduce completion length while on battery.",
        );
        self.section("Inference Memory");
        self.number(c,"idle_release_seconds","Release model after idle (seconds)","0 preloads the cached model at startup and keeps it ready. 1–3600 releases after inactivity. Pause and Quit release immediately.");
        self.note("With a timeout, preparation begins only in a supported multiline writing field. Startup preparation at 0 sends no writing and respects battery on-demand mode. Models never download automatically.");
    }
    fn rules(&mut self, c: &Config, status: &Value) {
        self.section("Apps and Websites");
        let search = obj("NSSearchField");
        set_string(&search, &self.rule_query);
        identify(
            &search,
            "inklow.appSettings.search",
            "Search apps and websites",
        );
        unsafe {
            let _: () = msg_send![&*search,setSendsSearchStringImmediately:true];
            let _: () = msg_send![&*search,setPlaceholderString:&*NSString::from_str("Search apps and websites")];
        }
        self.kit.rebind(
            &search,
            UiAction::FilterRules(String::new()),
            ReadValue::Text,
        );
        self.custom(search, 30.);
        let mut actions = Vec::new();
        for (title, id, action) in [
            ("Add App…", "addApp", UiAction::AddApp),
            ("Add Current App", "addCurrentApp", UiAction::AddCurrentApp),
            ("Add Domain…", "addDomain", UiAction::AddDomain),
        ] {
            actions.push(self.button(title, &format!("inklow.appSettings.{id}"), action));
        }
        self.form.as_mut().unwrap().actions(&self.kit, actions);
        let table = Table::new(
            &self.kit,
            "inklow.appSettings.list",
            &[
                ("name", "Application / Website", 240.),
                ("summary", "Overrides", 300.),
            ],
            true,
        );
        self.custom(table.scroll.clone(), 220.);
        self.rule_table = Some(table);
        self.note("Drag an app from Finder or a browser link onto the list. Website rules work when the browser exposes its URL. Custom exclusions remain hard blocks, including with Terminal opt-in.");
        self.section("Hard App Exclusions");
        for excluded in &c.excluded_apps {
            self.action_row(
                excluded,
                "Excluded before capture, context and inference.",
                "Remove Exclusion",
                &format!("inklow.appSettings.exclusion.{excluded}"),
                UiAction::RemoveExclusion(excluded.clone()),
            );
        }
        if !c.app_rules.contains_key(&self.selected_rule) {
            self.selected_rule = c.app_rules.keys().next().cloned().unwrap_or_default();
        }
        let key = self.selected_rule.clone();
        let Some(rule) = c.app_rules.get(&key) else {
            self.note("No app or website has its own settings. Everything follows General, Context and Personalization.");
            return;
        };
        self.section(key.split_once(':').map(|(_, id)| id).unwrap_or(&key));
        for (field, title, detail) in [
            (
                "enabled",
                "Completions",
                "Inherit follows General or the browser’s app rule for a website.",
            ),
            (
                "tab_enabled",
                "Accept with Tab",
                "Other acceptance shortcuts keep working.",
            ),
            (
                "record_history",
                "Record writing",
                "Enabled asks for consent and requires secure storage, even when global history is off. Disabled never records here.",
            ),
        ] {
            self.rule_choice(&key, rule, field, title, detail, false);
        }
        if key.starts_with("app:") {
            self.rule_choice(
                &key,
                rule,
                "improve_compatibility",
                "Improve compatibility with this app",
                "Automatic requests full accessibility only for Electron/Chromium. Off never does.",
                true,
            );
            self.rule_choice(&key,rule,"complete_in_code_editors","Complete in code editors","Code editing is off by default. Explicit permission allows other writable fields in this editor; recognized stock chat fields may work automatically.",false);
        }
        let count = scope_count(status, &key);
        self.live_label(
            "ruleCount",
            "inklow.appSettings.recordedCount",
            &format!(
                "Recorded texts: {}",
                count
                    .map(|v| v.to_string())
                    .unwrap_or_else(|| "not counted".into())
            ),
            30.,
        );
        self.action_row(
            "Recorded texts",
            "Unlocks encrypted history once to count records.",
            "Show Count…",
            "inklow.appSettings.showCount",
            UiAction::HistoryCounts,
        );
        let (app, domain) = if let Some(app) = key.strip_prefix("app:") {
            (Some(app.into()), None)
        } else {
            (None, key.strip_prefix("domain:").map(str::to_owned))
        };
        self.action_row(
            "Delete recorded texts here",
            "Irreversible. Deletes only this app or exact website.",
            "Delete History…",
            "inklow.appSettings.deleteHistory",
            UiAction::DeleteHistory { app, domain },
        );
        self.action_row(
            "Remove these settings",
            "Restores inherited behavior. Does not delete recorded texts.",
            "Remove…",
            "inklow.appSettings.deleteRule",
            UiAction::RemoveRule(key.clone()),
        );
        self.section("Custom Instructions");
        self.row("Additional instructions","Added after global instructions for this app or website. Unsaved drafts survive switching panes.",None);
        self.instructions(
            &format!("rule:{key}"),
            rule.get("custom_instructions")
                .and_then(Value::as_str)
                .unwrap_or(""),
            true,
        );
    }
    fn rule_choice(
        &mut self,
        key: &str,
        rule: &BTreeMap<String, Value>,
        field: &str,
        title: &str,
        detail: &str,
        compatibility: bool,
    ) {
        let items = if compatibility {
            [("Automatic", "auto"), ("On", "on"), ("Off", "off")]
        } else {
            [
                ("Inherit", "inherit"),
                ("Enabled", "yes"),
                ("Disabled", "no"),
            ]
        };
        let selected = match rule.get(field).and_then(Value::as_bool) {
            Some(true) => items[1].1,
            Some(false) => items[2].1,
            None => items[0].1,
        };
        let native_field = if field == "complete_in_code_editors" {
            field.into()
        } else {
            format!("rule_{field}")
        };
        self.choice(
            &native_field,
            title,
            detail,
            items
                .iter()
                .map(|(label, value)| ((*label).into(), json!(value)))
                .collect(),
            json!(selected),
            UiAction::Rule(key.into(), field.into(), Some(Value::Null)),
            false,
        );
    }
    fn labs(&mut self, c: &Config) {
        self.section("Alternative Suggestions");
        self.toggle(c,"alternatives_enabled","Show alternative suggestions","A numbered list below the caret. Generated locally and inserted only after explicit choice.");
        self.setting_choice(
            c,
            "alternatives_mode",
            "Show alternatives",
            "Shortcut also reveals the list early in delay mode.",
            &[
                ("With shortcut", "shortcut"),
                ("After a short delay", "pause"),
            ],
        );
        self.number(
            c,
            "alternatives_delay_ms",
            "Delay before showing (milliseconds)",
            "100–10000 milliseconds.",
        );
        self.section("Synonyms");
        self.toggle(
            c,
            "synonyms_enabled",
            "Suggest synonyms for selected words",
            "A numbered choice replaces only the exact revalidated selection.",
        );
        self.setting_choice(
            c,
            "synonyms_mode",
            "Show synonyms",
            "",
            &[
                ("After selecting text", "selection"),
                ("With shortcut", "shortcut"),
            ],
        );
        self.number(
            c,
            "synonyms_delay_ms",
            "Delay before showing (milliseconds)",
            "100–10000 milliseconds.",
        );
        self.section("Typing Timing");
        self.number(
            c,
            "debounce_ms",
            "Completion delay (milliseconds)",
            "Wait after typing before automatic inference; 100–5000.",
        );
        self.number(
            c,
            "min_chars",
            "Minimum preceding characters",
            "Minimum automatic completion context; 1–1000.",
        );
        self.number(
            c,
            "context_chars",
            "Maximum local context characters",
            "Bound text passed to local model; 64–8000.",
        );
        self.note("Word-tool shortcuts are in Shortcuts. Enabling a lab never makes unsupported or private fields eligible.");
    }
    fn statistics(&mut self, c: &Config) {
        self.section("Today’s Activity");
        self.live_label("today", "inklow.statistics.today", "", 38.);
        self.toggle(
            c,
            "statistics_enabled",
            "Collect aggregate statistics",
            "Only counts successful insertions. No writing content is stored.",
        );
        self.section("History");
        for (field, title, selected, items) in [
            (
                "stat_period",
                "Time range",
                self.stat_period.clone(),
                vec![
                    ("Today", "today"),
                    ("Last 7 Days", "week"),
                    ("Last 30 Days", "month"),
                    ("Last Year", "year"),
                    ("All Time", "all"),
                ],
            ),
            (
                "stat_group",
                "Group by",
                self.stat_group.clone(),
                vec![("Day", "day"), ("Month", "month"), ("Application", "app")],
            ),
            (
                "stat_metric",
                "Metric",
                self.stat_metric.clone(),
                vec![
                    ("Completions", "completions"),
                    ("Words", "words"),
                    ("Characters", "characters"),
                    ("Time saved (minutes)", "estimated_seconds"),
                ],
            ),
        ] {
            let action = UiAction::Statistics {
                period: (field == "stat_period").then(String::new),
                group: (field == "stat_group").then(String::new),
                metric: (field == "stat_metric").then(String::new),
            };
            self.choice(
                field,
                title,
                "",
                items.iter().map(|(l, v)| ((*l).into(), json!(v))).collect(),
                json!(selected),
                action,
                false,
            );
        }
        let chart = Chart::new(&self.kit);
        self.custom(chart.root.clone(), 300.);
        self.chart = Some(chart);
        self.live_label("totals", "inklow.statistics.totals", "", 78.);
        let table = Table::new(
            &self.kit,
            "inklow.statistics.table",
            &[
                ("label", "Day / Month / App", 190.),
                ("completions", "Completions", 100.),
                ("words", "Words", 85.),
                ("characters", "Characters", 100.),
                ("minutes", "Minutes", 85.),
            ],
            false,
        );
        self.custom(table.scroll.clone(), 200.);
        self.stats_table = Some(table);
        self.action_row(
            "Clear aggregate statistics",
            "Does not delete encrypted writing history.",
            "Clear Statistics…",
            "inklow.statistics.clear",
            UiAction::ClearStatistics,
        );
    }
    fn about(&mut self, c: &Config) {
        let icon = obj("NSImageView");
        unsafe {
            let app: *mut AnyObject =
                msg_send![AnyClass::get(c"NSApplication").unwrap(), sharedApplication];
            let image: *mut AnyObject = msg_send![app, applicationIconImage];
            let _: () = msg_send![&*icon,setImage:image];
            let _: () = msg_send![&*icon,setImageScaling:3usize];
        }
        self.custom(icon, 88.);
        let wordmark = wordmark(&self.kit, 550., 44., 32.);
        self.custom(wordmark, 44.);
        self.row(&format!("Version {}",env!("CARGO_PKG_VERSION")),"Local-first predictive typing. A menu utility, not a document editor. No account, subscription or telemetry.",None);
        self.section("Software Updates");
        self.live_label(
            "updateStatus",
            "inklow.about.updateStatus",
            "No update check has run yet.",
            95.,
        );
        let progress = self
            .kit
            .progress("inklow.about.updateProgress", rect(0., 0., 550., 16.));
        self.live.insert("updateProgress".into(), progress.clone());
        self.custom(progress, 16.);
        self.action_row(
            "Software update",
            "Downloads and installation require your approval.",
            "Check for Updates…",
            "inklow.about.update.primary",
            UiAction::UpdateCheck,
        );
        self.toggle(c,"check_updates_automatically","Check automatically","Checks signed releases at launch at most once a day. Downloads and installation require approval.");
        self.note("This app is not notarized. macOS may require per-app Open Anyway approval in Privacy & Security. Approve only this app if trusted; never disable Gatekeeper. Permissions may need approval again after updating.");
        self.section("BroTypist");
        for (title, detail, button, id, url) in [
            (
                "Releases",
                "Release notes and downloadable packages.",
                "Open Releases…",
                "releases",
                format!("{REPOSITORY}/releases"),
            ),
            (
                "Support",
                "Report a problem or ask a question. Nothing is sent automatically.",
                "Open Support…",
                "support",
                format!("{REPOSITORY}/issues"),
            ),
            (
                "Source repository",
                "Public source code and releases. No account or token required.",
                "View on GitHub",
                "source",
                REPOSITORY.into(),
            ),
        ] {
            self.action_row(
                title,
                detail,
                button,
                &format!("inklow.about.{id}"),
                UiAction::Url(url),
            );
        }
        self.action_row(
            "Welcome & setup",
            "Review privacy, permissions, models and real typing preview.",
            "Show Welcome…",
            "inklow.about.welcome",
            UiAction::Welcome,
        );
        self.row("Typing safety","Secure fields, excluded apps and unsupported controls are skipped before capture. Insertion revalidates the original target.",None);
        self.row(
            "Settings links",
            "Open brotypist://settings/<pane>, for example brotypist://settings/shortcuts.",
            None,
        );
        self.action_row(
            "Quit BroTypist",
            "Stops monitoring, cancels downloads and releases the inference worker.",
            "Quit",
            "inklow.about.quit",
            UiAction::Quit,
        );
    }
    pub(super) fn refresh(
        &mut self,
        c: &Config,
        status: &Value,
        inventory: &Value,
        update: &Value,
    ) -> Result<()> {
        let data = serde_json::to_value(c)?;
        for control in &self.controls {
            let field = if control.field == "launch_at_login" {
                status
                    .get("launch_at_login")
                    .unwrap_or(&data[&control.field])
            } else {
                &data[&control.field]
            };
            unsafe {
                let editor: *mut AnyObject = msg_send![&*control.view, currentEditor];
                if !editor.is_null() {
                    continue;
                }
                match control.kind {
                    0 => {
                        let _: () = msg_send![&*control.view,setState:isize::from(field.as_bool().unwrap_or(false))];
                    }
                    1 => {
                        if let Some(index) = control.options.iter().position(|v| v == field) {
                            let _: () = msg_send![&*control.view,selectItemAtIndex:index as isize];
                        }
                    }
                    3 => {
                        let _: () =
                            msg_send![&*control.view,setDoubleValue:field.as_f64().unwrap_or(0.)];
                    }
                    _ => {
                        let value = field
                            .as_str()
                            .map(str::to_owned)
                            .unwrap_or_else(|| field.to_string());
                        if string(&control.view) != value {
                            set_string(&control.view, &value);
                        }
                    }
                }
            }
        }
        for (key, instructions) in &self.editors {
            let saved = saved_instructions(c, key);
            let draft = string(&instructions.editor);
            let dirty = draft != saved;
            enable(&instructions.save, dirty && draft.chars().count() <= 8000);
            enable(&instructions.revert, dirty);
        }
        self.config_values = data;
        for (key, button) in &self.sidebar_buttons {
            unsafe {
                let destination = if self.current_pane.starts_with("rule:") {
                    "appSettings"
                } else {
                    &self.current_pane
                };
                let selected = key == destination;
                let state: isize = msg_send![&**button, state];
                // A native push-on/push-off button can change state before its
                // navigation action arrives. The owned highlight tracks selection.
                let selection_moved = selected
                    && self.sidebar_selection.as_ref().is_some_and(|background| {
                        let previous: NSRect = msg_send![&**background, frame];
                        let current: NSRect = msg_send![&**button, frame];
                        previous != current
                    });
                if state != isize::from(selected) || selection_moved {
                    let _: () = msg_send![&**button,setState:isize::from(selected)];
                    let font: *mut AnyObject = msg_send![
                        AnyClass::get(c"NSFont").unwrap(),
                        systemFontOfSize:14.0f64,
                        weight:if selected { 0.23f64 } else { 0.0f64 }
                    ];
                    let _: () = msg_send![&**button,setFont:font];
                    if selected {
                        let bounds: NSRect = msg_send![&**button, bounds];
                        let _: bool = msg_send![&**button,scrollRectToVisible:bounds];
                    }
                }
                if selected {
                    if let Some(background) = &self.sidebar_selection {
                        let bounds: NSRect = msg_send![&**button, frame];
                        frame(background, &bounds);
                    }
                }
            }
        }
        if let Some(material) = &self.sidebar_material {
            let solid = status["reduce_transparency"].as_bool().unwrap_or(false)
                || status["increase_contrast"].as_bool().unwrap_or(false);
            hide(material, solid);
            if let Some(sidebar) = &self.sidebar {
                unsafe {
                    let _: () = msg_send![&**sidebar,setWantsLayer:true];
                    let layer: *mut AnyObject = msg_send![&**sidebar, layer];
                    let color: *mut AnyObject =
                        msg_send![AnyClass::get(c"NSColor").unwrap(), windowBackgroundColor];
                    let cg: *mut objc2_core_graphics::CGColor = if solid {
                        msg_send![color, CGColor]
                    } else {
                        std::ptr::null_mut()
                    };
                    let _: () = msg_send![layer,setBackgroundColor:cg];
                }
            }
        }
        if let Some(footer) = &self.sidebar_status {
            let active = model_entries(inventory)
                .iter()
                .find(|m| {
                    m["path"].as_str() == Some(c.model_path.as_str()) && !c.model_path.is_empty()
                })
                .and_then(|m| m["label"].as_str())
                .unwrap_or(if c.backend == "gguf" {
                    if c.model_path.is_empty() {
                        "No model selected"
                    } else {
                        std::path::Path::new(&c.model_path)
                            .file_name()
                            .and_then(|s| s.to_str())
                            .unwrap_or(&c.model_path)
                    }
                } else {
                    &c.model
                });
            let label = format!(
                "{} · {active}",
                if status["enabled"].as_bool() == Some(true) {
                    "On"
                } else {
                    "Paused"
                },
            );
            let diagnostic = status["message"].as_str().unwrap_or("");
            let help = if diagnostic.is_empty() {
                label.clone()
            } else {
                format!("{label}\n{diagnostic}")
            };
            unsafe {
                let help = NSString::from_str(&help);
                let _: () = msg_send![&**footer,setToolTip:&*help];
                let _: () = msg_send![&**footer,setAccessibilityHelp:&*help];
            }
            if string(footer) != label {
                set_string(footer, &label);
            }
        }
        self.refresh_setup(c, status, inventory);
        self.set_live("nativeStatus", text(status, "native_status"));
        self.set_live(
            "contextStatus",
            &format!(
                "Context: {}",
                status["context_message"]
                    .as_str()
                    .unwrap_or("Not requested")
            ),
        );
        let screen = permission(status, "screen_recording") == "granted";
        let wanted = c.screenshot_context || c.screenshot_appearance;
        self.set_live(
            "screenRecording",
            if screen {
                "Granted"
            } else if wanted {
                "Not granted — screenshots inactive"
            } else {
                "Not granted"
            },
        );
        if let Some(button) = self.live.get("inklow.context.screenRecording") {
            set_title(
                button,
                if screen {
                    "Open Settings…"
                } else {
                    "Request Access…"
                },
            );
            self.kit
                .rebind(button, screen_action(status), ReadValue::None);
            enable(button, screen || wanted);
        }
        let history = &status["history_info"];
        let history_text = if let Some(error) = history["error"].as_str().filter(|s| !s.is_empty())
        {
            format!("Secure storage unavailable: {error}")
        } else if history["available"].as_bool() != Some(true) {
            if history["count"].as_u64().unwrap_or(0) > 0 {
                "Encrypted history is locked; learning is off.".into()
            } else {
                "No encrypted history yet. Enable recording to learn locally.".into()
            }
        } else {
            format!(
                "{} encrypted records · {} bytes on disk",
                history["count"].as_u64().unwrap_or(0),
                history["bytes"].as_u64().unwrap_or(0)
            )
        };
        self.set_live("historyInfo", &history_text);
        let secure = status["secure_input"].as_bool().unwrap_or(false);
        self.set_live("secureInput",if secure{"Secure Input is active in another app. Global shortcuts and typing detection are paused; BroTypist never disables it."}else{"Shortcuts are local to this Mac. Click a recorder then press the actual shortcut. Escape cancels; Delete clears."});
        let recording = self.kit.queue.borrow().recording.clone();
        for (key, value) in &c.shortcuts {
            if let Some(button) = self.live.get(&format!("inklow.shortcuts.{key}")) {
                set_title(
                    button,
                    if recording.as_deref() == Some(key.as_str()) {
                        "Type shortcut…"
                    } else if value.is_empty() {
                        "Record Shortcut…"
                    } else {
                        value
                    },
                );
            }
        }
        self.refresh_models(c, status, inventory);
        self.refresh_rules(c, status);
        self.refresh_statistics(status);
        self.refresh_updates(status, update);
        if self.layout_dirty.replace(false) {
            self.reflow();
        }
        Ok(())
    }
    fn set_live(&self, key: &str, value: &str) {
        if let Some(view) = self.live.get(key) {
            if string(view) != value {
                set_string(view, value);
                self.layout_dirty.set(true);
            }
        }
    }
    fn refresh_setup(&self, c: &Config, status: &Value, inventory: &Value) {
        let mut required = 0;
        let mut optional = 0;
        for (key, importance) in [
            ("accessibility", true),
            ("input_monitoring", true),
            ("model", true),
            ("inline_prediction", false),
            ("screen_context", false),
            ("clipboard_context", false),
            ("screen_recording", false),
            ("terminal_completion", false),
            ("terminal_automation", false),
            ("iterm_automation", false),
        ] {
            let (done, label) = match key {
                "model" => {
                    let ready = model_ready(c, status, inventory);
                    (
                        ready,
                        if ready {
                            if c.backend == "gguf" {
                                "Ready".into()
                            } else {
                                "Local server".into()
                            }
                        } else {
                            let download = &status["download"];
                            if download["state"] == "downloading" {
                                let total = download["total"].as_f64().unwrap_or(0.);
                                if total > 0. {
                                    format!(
                                        "Downloading {:.0}%",
                                        download["received"].as_f64().unwrap_or(0.) / total * 100.
                                    )
                                } else {
                                    "Downloading…".into()
                                }
                            } else {
                                "Not installed".into()
                            }
                        },
                    )
                }
                "inline_prediction" => match status["inline_prediction_enabled"].as_bool() {
                    Some(true) => (false, "Enabled".into()),
                    Some(false) => (true, "Disabled".into()),
                    None => (true, "Couldn’t be detected".into()),
                },
                "screen_context" => (
                    c.screen_context,
                    if c.screen_context { "On" } else { "Off" }.into(),
                ),
                "clipboard_context" => (
                    c.clipboard_context,
                    if c.clipboard_context { "On" } else { "Off" }.into(),
                ),
                "terminal_completion" => (
                    c.terminal_completion,
                    if c.terminal_completion { "On" } else { "Off" }.into(),
                ),
                "screen_recording" => {
                    let granted = permission(status, key) == "granted";
                    let wanted = c.screenshot_context || c.screenshot_appearance;
                    (
                        granted || !wanted,
                        if granted {
                            "Granted"
                        } else if wanted {
                            "Not granted"
                        } else {
                            "Not needed"
                        }
                        .into(),
                    )
                }
                "terminal_automation" | "iterm_automation" => {
                    let value = permission(status, key);
                    (
                        !c.terminal_completion || value == "granted",
                        permission_label(&value).into(),
                    )
                }
                _ => {
                    let granted = permission(status, key) == "granted";
                    (
                        granted,
                        if granted { "Granted" } else { "Not granted" }.into(),
                    )
                }
            };
            if !done {
                if importance {
                    required += 1
                } else {
                    optional += 1
                }
            }
            self.set_live(&format!("setup.{key}.status"), &label);
            if let Some(view) = self.live.get(&format!("setup.{key}.status")) {
                unsafe {
                    let class = AnyClass::get(c"NSColor").unwrap();
                    let color: *mut AnyObject = if done
                        && (!matches!(
                            key,
                            "screen_recording" | "terminal_automation" | "iterm_automation"
                        ) || label == "Granted")
                    {
                        msg_send![class, systemGreenColor]
                    } else if importance {
                        msg_send![class, systemRedColor]
                    } else {
                        msg_send![class, secondaryLabelColor]
                    };
                    let _: () = msg_send![&**view,setTextColor:color];
                }
            }
            if let Some(button) = self.live.get(&format!("inklow.setup.{key}")) {
                let title = match key {
                    "model" => "Choose Model…",
                    "inline_prediction" => "Open Keyboard Settings…",
                    "screen_context" | "clipboard_context" => {
                        if done {
                            "Review…"
                        } else {
                            "Enable…"
                        }
                    }
                    "screen_recording" => {
                        if done {
                            "Review…"
                        } else {
                            "Request Access…"
                        }
                    }
                    "terminal_completion" => {
                        if done {
                            "Turn Off"
                        } else {
                            "Enable…"
                        }
                    }
                    "terminal_automation" | "iterm_automation" => {
                        if ["Granted", "Denied"].contains(&label.as_str()) {
                            "Open Settings…"
                        } else {
                            "Request Access…"
                        }
                    }
                    _ => "Open Settings…",
                };
                set_title(button, title);
                enable(
                    button,
                    !matches!(key, "terminal_automation" | "iterm_automation")
                        || c.terminal_completion,
                );
                let action = match key {
                    "screen_context" | "clipboard_context" => {
                        if done {
                            Some(UiAction::Navigate("context".into()))
                        } else {
                            Some(UiAction::Set(key.into(), json!(true)))
                        }
                    }
                    "terminal_completion" => {
                        Some(UiAction::Set(key.into(), json!(!c.terminal_completion)))
                    }
                    "screen_recording" => Some(if done {
                        UiAction::Navigate("context".into())
                    } else {
                        UiAction::RequestScreen
                    }),
                    "terminal_automation" | "iterm_automation" => {
                        Some(if ["Granted", "Denied"].contains(&label.as_str()) {
                            UiAction::Permission("automation".into())
                        } else {
                            UiAction::RequestAutomation(
                                if key == "terminal_automation" {
                                    "com.apple.Terminal"
                                } else {
                                    "com.googlecode.iterm2"
                                }
                                .into(),
                            )
                        })
                    }
                    _ => None,
                };
                if let Some(action) = action {
                    self.kit.rebind(button, action, ReadValue::None);
                }
            }
        }
        let summary = if required > 0 {
            format!(
                "{required} Required Step{} Left",
                if required == 1 { "" } else { "s" }
            )
        } else if optional > 0 {
            format!(
                "{optional} Optional Step{} Left",
                if optional == 1 { "" } else { "s" }
            )
        } else {
            "You’re All Set".into()
        };
        self.set_live("setupSummary", &summary);
        if let Some(badge) = &self.sidebar_badge {
            hide(badge, required == 0 && optional == 0);
        }
    }
    fn refresh_models(&self, c: &Config, status: &Value, inventory: &Value) {
        let current = if c.backend != "gguf" {
            c.model.as_str()
        } else if !c.model_path.is_empty() {
            std::path::Path::new(&c.model_path)
                .file_name()
                .and_then(|v| v.to_str())
                .unwrap_or(&c.model_path)
        } else {
            "No model selected"
        };
        self.set_live(
            "currentModel",
            &format!(
                "{current}\n{}",
                status["message"]
                    .as_str()
                    .unwrap_or("Prepares locally in supported writing fields.")
            ),
        );
        self.set_live(
            "modelFile",
            &format!(
                "Current GGUF file: {}",
                if c.model_path.is_empty() {
                    "None selected"
                } else {
                    &c.model_path
                }
            ),
        );
        let entries = model_entries(inventory);
        let selected = selected_model_id(c, self.model_selection.as_deref(), inventory);
        let model = entries
            .iter()
            .find(|m| m["id"].as_str() == Some(selected.as_str()));
        if let Some(popup) = self.live.get("inklow.models.model_preset") {
            unsafe {
                for (i, entry) in entries.iter().enumerate() {
                    let item: *mut AnyObject = msg_send![&**popup,itemAtIndex:(i+1) as isize];
                    if !item.is_null() {
                        set_title(&*item, &model_title(entry));
                    }
                }
                let index = entries
                    .iter()
                    .position(|m| m["id"].as_str() == Some(selected.as_str()))
                    .map(|i| i + 1)
                    .unwrap_or(0);
                let _: () = msg_send![&**popup,selectItemAtIndex:index as isize];
            }
        }
        let description = model
            .map(|m| {
                let mut parts = Vec::new();
                for key in ["description", "license", "availability"] {
                    if let Some(value) = m[key].as_str().filter(|s| !s.is_empty()) {
                        parts.push(value);
                    }
                }
                parts.join(" · ")
            })
            .unwrap_or_else(|| {
                "Imported local model. Use Import Model to choose a GGUF file.".into()
            });
        self.set_live("modelDescription", &description);
        let download = &status["download"];
        let active = download["state"] == "downloading";
        let shutting = status["shutdown_started"].as_bool().unwrap_or(false);
        let unavailable = model
            .and_then(|m| m["availability"].as_str())
            .is_some_and(|s| !s.is_empty());
        let installed = model.is_some_and(|m| m["installed"].as_bool() == Some(true));
        if let Some(button) = self.live.get("inklow.models.download") {
            let title = if installed {
                "Use Model"
            } else {
                "Download Model"
            };
            let old: *mut NSString = unsafe { msg_send![&**button, title] };
            if old.is_null() || unsafe { &*old }.to_string() != title {
                set_title(button, title);
                self.layout_dirty.set(true);
            }
            enable(
                button,
                model.is_some() && !active && !shutting && !unavailable,
            );
            self.kit
                .rebind(button, UiAction::ModelDownload(selected), ReadValue::None);
        }
        if let Some(button) = self.live.get("inklow.models.cancelDownload") {
            let hidden: bool = unsafe { msg_send![&**button, isHidden] };
            if hidden == active {
                hide(button, !active);
                self.layout_dirty.set(true);
            }
            enable(button, active && !shutting);
        }
        if let Some(button) = self.live.get("inklow.models.chooseModel") {
            enable(button, !active && !shutting);
        }
        if let Some(progress) = self.live.get("modelProgress") {
            progress_value(progress, download, active);
            let hidden: bool = unsafe { msg_send![&**progress, isHidden] };
            if hidden == active {
                hide(progress, !active);
                self.layout_dirty.set(true);
            }
        }
        let done = download["received"]
            .as_f64()
            .or_else(|| download["downloaded"].as_f64())
            .unwrap_or(0.);
        let total = download["total"].as_f64().unwrap_or(0.);
        let message = if active && total > 0. {
            format!("Downloading: {:.1} / {:.1} MB", done / 1e6, total / 1e6)
        } else if download["state"] == "complete" {
            if download["path"].as_str() == Some(c.model_path.as_str()) {
                "Download verified. This model is in use.".into()
            } else {
                "Download verified. Select it and press Use Model to activate it.".into()
            }
        } else {
            download["error"]
                .as_str()
                .or_else(|| download["message"].as_str())
                .filter(|s| !s.is_empty())
                .unwrap_or("No download in progress. Installed models are ready when selected.")
                .into()
        };
        self.set_live("downloadStatus", &message);
    }
    fn refresh_rules(&self, c: &Config, status: &Value) {
        if let Some(table) = &self.rule_table {
            let query = self.rule_query.trim().to_lowercase();
            let mut keys = Vec::new();
            let mut rows = Vec::new();
            for (key, rule) in &c.app_rules {
                let summary = rule_summary(rule);
                if !format!("{key} {summary}").to_lowercase().contains(&query) {
                    continue;
                }
                keys.push(key.clone());
                rows.push(BTreeMap::from([
                    (
                        "name".into(),
                        key.split_once(':').map(|(_, id)| id).unwrap_or(key).into(),
                    ),
                    ("summary".into(), summary),
                ]));
            }
            table.set(rows, keys, &self.selected_rule);
        }
        if let Some(count) = scope_count(status, &self.selected_rule) {
            self.set_live("ruleCount", &format!("Recorded texts: {count}"));
        }
    }
    fn refresh_statistics(&mut self, status: &Value) {
        if self.stats_table.is_none() {
            return;
        }
        let stats = &status["statistics"];
        if self.statistics_snapshot == *stats && self.statistics_metric == self.stat_metric {
            return;
        }
        self.statistics_snapshot = stats.clone();
        self.statistics_metric = self.stat_metric.clone();
        let rows = stats["rows"].as_array().map(Vec::as_slice).unwrap_or(&[]);
        let mut table_rows = Vec::with_capacity(rows.len());
        for row in rows {
            let mut record = BTreeMap::new();
            for key in ["label", "completions", "words", "characters"] {
                record.insert(
                    key.into(),
                    row[key]
                        .as_str()
                        .map(str::to_owned)
                        .unwrap_or_else(|| row[key].as_u64().unwrap_or(0).to_string()),
                );
            }
            record.insert(
                "minutes".into(),
                minutes(row["estimated_seconds"].as_f64().unwrap_or(0.)),
            );
            table_rows.push(record);
        }
        if let Some(table) = &self.stats_table {
            table.set(table_rows, Vec::new(), "");
        }
        if let Some(chart) = &mut self.chart {
            chart.set(&self.kit, rows, &self.stat_metric);
        }
        let today = &stats["today"];
        self.set_live(
            "today",
            &format!(
                "{} completions · {} words · {} characters",
                today["completions"].as_u64().unwrap_or(0),
                today["words"].as_u64().unwrap_or(0),
                today["characters"].as_u64().unwrap_or(0)
            ),
        );
        let scale = if self.stat_metric == "estimated_seconds" {
            60.
        } else {
            1.
        };
        let total = if self.stat_metric == "estimated_seconds" {
            stats["estimated_seconds"].as_f64().unwrap_or(0.)
        } else {
            stats["totals"][&self.stat_metric].as_f64().unwrap_or(0.)
        } / scale;
        let average = stats["daily_average"][&self.stat_metric]
            .as_f64()
            .unwrap_or(0.)
            / scale;
        self.set_live("totals",&format!("Total: {total:.1} · Daily average ({} active days): {average:.1}\nEstimated time saved: ≈{} minutes (from accepted characters).",stats["active_days"].as_u64().unwrap_or(0),minutes(stats["estimated_seconds"].as_f64().unwrap_or(0.))));
    }
    fn refresh_updates(&self, status: &Value, update: &Value) {
        if !self.live.contains_key("updateStatus") {
            return;
        }
        let state = update["state"].as_str().unwrap_or("idle");
        let termination = status["termination_requested"].as_bool().unwrap_or(false);
        let handoff = status["update_handoff"].as_bool().unwrap_or(false);
        let committed = status["update_committed"].as_bool().unwrap_or(false);
        let heading = format!(
            "Installed: {}{}",
            update["current_version"]
                .as_str()
                .unwrap_or(env!("CARGO_PKG_VERSION")),
            update["version"]
                .as_str()
                .filter(|v| !v.is_empty())
                .map(|v| format!(" · Release: {v}"))
                .unwrap_or_default()
        );
        let message = if termination {
            if handoff {
                "Finishing local work before installing…"
            } else {
                "Finishing local work before quitting…"
            }
        } else {
            status["shutdown_error"]
                .as_str()
                .filter(|s| !s.is_empty())
                .unwrap_or_else(|| {
                    update["message"]
                        .as_str()
                        .unwrap_or("No update check has run yet.")
                })
        };
        let reason = if update["installable"].as_bool() == Some(true) {
            ""
        } else {
            update["install_reason"].as_str().unwrap_or("")
        };
        let received = update["received"].as_f64().unwrap_or(0.);
        let total = update["total"].as_f64().unwrap_or(0.);
        let transfer = if state == "downloading" {
            if total > 0. {
                format!("\n{:.1} of {:.1} MB", received / 1e6, total / 1e6)
            } else {
                format!("\n{:.1} MB received", received / 1e6)
            }
        } else {
            String::new()
        };
        self.set_live(
            "updateStatus",
            &format!("{heading}\n{message}{transfer}\n{reason}"),
        );
        let busy = ["checking", "downloading", "installing"].contains(&state);
        if let Some(progress) = self.live.get("updateProgress") {
            progress_value(progress, update, busy);
            let indeterminate = state == "checking"
                || state == "installing"
                || (state == "downloading" && total <= 0.);
            unsafe {
                let _: () = msg_send![&**progress,setIndeterminate:indeterminate];
                if indeterminate {
                    let _: () = msg_send![&**progress,startAnimation:std::ptr::null::<AnyObject>()];
                } else {
                    let _: () = msg_send![&**progress,stopAnimation:std::ptr::null::<AnyObject>()];
                }
            }
            hide(progress, !(busy || (termination && handoff)));
        }
        if let Some(button) = self.live.get("inklow.about.update.primary") {
            let plan = update_plan(update, handoff);
            set_title(button, &plan.0);
            self.kit.rebind(button, plan.1, ReadValue::None);
            hide(button, committed || (termination && !handoff));
            enable(button, !committed && (!termination || handoff));
        }
    }
}
fn wordmark(kit: &Kit, width: f64, height: f64, size: f64) -> Obj {
    let root = kit.view(rect(0., 0., width, height));
    let bro = kit.label("Bro", rect(0., 0., width, height), size, true);
    let typist = kit.label("Typist", rect(0., 0., width, height), size, true);
    unsafe {
        let _: () = msg_send![&*bro, sizeToFit];
        let _: () = msg_send![&*typist, sizeToFit];
        let b: NSRect = msg_send![&*bro, frame];
        let t: NSRect = msg_send![&*typist, frame];
        let x = ((width - b.size.width - t.size.width) / 2.).max(0.);
        frame(&bro, &rect(x, 0., b.size.width, height));
        frame(&typist, &rect(x + b.size.width, 0., t.size.width, height));
        let color: *mut AnyObject =
            msg_send![AnyClass::get(c"NSColor").unwrap(), secondaryLabelColor];
        let _: () = msg_send![&*typist,setTextColor:color];
        let _: () = msg_send![&*bro,setAutoresizingMask:5usize];
        let _: () = msg_send![&*typist,setAutoresizingMask:5usize];
    }
    add(&root, &bro);
    add(&root, &typist);
    identify(&root, "", "BroTypist");
    root
}
fn sidebar_symbol(button: &AnyObject, pane: &str) {
    let (symbol, r, g, b) = match pane {
        "setup" => ("checklist", 255., 149., 0.),
        "general" => ("gearshape", 142., 142., 147.),
        "models" => ("cpu", 0., 122., 255.),
        "context" => ("text.viewfinder", 0., 122., 255.),
        "personalization" => ("person.text.rectangle", 175., 82., 222.),
        "emoji" => ("face.smiling", 255., 204., 0.),
        "shortcuts" => ("command", 88., 86., 214.),
        "battery" => ("battery.75percent", 52., 199., 89.),
        "appSettings" => ("square.grid.2x2", 90., 200., 250.),
        "labs" => ("flask", 255., 45., 85.),
        "statistics" => ("chart.bar.xaxis", 31., 169., 122.),
        "support" => ("questionmark.bubble", 0., 122., 255.),
        _ => ("info.circle", 142., 142., 147.),
    };
    unsafe {
        let image_class = AnyClass::get(c"NSImage").unwrap();
        let symbol: *mut AnyObject = msg_send![image_class,imageWithSystemSymbolName:&*NSString::from_str(symbol),accessibilityDescription:std::ptr::null::<NSString>()];
        if symbol.is_null() {
            return;
        }
        let color_class = AnyClass::get(c"NSColor").unwrap();
        let white: *mut AnyObject = msg_send![color_class, whiteColor];
        let colors = obj("NSMutableArray");
        let _: () = msg_send![&*colors,addObject:white];
        let config: *mut AnyObject = msg_send![AnyClass::get(c"NSImageSymbolConfiguration").unwrap(),configurationWithPaletteColors:&*colors];
        let symbol: *mut AnyObject = msg_send![symbol,imageWithSymbolConfiguration:config];
        let raw: *mut AnyObject = msg_send![image_class, alloc];
        // The 24pt tile lives inside a transparent row-height canvas: 8pt on each
        // horizontal edge keeps the native cell's image away from the selection edge
        // and its title, without replacing NSButton's title, hit target or AXPress.
        let raw: *mut AnyObject = msg_send![raw,initWithSize:NSSize::new(40.,36.)];
        let tile = Retained::from_raw(raw).expect("sidebar icon");
        let _: () = msg_send![&*tile, lockFocus];
        let color: *mut AnyObject =
            msg_send![color_class,colorWithSRGBRed:r/255.,green:g/255.,blue:b/255.,alpha:1.0f64];
        let _: () = msg_send![color, setFill];
        let path: *mut AnyObject = msg_send![AnyClass::get(c"NSBezierPath").unwrap(),bezierPathWithRoundedRect:rect(8.,6.,24.,24.),xRadius:6.0f64,yRadius:6.0f64];
        let _: () = msg_send![path, fill];
        let _: () = msg_send![symbol,drawInRect:rect(12.,10.,16.,16.),fromRect:rect(0.,0.,0.,0.),operation:2usize,fraction:1.0f64,respectFlipped:false,hints:std::ptr::null::<AnyObject>()];
        let _: () = msg_send![&*tile, unlockFocus];
        let _: () = msg_send![&*tile,setTemplate:false];
        let _: () = msg_send![button,setImage:&*tile];
        let _: () = msg_send![button,setImagePosition:2usize];
        let _: () = msg_send![button,setImageScaling:2usize];
        let font: *mut AnyObject =
            msg_send![AnyClass::get(c"NSFont").unwrap(),systemFontOfSize:14.0f64];
        let _: () = msg_send![button,setFont:font];
    }
}
/// Measure the actual retained image rather than repeating its drawing constants.
#[cfg(feature = "native-smoke")]
fn sidebar_image_ink(image: &AnyObject) -> Result<(NSSize, NSRect, NSRect, NSSize)> {
    unsafe {
        let canvas: NSSize = msg_send![image, size];
        let data: *mut AnyObject = msg_send![image, TIFFRepresentation];
        anyhow::ensure!(!data.is_null(), "Sidebar image has no TIFF representation");
        let bitmap: *mut AnyObject =
            msg_send![AnyClass::get(c"NSBitmapImageRep").unwrap(),imageRepWithData:data];
        anyhow::ensure!(
            !bitmap.is_null(),
            "Sidebar image has no bitmap representation"
        );
        let width: isize = msg_send![bitmap, pixelsWide];
        let height: isize = msg_send![bitmap, pixelsHigh];
        anyhow::ensure!(
            width > 0 && height > 0 && canvas.width > 0. && canvas.height > 0.,
            "Sidebar image has empty dimensions"
        );
        let mut tile = (width, height, -1isize, -1isize);
        let mut glyph = tile;
        for y in 0..height {
            for x in 0..width {
                let color: *mut AnyObject = msg_send![bitmap,colorAtX:x,y:y];
                if color.is_null() {
                    continue;
                }
                let alpha: f64 = msg_send![color, alphaComponent];
                if alpha <= 0.05 {
                    continue;
                }
                tile = (tile.0.min(x), tile.1.min(y), tile.2.max(x), tile.3.max(y));
                let red: f64 = msg_send![color, redComponent];
                let green: f64 = msg_send![color, greenComponent];
                let blue: f64 = msg_send![color, blueComponent];
                if red > 0.9 && green > 0.9 && blue > 0.9 {
                    glyph = (
                        glyph.0.min(x),
                        glyph.1.min(y),
                        glyph.2.max(x),
                        glyph.3.max(y),
                    );
                }
            }
        }
        anyhow::ensure!(
            tile.2 >= tile.0 && glyph.2 >= glyph.0,
            "Sidebar image is missing its tile or white glyph"
        );
        let measured = |bounds: (isize, isize, isize, isize)| {
            rect(
                bounds.0 as f64 * canvas.width / width as f64,
                bounds.1 as f64 * canvas.height / height as f64,
                (bounds.2 - bounds.0 + 1) as f64 * canvas.width / width as f64,
                (bounds.3 - bounds.1 + 1) as f64 * canvas.height / height as f64,
            )
        };
        Ok((
            canvas,
            measured(tile),
            measured(glyph),
            NSSize::new(width as f64, height as f64),
        ))
    }
}
fn text<'a>(value: &'a Value, key: &str) -> &'a str {
    value[key].as_str().unwrap_or("")
}
fn gb(bytes: f64) -> String {
    format!("{:.1} GB", bytes / 1e9)
}
fn minutes(seconds: f64) -> String {
    let value = seconds / 60.;
    if value > 0. && value < 10. {
        format!("{value:.1}")
    } else {
        format!("{value:.0}")
    }
}
fn set_title(control: &AnyObject, title: &str) {
    unsafe {
        let value = NSString::from_str(title);
        let _: () = msg_send![control,setTitle:&*value];
        let supports: bool = msg_send![control,respondsToSelector:sel!(setAccessibilityLabel:)];
        if supports {
            let _: () = msg_send![control,setAccessibilityLabel:&*value];
        }
    }
}
fn enable(control: &AnyObject, value: bool) {
    unsafe {
        let _: () = msg_send![control,setEnabled:value];
    }
}
fn hide(control: &AnyObject, value: bool) {
    unsafe {
        let _: () = msg_send![control,setHidden:value];
    }
}
fn permission(status: &Value, key: &str) -> String {
    match &status["permissions"][key] {
        Value::Bool(true) => "granted".into(),
        Value::Bool(false) => "denied".into(),
        Value::String(s) => s.clone(),
        _ => "unavailable".into(),
    }
}
fn permission_label(value: &str) -> &str {
    match value {
        "granted" => "Granted",
        "denied" => "Denied",
        "not-determined" => "Not requested",
        "not-running" => "Not running",
        _ => "Unavailable",
    }
}
fn screen_action(status: &Value) -> UiAction {
    if permission(status, "screen_recording") == "granted" {
        UiAction::Permission("screen_recording".into())
    } else {
        UiAction::RequestScreen
    }
}
fn model_entries(inventory: &Value) -> &[Value] {
    inventory
        .as_array()
        .or_else(|| inventory["models"].as_array())
        .map(Vec::as_slice)
        .unwrap_or(&[])
}
fn model_title(model: &Value) -> String {
    format!(
        "{} {} ({})",
        if model["installed"].as_bool() == Some(true) {
            "✓"
        } else {
            "↓"
        },
        text(model, "label"),
        gb(model["size_bytes"].as_f64().unwrap_or(0.))
    )
}
fn selected_model_id(c: &Config, pending: Option<&str>, inventory: &Value) -> String {
    if let Some(pending) = pending {
        return pending.into();
    }
    model_entries(inventory)
        .iter()
        .find(|m| !c.model_path.is_empty() && m["path"].as_str() == Some(c.model_path.as_str()))
        .and_then(|m| m["id"].as_str())
        .unwrap_or(if c.model_path.is_empty() {
            &c.model_id
        } else {
            ""
        })
        .into()
}
fn model_ready(c: &Config, status: &Value, inventory: &Value) -> bool {
    c.backend != "gguf"
        || status["model_ready"].as_bool().unwrap_or_else(|| {
            !c.model_path.is_empty()
                && model_entries(inventory).iter().any(|m| {
                    m["path"].as_str() == Some(c.model_path.as_str())
                        && m["installed"].as_bool() == Some(true)
                })
        })
}
fn saved_instructions<'a>(c: &'a Config, key: &str) -> &'a str {
    if key == "global" {
        &c.custom_instructions
    } else {
        key.strip_prefix("rule:")
            .and_then(|key| c.app_rules.get(key))
            .and_then(|rule| rule.get("custom_instructions"))
            .and_then(Value::as_str)
            .unwrap_or("")
    }
}
fn scope_count(status: &Value, key: &str) -> Option<u64> {
    let (kind, key) = key.split_once(':')?;
    status["history_counts"][if kind == "app" { "apps" } else { "domains" }][key].as_u64()
}
fn rule_summary(rule: &BTreeMap<String, Value>) -> String {
    let mut labels = Vec::new();
    for (field, on, off) in [
        ("enabled", "Completions on", "Completions off"),
        ("tab_enabled", "Tab on", "Tab off"),
        ("record_history", "Recording on", "Recording off"),
        (
            "improve_compatibility",
            "Compatibility on",
            "Compatibility off",
        ),
        (
            "complete_in_code_editors",
            "Code editing on",
            "Code editing off",
        ),
    ] {
        if let Some(value) = rule.get(field).and_then(Value::as_bool) {
            labels.push(if value { on } else { off });
        }
    }
    if rule
        .get("custom_instructions")
        .and_then(Value::as_str)
        .is_some_and(|s| !s.is_empty())
    {
        labels.push("Custom instructions");
    }
    if labels.is_empty() {
        "Uses defaults".into()
    } else {
        labels.join(" · ")
    }
}
fn progress_value(control: &AnyObject, snapshot: &Value, active: bool) {
    let done = snapshot["received"]
        .as_f64()
        .or_else(|| snapshot["downloaded"].as_f64())
        .unwrap_or(0.)
        .max(0.);
    let total = snapshot["total"].as_f64().unwrap_or(0.).max(0.);
    unsafe {
        let _: () = msg_send![control,setMaxValue:total.max(1.)];
        let _: () = msg_send![control,setDoubleValue:done.min(total.max(1.))];
        let indeterminate = active && total <= 0.;
        let _: () = msg_send![control,setIndeterminate:indeterminate];
        if indeterminate {
            let _: () = msg_send![control,startAnimation:std::ptr::null::<AnyObject>()];
        } else {
            let _: () = msg_send![control,stopAnimation:std::ptr::null::<AnyObject>()];
        }
    }
}
fn update_plan(update: &Value, handoff: bool) -> (String, UiAction) {
    if handoff {
        return ("Cancel Installation".into(), UiAction::UpdateCancel);
    }
    let state = text(update, "state");
    if ["available", "ready"].contains(&state) && update["installable"].as_bool() != Some(true) {
        return (
            "Open Releases…".into(),
            UiAction::Url(
                update["release_url"]
                    .as_str()
                    .filter(|s| !s.is_empty())
                    .map(str::to_owned)
                    .unwrap_or_else(|| format!("{REPOSITORY}/releases")),
            ),
        );
    }
    match state {
        "checking" => ("Cancel Check".into(), UiAction::UpdateCancel),
        "downloading" => ("Cancel Download".into(), UiAction::UpdateCancel),
        "installing" => ("Cancel Installation".into(), UiAction::UpdateCancel),
        "available" => ("Download Update".into(), UiAction::UpdateDownload),
        "ready" => ("Install & Relaunch".into(), UiAction::UpdateInstall),
        "error" => ("Try Again…".into(), UiAction::UpdateCheck),
        _ => ("Check for Updates…".into(), UiAction::UpdateCheck),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn pending_model_does_not_follow_active_model_readiness() {
        let c = Config {
            model_path: "/models/installed.gguf".into(),
            model_id: "installed".into(),
            ..Config::default()
        };
        let inventory = json!([{"id":"installed","path":"/models/installed.gguf","installed":true},{"id":"pending","path":"/models/pending.gguf","installed":false}]);
        assert_eq!(
            selected_model_id(&c, Some("pending"), &inventory),
            "pending"
        );
        assert_eq!(selected_model_id(&c, None, &inventory), "installed");
        assert!(model_ready(&c, &json!({}), &inventory));
    }
    #[test]
    fn imported_model_is_not_mistaken_for_configured_catalogue_id() {
        let c = Config {
            model_path: "/custom.gguf".into(),
            model_id: "tiny".into(),
            ..Config::default()
        };
        assert_eq!(selected_model_id(&c, None, &json!([])), "");
    }
    #[test]
    fn update_intent_matches_the_rendered_button() {
        let ready = json!({"state":"ready","installable":true});
        let (title, action) = update_plan(&ready, false);
        assert_eq!(title, "Install & Relaunch");
        assert!(matches!(action, UiAction::UpdateInstall));
        let (title, action) = update_plan(
            &json!({"state":"ready","installable":false,"release_url":"https://example.com/releases"}),
            false,
        );
        assert_eq!(title, "Open Releases…");
        assert!(matches!(action,UiAction::Url(url)if url=="https://example.com/releases"));
        assert!(matches!(
            update_plan(&ready, true).1,
            UiAction::UpdateCancel
        ));
    }
    #[test]
    fn instruction_baselines_keep_scopes_distinct() {
        let c = Config {
            custom_instructions: "global".into(),
            app_rules: BTreeMap::from([(
                "app:editor".into(),
                BTreeMap::from([("custom_instructions".into(), json!("editor"))]),
            )]),
            ..Config::default()
        };
        assert_eq!(saved_instructions(&c, "global"), "global");
        assert_eq!(saved_instructions(&c, "rule:app:editor"), "editor");
        assert_eq!(saved_instructions(&c, "rule:domain:example.com"), "");
    }
}
