//! Retained native layout, aggregate tables, and drag-and-drop rule lists.
use super::*;
use objc2::{DefinedClass, MainThreadOnly, define_class};
use objc2_foundation::{MainThreadMarker, NSNotification, NSObject, NSObjectProtocol, NSString};
use std::cell::{Cell, RefCell};
use std::rc::Rc;

pub(super) const CONTENT_INSET: f64 = 38.;
const GROUP_MARGIN: f64 = 24.;
const ROW_INSET: f64 = CONTENT_INSET - GROUP_MARGIN;
const CONTROL_COLUMN: f64 = 220.;
const CONTROL_GAP: f64 = 32.;
const TEXT_MAX_WIDTH: f64 = 420.;
const TEXT_MIN_WIDTH: f64 = 240.;
const ROW_PADDING: f64 = 16.;
const ROW_MIN_HEIGHT: f64 = 60.;
const TITLE_DETAIL_GAP: f64 = 8.;
const STACKED_CONTROL_GAP: f64 = 12.;
const HEADING_GROUP_GAP: f64 = 12.;
const SECTION_GAP: f64 = 32.;

struct Row {
    title: Obj,
    detail: Option<Obj>,
    control: Option<Obj>,
    control_size: NSSize,
    divider: Option<Obj>,
}
struct Group {
    heading: Option<Obj>,
    box_view: Obj,
    content: Obj,
    rows: Vec<Row>,
}
enum Block {
    Group(Group),
    Note(Obj),
    Custom { view: Obj, height: f64 },
    Actions { view: Obj, actions: Vec<Obj> },
}
pub(super) struct Form {
    pub root: Obj,
    blocks: Vec<Block>,
    pub width: f64,
}
impl Form {
    pub fn new(kit: &Kit) -> Self {
        Self {
            root: kit.view(rect(0., 0., 690., 588.)),
            blocks: Vec::new(),
            width: 690.,
        }
    }
    pub fn custom(&mut self, view: Obj, height: f64) {
        add(&self.root, &view);
        self.blocks.push(Block::Custom { view, height });
    }
    pub fn section(&mut self, kit: &Kit, title: &str) {
        let heading = if title.is_empty() {
            None
        } else {
            let heading = kit.label(&title.to_uppercase(), rect(0., 0., 550., 18.), 11.5, true);
            secondary(&heading);
            add(&self.root, &heading);
            Some(heading)
        };
        let box_view = obj("NSBox");
        let content = kit.view(rect(0., 0., 550., 0.));
        unsafe {
            let _: () = msg_send![&*box_view,setBoxType:4usize];
            let _: () = msg_send![&*box_view,setBorderType:0usize];
            let _: () = msg_send![&*box_view,setTitlePosition:0usize];
            let _: () = msg_send![&*box_view,setTransparent:false];
            let color: *mut AnyObject =
                msg_send![AnyClass::get(c"NSColor").unwrap(), controlBackgroundColor];
            let _: () = msg_send![&*box_view,setFillColor:color];
            let _: () = msg_send![&*box_view,setCornerRadius:10.0f64];
            let _: () = msg_send![&*box_view,setContentViewMargins:NSSize::new(0.,0.)];
            let _: () = msg_send![&*box_view,setContentView:&*content];
        }
        add(&self.root, &box_view);
        self.blocks.push(Block::Group(Group {
            heading,
            box_view,
            content,
            rows: Vec::new(),
        }));
    }
    pub fn note(&mut self, kit: &Kit, text: &str) {
        let view = kit.label(text, rect(0., 0., 550., 40.), 11.5, false);
        secondary(&view);
        add(&self.root, &view);
        self.blocks.push(Block::Note(view));
    }
    pub fn actions(&mut self, kit: &Kit, actions: Vec<Obj>) {
        let view = kit.view(rect(0., 0., 550., 28.));
        for action in &actions {
            add(&view, action);
        }
        add(&self.root, &view);
        self.blocks.push(Block::Actions { view, actions });
    }
    pub fn row(&mut self, kit: &Kit, title: &str, detail: &str, control: Option<Obj>) {
        if !matches!(self.blocks.last(), Some(Block::Group(_))) {
            self.section(kit, "");
        }
        let Some(Block::Group(group)) = self.blocks.last_mut() else {
            unreachable!("a form row always belongs to a group");
        };
        let label = kit.label(title, rect(0., 0., 300., 18.), 13., false);
        add(&group.content, &label);
        let detail = if detail.is_empty() {
            None
        } else {
            let label = kit.label(detail, rect(0., 0., 500., 18.), 11.5, false);
            secondary(&label);
            add(&group.content, &label);
            Some(label)
        };
        if let Some(control) = &control {
            add(&group.content, control);
        }
        let control_size = control
            .as_ref()
            .map(|control| unsafe {
                let bounds: NSRect = msg_send![&**control, frame];
                NSSize::new(
                    if bounds.size.width > 0. && bounds.size.width < 400. {
                        bounds.size.width
                    } else {
                        CONTROL_COLUMN
                    },
                    bounds.size.height.max(1.),
                )
            })
            .unwrap_or(NSSize::new(CONTROL_COLUMN, 28.));
        let divider = if group.rows.is_empty() {
            None
        } else {
            let divider = obj("NSBox");
            unsafe {
                let _: () = msg_send![&*divider,setBoxType:2usize];
            }
            add(&group.content, &divider);
            Some(divider)
        };
        group.rows.push(Row {
            title: label,
            detail,
            control,
            control_size,
            divider,
        });
    }
    pub fn reflow(&mut self, width: f64, viewport: f64) {
        self.width = width;
        let inner = (width - 2. * GROUP_MARGIN).max(1.);
        let text_width = (width - 2. * CONTENT_INSET).max(1.);
        let mut y = 24.;
        for block in &mut self.blocks {
            match block {
                Block::Group(group) => {
                    if let Some(heading) = &group.heading {
                        let height = text_height(heading, text_width);
                        frame(heading, &rect(CONTENT_INSET, y, text_width, height));
                        y += height + HEADING_GROUP_GAP;
                    }
                    // Reserve one column per group, including rows without a control.
                    let control_column = group
                        .rows
                        .iter()
                        .filter(|row| row.control.is_some())
                        .map(|row| row.control_size.width)
                        .fold(CONTROL_COLUMN, f64::max);
                    let stacked = text_width - control_column - CONTROL_GAP < TEXT_MIN_WIDTH;
                    let column_width = if stacked {
                        text_width
                    } else {
                        text_width - control_column - CONTROL_GAP
                    }
                    .min(TEXT_MAX_WIDTH);
                    let mut row_y = 0.;
                    for row in &group.rows {
                        let control_width = row.control_size.width.min(text_width);
                        let title_height = text_height(&row.title, column_width);
                        let detail_height = row
                            .detail
                            .as_ref()
                            .map(|detail| text_height(detail, column_width));
                        let text_block_height = title_height
                            + detail_height
                                .map(|height| TITLE_DETAIL_GAP + height)
                                .unwrap_or(0.);
                        let control_height = row.control.as_ref().map(|control| unsafe {
                            let label: bool = msg_send![&**control,isKindOfClass:AnyClass::get(c"NSTextField").unwrap()];
                            let editable: bool = if label {
                                msg_send![&**control, isEditable]
                            } else {
                                false
                            };
                            if label && !editable {
                                text_height(control, control_width)
                            } else {
                                let intrinsic: NSSize = msg_send![&**control, intrinsicContentSize];
                                if intrinsic.height > 0. {
                                    intrinsic.height
                                } else {
                                    row.control_size.height
                                }
                            }
                        });
                        let content_height = if stacked && row.control.is_some() {
                            text_block_height + STACKED_CONTROL_GAP + control_height.unwrap()
                        } else {
                            text_block_height.max(control_height.unwrap_or(0.))
                        };
                        let row_height = (content_height + 2. * ROW_PADDING).max(ROW_MIN_HEIGHT);
                        let content_top = row_y + (row_height - content_height) / 2.;
                        let text_top = if stacked {
                            content_top
                        } else {
                            content_top + (content_height - text_block_height) / 2.
                        };
                        frame(
                            &row.title,
                            &rect(ROW_INSET, text_top, column_width, title_height),
                        );
                        if let Some(detail) = &row.detail {
                            frame(
                                detail,
                                &rect(
                                    ROW_INSET,
                                    text_top + title_height + TITLE_DETAIL_GAP,
                                    column_width,
                                    detail_height.unwrap(),
                                ),
                            );
                        }
                        if let Some(control) = &row.control {
                            let height = control_height.unwrap();
                            let top = if stacked {
                                text_top + text_block_height + STACKED_CONTROL_GAP
                            } else {
                                content_top + (content_height - height) / 2.
                            };
                            // Align native content, not frames that include shadow/bezel outsets.
                            let alignment = rect(
                                inner - ROW_INSET - control_width,
                                top,
                                control_width,
                                height,
                            );
                            let bounds: NSRect =
                                unsafe { msg_send![&**control,frameForAlignmentRect:alignment] };
                            frame(control, &bounds);
                        }
                        if let Some(divider) = &row.divider {
                            frame(divider, &rect(ROW_INSET, row_y, text_width, 1.));
                        }
                        row_y += row_height;
                    }
                    frame(&group.box_view, &rect(GROUP_MARGIN, y, inner, row_y));
                    frame(&group.content, &rect(0., 0., inner, row_y));
                    if row_y > 0. {
                        y += row_y + SECTION_GAP;
                    }
                }
                Block::Note(view) => {
                    let height = text_height(view, text_width);
                    frame(view, &rect(CONTENT_INSET, y, text_width, height));
                    y += height + SECTION_GAP;
                }
                Block::Actions { view, actions } => {
                    let mut x = 0.;
                    let mut row_y = 0.;
                    for action in actions {
                        unsafe {
                            let hidden: bool = msg_send![&**action, isHidden];
                            if hidden {
                                continue;
                            }
                            let _: () = msg_send![&**action, sizeToFit];
                            let intrinsic: NSSize = msg_send![&**action, intrinsicContentSize];
                            let action_width = (intrinsic.width + 18.).max(100.).min(text_width);
                            if x > 0. && x + action_width > text_width {
                                x = 0.;
                                row_y += 36.;
                            }
                            let height = intrinsic.height.max(1.);
                            let alignment =
                                rect(x, row_y + (28. - height) / 2., action_width, height);
                            let bounds: NSRect =
                                msg_send![&**action,frameForAlignmentRect:alignment];
                            frame(action, &bounds);
                            x += action_width + 8.;
                        }
                    }
                    let height = row_y + 28.;
                    frame(view, &rect(CONTENT_INSET, y, text_width, height));
                    y += height + SECTION_GAP;
                }
                Block::Custom { view, height } => {
                    let hidden: bool = unsafe { msg_send![&**view, isHidden] };
                    if hidden {
                        continue;
                    }
                    let label: bool = unsafe {
                        msg_send![&**view,respondsToSelector:sel!(attributedStringValue)]
                    };
                    let height = if label {
                        (*height).max(text_height(view, text_width))
                    } else {
                        *height
                    };
                    frame(view, &rect(CONTENT_INSET, y, text_width, height));
                    fit_scroll_document(view);
                    y += height + SECTION_GAP;
                }
            }
        }
        frame(&self.root, &rect(0., 0., width, y.max(viewport)));
    }
    #[cfg(feature = "native-smoke")]
    pub(super) fn alignment_report(&self) -> anyhow::Result<Value> {
        use anyhow::{Context, ensure};
        let root_bounds: NSRect = unsafe { msg_send![&*self.root, bounds] };
        let leading = CONTENT_INSET;
        let trailing = root_bounds.size.width - CONTENT_INSET;
        let native_rect = |view: &AnyObject, aligned: bool| -> anyhow::Result<NSRect> {
            unsafe {
                let parent: *mut AnyObject = msg_send![view, superview];
                ensure!(!parent.is_null(), "Layout element lost its native parent");
                let bounds: NSRect = msg_send![view, frame];
                let bounds: NSRect = if aligned {
                    msg_send![view,alignmentRectForFrame:bounds]
                } else {
                    bounds
                };
                let converted: NSRect = msg_send![parent,convertRect:bounds,toView:&*self.root];
                Ok(converted)
            }
        };
        let packed = |bounds: NSRect| {
            [
                bounds.origin.x,
                bounds.origin.y,
                bounds.size.width,
                bounds.size.height,
            ]
        };
        let mut rows = Vec::new();
        let mut sections = Vec::new();
        let mut block_gaps = Vec::new();
        let mut previous_bottom: Option<f64> = None;
        let mut next_block_gap = SECTION_GAP;
        for block in &self.blocks {
            if let Block::Group(group) = block {
                let group_bounds = native_rect(&group.content, false)?;
                let heading = group
                    .heading
                    .as_ref()
                    .map(|view| native_rect(view, false))
                    .transpose()?;
                let heading_gap = heading
                    .map(|bounds| group_bounds.origin.y - bounds.origin.y - bounds.size.height);
                if let Some(bounds) = heading {
                    ensure!(
                        (bounds.origin.x - leading).abs() <= 0.5,
                        "Section heading escaped the text column"
                    );
                    ensure!(
                        (heading_gap.unwrap() - HEADING_GROUP_GAP).abs() <= 0.5,
                        "Section heading is too close to its group"
                    );
                }
                let section_top = heading.map_or(group_bounds.origin.y, |bounds| bounds.origin.y);
                let section_gap = previous_bottom.map(|bottom| section_top - bottom);
                if let Some(gap) = section_gap {
                    ensure!(
                        (gap - next_block_gap).abs() <= 0.5,
                        "Section spacing does not match the shared rhythm"
                    );
                    block_gaps.push(gap);
                }
                let available_width = (trailing - leading).max(1.);
                let control_column = group
                    .rows
                    .iter()
                    .filter(|row| row.control.is_some())
                    .map(|row| row.control_size.width)
                    .fold(CONTROL_COLUMN, f64::max);
                let stacked = available_width - control_column - CONTROL_GAP < TEXT_MIN_WIDTH;
                let column_width = if stacked {
                    available_width
                } else {
                    available_width - control_column - CONTROL_GAP
                }
                .min(TEXT_MAX_WIDTH);
                sections.push(json!({
                    "heading":group.heading.as_ref().map(|view| string(view)),
                    "group_rect":packed(group_bounds),"heading_group_gap":heading_gap,
                    "previous_block_gap":section_gap,"reserved_control_column":control_column,
                    "text_column_width":column_width,"stacked":stacked
                }));
                let mut previous_content_bottom: Option<f64> = None;
                for (index, row) in group.rows.iter().enumerate() {
                    let title = native_rect(&row.title, false)?;
                    ensure!(
                        (title.origin.x - leading).abs() <= 0.5
                            && (title.size.width - column_width).abs() <= 0.5,
                        "Row title escaped the text column: {}",
                        string(&row.title)
                    );
                    let control = row
                        .control
                        .as_ref()
                        .map(|view| native_rect(view, true))
                        .transpose()?;
                    let intrinsic = row.control.as_ref().map(|view| unsafe {
                        let size: NSSize = msg_send![&**view, intrinsicContentSize];
                        [size.width, size.height]
                    });
                    let detail = row
                        .detail
                        .as_ref()
                        .map(|view| native_rect(view, false))
                        .transpose()?;
                    for label in std::iter::once(&row.title).chain(row.detail.iter()) {
                        let bounds = native_rect(label, false)?;
                        let required: NSSize = unsafe {
                            let cell: *mut AnyObject = msg_send![&**label, cell];
                            msg_send![cell,cellSizeForBounds:rect(0.,0.,bounds.size.width,10000.)]
                        };
                        ensure!(
                            bounds.size.height + 0.5 >= required.height,
                            "Wrapped setting text is clipped: {}",
                            string(label)
                        );
                    }
                    let title_bottom = title.origin.y + title.size.height;
                    let title_detail_gap = detail.map(|bounds| bounds.origin.y - title_bottom);
                    if let Some(bounds) = detail {
                        ensure!(
                            (bounds.origin.x - title.origin.x).abs() <= 0.5
                                && (bounds.size.width - title.size.width).abs() <= 0.5
                                && (title_detail_gap.unwrap() - TITLE_DETAIL_GAP).abs() <= 0.5,
                            "Description must share its title's bounded width and sit directly below it: {}",
                            string(&row.title)
                        );
                    }
                    let text_bottom =
                        detail.map_or(title_bottom, |bounds| bounds.origin.y + bounds.size.height);
                    let text_block = rect(
                        title.origin.x,
                        title.origin.y,
                        title.size.width,
                        text_bottom - title.origin.y,
                    );
                    let mut centered = None;
                    let mut horizontal_gap = None;
                    let mut stacked_gap = None;
                    let mut content_top = title.origin.y;
                    let mut content_bottom = text_bottom;
                    if let Some(bounds) = control {
                        ensure!(
                            (bounds.origin.x + bounds.size.width - trailing).abs() <= 0.5,
                            "Control escaped the trailing column: {}",
                            string(&row.title)
                        );
                        ensure!(
                            bounds.origin.x >= leading - 0.5,
                            "Control clipped the leading inset"
                        );
                        if stacked {
                            let gap = bounds.origin.y - text_bottom;
                            ensure!(
                                (gap - STACKED_CONTROL_GAP).abs() <= 0.5,
                                "Stacked control must follow the entire text block: {}",
                                string(&row.title)
                            );
                            stacked_gap = Some(gap);
                        } else {
                            let gap = bounds.origin.x - title.origin.x - title.size.width;
                            ensure!(
                                gap + 0.5 >= CONTROL_GAP,
                                "Control crowds the bounded text column: {}",
                                string(&row.title)
                            );
                            let error = ((text_block.origin.y + text_block.size.height / 2.)
                                - (bounds.origin.y + bounds.size.height / 2.))
                                .abs();
                            ensure!(
                                error <= 0.5,
                                "Control is not centered with its entire text block: {}",
                                string(&row.title)
                            );
                            horizontal_gap = Some(gap);
                            centered = Some(error);
                        }
                        content_top = content_top.min(bounds.origin.y);
                        content_bottom = content_bottom.max(bounds.origin.y + bounds.size.height);
                        let view = row.control.as_ref().context("Missing native row control")?;
                        let switch: bool = unsafe {
                            msg_send![&**view,isKindOfClass:AnyClass::get(c"NSSwitch").unwrap()]
                        };
                        if switch {
                            let size = intrinsic.context("Switch has no native size")?;
                            ensure!(
                                bounds.size.width + 0.5 >= size[0]
                                    && bounds.size.height + 0.5 >= size[1],
                                "Switch allocation is smaller than AppKit's intrinsic content"
                            );
                        }
                        let primitive: bool = unsafe {
                            msg_send![&**view,isKindOfClass:AnyClass::get(c"NSControl").unwrap()]
                        };
                        if !primitive {
                            let children: *mut AnyObject = unsafe { msg_send![&**view, subviews] };
                            let count: usize = unsafe { msg_send![children, count] };
                            for index in 0..count {
                                let child: *mut AnyObject =
                                    unsafe { msg_send![children,objectAtIndex:index] };
                                let child_bounds = native_rect(unsafe { &*child }, true)?;
                                ensure!(
                                    child_bounds.origin.x >= bounds.origin.x - 0.5
                                        && child_bounds.origin.x + child_bounds.size.width
                                            <= bounds.origin.x + bounds.size.width + 0.5
                                        && child_bounds.origin.y >= bounds.origin.y - 0.5
                                        && child_bounds.origin.y + child_bounds.size.height
                                            <= bounds.origin.y + bounds.size.height + 0.5,
                                    "Compound row child escapes its native allocation: {}",
                                    string(&row.title)
                                );
                            }
                        }
                    }
                    let row_top = row
                        .divider
                        .as_ref()
                        .map(|divider| native_rect(divider, false))
                        .transpose()?
                        .map_or(group_bounds.origin.y, |bounds| bounds.origin.y);
                    let row_bottom = group
                        .rows
                        .get(index + 1)
                        .and_then(|next| next.divider.as_ref())
                        .map(|divider| native_rect(divider, false))
                        .transpose()?
                        .map_or(group_bounds.origin.y + group_bounds.size.height, |bounds| {
                            bounds.origin.y
                        });
                    let top_padding = content_top - row_top;
                    let bottom_padding = row_bottom - content_bottom;
                    let row_separation = previous_content_bottom.map(|bottom| content_top - bottom);
                    ensure!(
                        row_bottom - row_top + 0.5 >= ROW_MIN_HEIGHT
                            && top_padding + 0.5 >= ROW_PADDING
                            && bottom_padding + 0.5 >= ROW_PADDING,
                        "Row lost its comfortable height or vertical padding: {}",
                        string(&row.title)
                    );
                    if let Some(gap) = row_separation {
                        ensure!(
                            gap + 0.5 >= 2. * ROW_PADDING,
                            "Adjacent row contents are too close: {}",
                            string(&row.title)
                        );
                    }
                    previous_content_bottom = Some(content_bottom);
                    rows.push(json!({"title":string(&row.title),"title_rect":packed(title),
                        "detail_rect":detail.map(packed),"control_alignment_rect":control.map(packed),
                        "control_intrinsic_size":intrinsic,"text_block_rect":packed(text_block),
                        "text_block_center_error":centered,"horizontal_text_control_gap":horizontal_gap,
                        "stacked_text_control_gap":stacked_gap,"title_detail_gap":title_detail_gap,
                        "row_rect":packed(rect(leading,row_top,available_width,row_bottom-row_top)),
                        "top_padding":top_padding,"bottom_padding":bottom_padding,
                        "previous_row_content_gap":row_separation,"stacked":stacked}));
                }
                previous_bottom = Some(group_bounds.origin.y + group_bounds.size.height);
                // A heading-only section introduces its following custom content:
                // the heading-to-content gap was already checked above.
                next_block_gap = if group.rows.is_empty() {
                    0.
                } else {
                    SECTION_GAP
                };
            } else {
                let view = match block {
                    Block::Note(view)
                    | Block::Custom { view, .. }
                    | Block::Actions { view, .. } => view,
                    _ => unreachable!(),
                };
                let hidden: bool = unsafe { msg_send![&**view, isHidden] };
                if !hidden {
                    let bounds = native_rect(view, false)?;
                    if let Some(bottom) = previous_bottom {
                        let gap = bounds.origin.y - bottom;
                        ensure!(
                            (gap - next_block_gap).abs() <= 0.5,
                            "Form block lost the shared section spacing"
                        );
                        block_gaps.push(gap);
                    }
                    previous_bottom = Some(bounds.origin.y + bounds.size.height);
                    next_block_gap = SECTION_GAP;
                    ensure!(
                        (bounds.origin.x - leading).abs() <= 0.5
                            && (bounds.origin.x + bounds.size.width - trailing).abs() <= 0.5,
                        "Form block escaped the shared text column"
                    );
                }
            }
        }
        Ok(json!({"text_leading":leading,"control_trailing":trailing,
            "rows":rows,"sections":sections,"block_gaps":block_gaps,
            "geometry_contract":{"minimum_control_column":CONTROL_COLUMN,"text_control_gap":CONTROL_GAP,
                "maximum_text_width":TEXT_MAX_WIDTH,"minimum_side_by_side_text_width":TEXT_MIN_WIDTH,
                "row_padding":ROW_PADDING,"minimum_row_height":ROW_MIN_HEIGHT,
                "title_detail_gap":TITLE_DETAIL_GAP,"stacked_control_gap":STACKED_CONTROL_GAP,
                "heading_group_gap":HEADING_GROUP_GAP,"section_gap":SECTION_GAP},
            "source":"actual Cocoa-converted frames and native alignment rectangles"}))
    }
}
fn text_height(label: &AnyObject, width: f64) -> f64 {
    unsafe {
        // Measure the text cell's real word wrapping and drawing insets.
        // Attributed-string bounds can undercount the lines drawn by NSTextField.
        let cell: *mut AnyObject = msg_send![label, cell];
        let size: NSSize = msg_send![cell,cellSizeForBounds:rect(0.,0.,width.max(1.),10000.)];
        (size.height.ceil() + 2.).max(18.)
    }
}

/// Keep embedded editors fitted to their actual clip width, including legacy scrollbars.
fn fit_scroll_document(view: &AnyObject) {
    unsafe {
        let is_scroll: bool = msg_send![view,isKindOfClass:AnyClass::get(c"NSScrollView").unwrap()];
        if !is_scroll {
            return;
        }
        let document: *mut AnyObject = msg_send![view, documentView];
        if document.is_null() {
            return;
        }
        let is_editor: bool =
            msg_send![document,isKindOfClass:AnyClass::get(c"NSTextView").unwrap()];
        if is_editor {
            let clip: *mut AnyObject = msg_send![view, contentView];
            let viewport: NSRect = msg_send![clip, bounds];
            let bounds: NSRect = msg_send![document, frame];
            let _: () = msg_send![document,setFrameSize:NSSize::new(viewport.size.width,bounds.size.height.max(viewport.size.height))];
        }
    }
}

pub(super) struct Table {
    pub scroll: Obj,
    pub view: Obj,
    source: Retained<SettingsTableSource>,
}
struct TableState {
    rows: RefCell<Vec<BTreeMap<String, String>>>,
    keys: RefCell<Vec<String>>,
    queue: Rc<RefCell<super::super::macos_settings_contract::UiQueue>>,
    rules: bool,
    updating: Cell<bool>,
}
define_class!(
    #[unsafe(super = NSObject)]
    #[thread_kind = MainThreadOnly]
    #[ivars = TableState]
    struct SettingsTableSource;
    unsafe impl NSObjectProtocol for SettingsTableSource {}
    impl SettingsTableSource {
        #[unsafe(method(numberOfRowsInTableView:))]
        fn count(&self,_table:&AnyObject)->isize { self.ivars().rows.borrow().len() as isize }
        #[unsafe(method_id(tableView:objectValueForTableColumn:row:))]
        fn value(&self,_table:&AnyObject,column:&AnyObject,row:isize)->Retained<NSString> {
            let identifier:*mut NSString=unsafe{msg_send![column,identifier]};
            let key=if identifier.is_null(){String::new()}else{unsafe{&*identifier}.to_string()};
            NSString::from_str(self.ivars().rows.borrow().get(row as usize).and_then(|r|r.get(&key)).map(String::as_str).unwrap_or(""))
        }
        #[unsafe(method(tableViewSelectionDidChange:))]
        fn selected(&self,notification:&NSNotification) {
            if !self.ivars().rules || self.ivars().updating.get(){return}
            unsafe { let table:*mut AnyObject=msg_send![notification,object]; let row:isize=msg_send![table,selectedRow];
                if let Some(key)=self.ivars().keys.borrow().get(row as usize) { self.ivars().queue.borrow_mut().pending.push_back(UiAction::SelectRule(key.clone())); }
            }
        }
        #[unsafe(method(tableView:validateDrop:proposedRow:proposedDropOperation:))]
        fn validate(&self,_table:&AnyObject,info:&AnyObject,_row:isize,_op:usize)->usize { if self.ivars().rules&&!drop_items(info).is_empty(){1}else{0} }
        #[unsafe(method(tableView:acceptDrop:row:dropOperation:))]
        fn accept(&self,_table:&AnyObject,info:&AnyObject,_row:isize,_op:usize)->bool {
            let items=drop_items(info); if items.is_empty(){return false.into()} self.ivars().queue.borrow_mut().pending.push_back(UiAction::RuleDrop(items)); true
        }
    }
);
fn drop_items(info: &AnyObject) -> Vec<(u8, String)> {
    let mut result = Vec::new();
    unsafe {
        let pasteboard: *mut AnyObject = msg_send![info, draggingPasteboard];
        let classes: Obj = obj("NSMutableArray");
        let url_class = objc2::runtime::AnyClass::get(c"NSURL").unwrap();
        let _: () = msg_send![&*classes,addObject:url_class as *const AnyClass as *const AnyObject];
        let options = obj("NSDictionary");
        let urls: *mut AnyObject =
            msg_send![pasteboard,readObjectsForClasses:&*classes,options:&*options];
        if urls.is_null() {
            return result;
        }
        let count: usize = msg_send![urls, count];
        for i in 0..count {
            let url: *mut AnyObject = msg_send![urls,objectAtIndex:i];
            let file: bool = msg_send![url, isFileURL];
            if file {
                let path: *mut NSString = msg_send![url, path];
                if path.is_null() || !(&*path).to_string().ends_with(".app") {
                    continue;
                }
                let bundle_class = objc2::runtime::AnyClass::get(c"NSBundle").unwrap();
                let bundle: *mut AnyObject = msg_send![bundle_class,bundleWithPath:path];
                if bundle.is_null() {
                    continue;
                }
                let id: *mut NSString = msg_send![bundle, bundleIdentifier];
                if !id.is_null() {
                    result.push((0, (&*id).to_string()));
                }
            } else {
                let scheme: *mut NSString = msg_send![url, scheme];
                if scheme.is_null()
                    || !["http", "https"].contains(&(&*scheme).to_string().to_lowercase().as_str())
                {
                    continue;
                }
                let host: *mut NSString = msg_send![url, host];
                if !host.is_null() {
                    result.push((1, (&*host).to_string()));
                }
            }
        }
    }
    result
}
impl Table {
    pub fn new(kit: &Kit, id: &str, columns: &[(&str, &str, f64)], rules: bool) -> Self {
        let mtm = MainThreadMarker::new().expect("settings main thread");
        let source = SettingsTableSource::alloc(mtm).set_ivars(TableState {
            rows: RefCell::new(Vec::new()),
            keys: RefCell::new(Vec::new()),
            queue: kit.queue.clone(),
            rules,
            updating: Cell::new(false),
        });
        let source: Retained<SettingsTableSource> = unsafe { msg_send![super(source), init] };
        let view = obj("NSTableView");
        frame(&view, &rect(0., 0., 550., 220.));
        identify(
            &view,
            id,
            if rules {
                "Apps and websites with their own settings"
            } else {
                "Accepted completions by day, month or application"
            },
        );
        unsafe {
            for (key, title, width) in columns {
                let key = NSString::from_str(key);
                let class = AnyClass::get(c"NSTableColumn").unwrap();
                let raw: *mut AnyObject = msg_send![class, alloc];
                let raw: *mut AnyObject = msg_send![raw,initWithIdentifier:&*key];
                let column = Retained::from_raw(raw).expect("native table column");
                let title = NSString::from_str(title);
                let _: () = msg_send![&*column,setTitle:&*title];
                let _: () = msg_send![&*column,setWidth:*width];
                let _: () = msg_send![&*view,addTableColumn:&*column];
            }
            let _: () = msg_send![&*view,setDataSource:&*source];
            let _: () = msg_send![&*view,setDelegate:&*source];
            let _: () = msg_send![&*view,setRowHeight:if rules{44.0f64}else{24.0f64}];
            let _: () = msg_send![&*view,setUsesAlternatingRowBackgroundColors:true];
            let _: () = msg_send![&*view,setColumnAutoresizingStyle:1usize];
            let _: () = msg_send![&*view,setAutoresizingMask:2usize];
            let _: () = msg_send![&*view,setAllowsColumnReordering:false];
            if rules {
                let types = obj("NSMutableArray");
                for name in ["public.file-url", "public.url"] {
                    let s = NSString::from_str(name);
                    let _: () = msg_send![&*types,addObject:&*s];
                }
                let _: () = msg_send![&*view,registerForDraggedTypes:&*types];
            }
        }
        let scroll = scroll(&view, 220.);
        Self {
            scroll,
            view,
            source,
        }
    }
    pub fn set(&self, rows: Vec<BTreeMap<String, String>>, keys: Vec<String>, selected: &str) {
        self.source.ivars().updating.set(true);
        let changed = *self.source.ivars().rows.borrow() != rows
            || *self.source.ivars().keys.borrow() != keys;
        if changed {
            *self.source.ivars().rows.borrow_mut() = rows;
            *self.source.ivars().keys.borrow_mut() = keys;
            unsafe {
                let _: () = msg_send![&*self.view, reloadData];
            }
        }
        unsafe {
            if let Some(index) = self
                .source
                .ivars()
                .keys
                .borrow()
                .iter()
                .position(|k| k == selected)
            {
                let current: isize = msg_send![&*self.view, selectedRow];
                if current != index as isize {
                    let class = AnyClass::get(c"NSIndexSet").unwrap();
                    let indexes: *mut AnyObject = msg_send![class,indexSetWithIndex:index];
                    let _: () =
                        msg_send![&*self.view,selectRowIndexes:indexes,byExtendingSelection:false];
                }
            } else if self.source.ivars().rules {
                let _: () = msg_send![&*self.view,deselectAll:std::ptr::null::<AnyObject>()];
            }
        }
        self.source.ivars().updating.set(false);
    }
}
impl Drop for Table {
    fn drop(&mut self) {
        unsafe {
            let _: () = msg_send![&*self.view,setDelegate:std::ptr::null::<AnyObject>()];
            let _: () = msg_send![&*self.view,setDataSource:std::ptr::null::<AnyObject>()];
        }
    }
}
pub(super) fn scroll(document: &AnyObject, height: f64) -> Obj {
    let scroll = obj("NSScrollView");
    frame(&scroll, &rect(0., 0., 550., height));
    unsafe {
        let _: () = msg_send![&*scroll,setHasVerticalScroller:true];
        let _: () = msg_send![&*scroll,setAutohidesScrollers:true];
        let _: () = msg_send![&*scroll,setAutomaticallyAdjustsContentInsets:false];
        let _: () = msg_send![&*scroll,setBorderType:2usize];
        let _: () = msg_send![&*scroll,setDocumentView:document];
    }
    scroll
}

pub(super) struct Chart {
    pub root: Obj,
    bars: Vec<(Obj, Obj, Obj, f64)>,
    empty: bool,
}
impl Chart {
    pub fn new(kit: &Kit) -> Self {
        let root = kit.view(rect(0., 0., 550., 200.));
        identify(&root, "inklow.statistics.chart", "Statistics bar chart");
        Self {
            root,
            bars: Vec::new(),
            empty: true,
        }
    }
    pub fn set(&mut self, kit: &Kit, rows: &[Value], metric: &str) {
        self.empty = rows.is_empty();
        for (label, bar, value, _) in self.bars.drain(..) {
            unsafe {
                let _: () = msg_send![&*label, removeFromSuperview];
                let _: () = msg_send![&*bar, removeFromSuperview];
                let _: () = msg_send![&*value, removeFromSuperview];
            }
        }
        let maximum = rows
            .iter()
            .map(|r| r[metric].as_f64().unwrap_or(0.))
            .fold(0., f64::max)
            .max(1.);
        for row in rows.iter().rev().take(12).rev() {
            let v = row[metric].as_f64().unwrap_or(0.).max(0.);
            let label = kit.label(
                row["label"].as_str().unwrap_or(""),
                rect(0., 0., 100., 20.),
                11.,
                false,
            );
            let bar = obj("NSBox");
            let display = if metric == "estimated_seconds" {
                format!("{:.1} min", v / 60.)
            } else {
                format!("{v:.0}")
            };
            let value = kit.label(&display, rect(0., 0., 80., 20.), 11., false);
            unsafe {
                let _: () = msg_send![&*bar,setBoxType:4usize];
                let _: () = msg_send![&*bar,setBorderType:0usize];
                let _: () = msg_send![&*bar,setTitlePosition:0usize];
                let color: *mut AnyObject =
                    msg_send![AnyClass::get(c"NSColor").unwrap(), controlAccentColor];
                let _: () = msg_send![&*bar,setFillColor:color];
                let _: () = msg_send![&*bar,setCornerRadius:3.0f64];
            }
            identify(
                &bar,
                "",
                &format!("{}: {display}", row["label"].as_str().unwrap_or("")),
            );
            add(&self.root, &label);
            add(&self.root, &bar);
            add(&self.root, &value);
            self.bars.push((label, bar, value, v / maximum));
        }
        if rows.is_empty() {
            let label = kit.label(
                "No accepted completions in this period",
                rect(0., 0., 400., 25.),
                13.,
                false,
            );
            add(&self.root, &label);
            let bar = kit.view(rect(0., 0., 0., 0.));
            let value = kit.view(rect(0., 0., 0., 0.));
            self.bars.push((label, bar, value, 0.));
        }
        self.reflow();
    }
    pub fn reflow(&self) {
        let bounds: NSRect = unsafe { msg_send![&*self.root, bounds] };
        let width = bounds.size.width;
        for (i, (label, bar, value, fraction)) in self.bars.iter().enumerate() {
            if self.empty {
                frame(label, &rect(0., 0., width, 40.));
                continue;
            }
            let y = i as f64 * 24.;
            frame(label, &rect(0., y, 120., 22.));
            frame(
                bar,
                &rect(
                    124.,
                    y + 4.,
                    ((width - 210.).max(20.) * fraction).max(0.),
                    14.,
                ),
            );
            frame(value, &rect((width - 80.).max(150.), y, 80., 22.));
        }
    }
}
fn secondary(view: &AnyObject) {
    unsafe {
        let color: *mut AnyObject =
            msg_send![AnyClass::get(c"NSColor").unwrap(), secondaryLabelColor];
        let _: () = msg_send![view,setTextColor:color];
    }
}
