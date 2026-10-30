//! Retained native layout, aggregate tables, and drag-and-drop rule lists.
use super::*;
use objc2::{DefinedClass, MainThreadOnly, define_class};
use objc2_foundation::{MainThreadMarker, NSNotification, NSObject, NSObjectProtocol, NSString};
use std::cell::{Cell, RefCell};
use std::rc::Rc;

struct Row {
    title: Obj,
    detail: Option<Obj>,
    control: Option<Obj>,
    control_width: f64,
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
        let control_width = control
            .as_ref()
            .map(|control| {
                let bounds: NSRect = unsafe { msg_send![&**control, frame] };
                if bounds.size.width > 0. && bounds.size.width < 400. {
                    bounds.size.width
                } else {
                    220.
                }
            })
            .unwrap_or(220.);
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
            control_width,
            divider,
        });
    }
    pub fn reflow(&mut self, width: f64, viewport: f64) {
        self.width = width;
        let inner = (width - 48.).max(1.);
        let mut y = 24.;
        for block in &mut self.blocks {
            match block {
                Block::Group(group) => {
                    if let Some(heading) = &group.heading {
                        let height = text_height(heading, inner - 8.);
                        frame(heading, &rect(28., y, inner - 8., height));
                        y += height + 7.;
                    }
                    let mut row_y = 0.;
                    for row in &group.rows {
                        let control_width = row.control_width.min(inner - 28.);
                        let stacked = row.control.is_some() && inner - control_width - 42. < 180.;
                        let text_width = if stacked || row.control.is_none() {
                            inner - 28.
                        } else {
                            inner - control_width - 42.
                        };
                        let title_height = text_height(&row.title, text_width);
                        frame(
                            &row.title,
                            &rect(14., row_y + 11., text_width, title_height),
                        );
                        let mut bottom = row_y + 11. + title_height;
                        if let Some(control) = &row.control {
                            let is_label: bool = unsafe {
                                let text_field: bool = msg_send![&**control,isKindOfClass:AnyClass::get(c"NSTextField").unwrap()];
                                let editable: bool = if text_field {
                                    msg_send![&**control, isEditable]
                                } else {
                                    false
                                };
                                text_field && !editable
                            };
                            let control_height = if is_label {
                                text_height(control, control_width)
                            } else {
                                27.
                            };
                            let control_y = if stacked { bottom + 7. } else { row_y + 8. };
                            frame(
                                control,
                                &rect(
                                    if stacked {
                                        14.
                                    } else {
                                        inner - 14. - control_width
                                    },
                                    control_y,
                                    control_width,
                                    control_height,
                                ),
                            );
                            bottom = bottom.max(control_y + control_height);
                        }
                        if let Some(detail) = &row.detail {
                            let height = text_height(detail, inner - 28.);
                            frame(detail, &rect(14., bottom + 6., inner - 28., height));
                            bottom += height + 6.;
                        }
                        if let Some(divider) = &row.divider {
                            frame(divider, &rect(14., row_y, inner - 28., 1.));
                        }
                        row_y += (bottom - row_y + 11.).max(46.);
                    }
                    frame(&group.box_view, &rect(24., y, inner, row_y));
                    frame(&group.content, &rect(0., 0., inner, row_y));
                    if row_y > 0. {
                        y += row_y + 22.;
                    }
                }
                Block::Note(view) => {
                    let height = text_height(view, inner - 8.);
                    frame(view, &rect(28., y, inner - 8., height));
                    y += height + 12.;
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
                            let bounds: NSRect = msg_send![&**action, frame];
                            let action_width = (bounds.size.width + 18.).max(100.).min(inner);
                            if x > 0. && x + action_width > inner {
                                x = 0.;
                                row_y += 36.;
                            }
                            frame(action, &rect(x, row_y, action_width, 28.));
                            x += action_width + 8.;
                        }
                    }
                    let height = row_y + 28.;
                    frame(view, &rect(24., y, inner, height));
                    y += height + 16.;
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
                        (*height).max(text_height(view, inner))
                    } else {
                        *height
                    };
                    frame(view, &rect(24., y, inner, height));
                    fit_scroll_document(view);
                    y += height + 16.;
                }
            }
        }
        frame(&self.root, &rect(0., 0., width, y.max(viewport)));
    }
}
fn text_height(label: &AnyObject, width: f64) -> f64 {
    unsafe {
        let text: *mut AnyObject = msg_send![label, attributedStringValue];
        let bounds: NSRect =
            msg_send![text,boundingRectWithSize:NSSize::new(width.max(1.),10000.),options:3usize];
        (bounds.size.height.ceil() + 2.).max(18.)
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
