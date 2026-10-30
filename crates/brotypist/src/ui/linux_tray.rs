//! Freedesktop StatusNotifierItem and DBusMenu; no Python or GI exporter.
use super::*;
use std::{
    collections::HashMap,
    sync::atomic::{AtomicBool, Ordering},
    thread::{self, JoinHandle},
};
use zbus::zvariant::{OwnedObjectPath, OwnedValue, Value as Variant};
type Props = HashMap<String, OwnedValue>;
type Layout = (i32, Props, Vec<OwnedValue>);
#[derive(Clone)]
struct Row {
    id: i32,
    label: String,
    action: String,
    enabled: bool,
    checked: Option<bool>,
    separator: bool,
    children: Vec<Row>,
}
struct State {
    rows: Vec<Row>,
    revision: u32,
    title: String,
    status: String,
    app: String,
    domain: String,
    menu_intent: (String, String),
    icons: Vec<(i32, i32, Vec<u8>)>,
    tracking: bool,
    config: Config,
    native_detail: String,
}
struct Item {
    state: Arc<Mutex<State>>,
    sender: Sender,
}
struct Menu {
    state: Arc<Mutex<State>>,
    sender: Sender,
}
fn string(value: &str) -> OwnedValue {
    Variant::from(value).try_to_owned().expect("D-Bus string")
}
// Wire contract: https://github.com/gnustep/libs-dbuskit/blob/master/Bundles/DBusMenu/com.canonical.dbusmenu.xml
fn props(row: &Row) -> Props {
    let mut p = Props::new();
    if row.separator {
        p.insert("type".into(), string("separator"));
    } else {
        p.insert("label".into(), string(&row.label.replace('_', "__")));
        p.insert("enabled".into(), OwnedValue::from(row.enabled));
        p.insert("visible".into(), OwnedValue::from(true));
        if let Some(checked) = row.checked {
            p.insert("toggle-type".into(), string("checkmark"));
            p.insert(
                "toggle-state".into(),
                OwnedValue::from(if checked { 1i32 } else { 0i32 }),
            );
        }
        if !row.children.is_empty() {
            p.insert("children-display".into(), string("submenu"));
        }
    }
    p
}
fn filtered(mut props: Props, names: &[String]) -> Props {
    if !names.is_empty() {
        props.retain(|key, _| names.contains(key));
    }
    props
}
fn root_props() -> Props {
    let mut p = Props::new();
    p.insert("label".into(), string(""));
    p.insert("enabled".into(), OwnedValue::from(true));
    p.insert("visible".into(), OwnedValue::from(true));
    p.insert("children-display".into(), string("submenu"));
    p
}
fn layout(row: &Row, depth: i32, names: &[String]) -> Layout {
    let children = if depth == 0 {
        Vec::new()
    } else {
        row.children
            .iter()
            .map(|r| {
                Variant::new(layout(r, if depth < 0 { -1 } else { depth - 1 }, names))
                    .try_to_owned()
                    .expect("D-Bus menu layout")
            })
            .collect()
    };
    (row.id, filtered(props(row), names), children)
}
fn collect_properties(rows: &[Row], names: &[String], out: &mut Vec<(i32, Props)>) {
    for row in rows {
        out.push((row.id, filtered(props(row), names)));
        collect_properties(&row.children, names, out);
    }
}
fn find(rows: &[Row], id: i32) -> Option<&Row> {
    for row in rows {
        if row.id == id {
            return Some(row);
        }
        if let Some(r) = find(&row.children, id) {
            return Some(r);
        }
    }
    None
}
#[zbus::interface(name = "org.kde.StatusNotifierItem")]
impl Item {
    #[zbus(property)]
    fn category(&self) -> &str {
        "ApplicationStatus"
    }
    #[zbus(property)]
    fn id(&self) -> &str {
        "com.inklow.Inklow"
    }
    #[zbus(property)]
    fn title(&self) -> String {
        self.state.lock().unwrap().title.clone()
    }
    #[zbus(property)]
    fn status(&self) -> String {
        self.state.lock().unwrap().status.clone()
    }
    #[zbus(property)]
    fn window_id(&self) -> i32 {
        0
    }
    #[zbus(property)]
    fn icon_name(&self) -> &str {
        "com.inklow.Inklow"
    }
    #[zbus(property)]
    fn icon_pixmap(&self) -> Vec<(i32, i32, Vec<u8>)> {
        self.state.lock().unwrap().icons.clone()
    }
    #[zbus(property)]
    fn overlay_icon_name(&self) -> &str {
        ""
    }
    #[zbus(property)]
    fn attention_icon_name(&self) -> &str {
        "com.inklow.Inklow"
    }
    #[zbus(property)]
    fn attention_movie_name(&self) -> &str {
        ""
    }
    #[zbus(property)]
    fn tool_tip(&self) -> (String, Vec<(i32, i32, Vec<u8>)>, String, String) {
        let s = self.state.lock().unwrap();
        (
            "com.inklow.Inklow".into(),
            s.icons.clone(),
            "BroTypist".into(),
            s.title.clone(),
        )
    }
    #[zbus(property)]
    fn item_is_menu(&self) -> bool {
        true
    }
    #[zbus(property)]
    fn menu(&self) -> OwnedObjectPath {
        OwnedObjectPath::try_from("/MenuBar").unwrap()
    }
    fn activate(&self, _x: i32, _y: i32) {
        let _ = self.sender.send(UiAction::Settings("General".into()));
    }
    fn secondary_activate(&self, _x: i32, _y: i32) {
        let _ = self.sender.send(UiAction::Settings("General".into()));
    }
    fn context_menu(&self, _x: i32, _y: i32) {
        let _ = self.sender.send(UiAction::Settings("General".into()));
    }
    fn scroll(&self, _delta: i32, _orientation: &str) {}
}
#[zbus::interface(name = "com.canonical.dbusmenu")]
impl Menu {
    #[zbus(property)]
    fn version(&self) -> u32 {
        3
    }
    #[zbus(property)]
    fn text_direction(&self) -> &str {
        "ltr"
    }
    #[zbus(property)]
    fn status(&self) -> &str {
        "normal"
    }
    #[zbus(property)]
    fn icon_theme_path(&self) -> Vec<String> {
        Vec::new()
    }
    fn get_layout(
        &self,
        parent_id: i32,
        recursion_depth: i32,
        property_names: Vec<String>,
    ) -> zbus::fdo::Result<(u32, Layout)> {
        if recursion_depth < -1 {
            return Err(zbus::fdo::Error::InvalidArgs(
                "Invalid recursion depth".into(),
            ));
        }
        let s = self.state.lock().unwrap();
        let value = if parent_id == 0 {
            let children = if recursion_depth == 0 {
                Vec::new()
            } else {
                s.rows
                    .iter()
                    .map(|r| {
                        Variant::new(layout(
                            r,
                            if recursion_depth < 0 {
                                -1
                            } else {
                                recursion_depth - 1
                            },
                            &property_names,
                        ))
                        .try_to_owned()
                        .expect("D-Bus root layout")
                    })
                    .collect()
            };
            (0, filtered(root_props(), &property_names), children)
        } else {
            layout(
                find(&s.rows, parent_id)
                    .ok_or_else(|| zbus::fdo::Error::InvalidArgs("Unknown menu item".into()))?,
                recursion_depth,
                &property_names,
            )
        };
        Ok((s.revision, value))
    }
    fn get_group_properties(
        &self,
        ids: Vec<i32>,
        property_names: Vec<String>,
    ) -> Vec<(i32, Props)> {
        let s = self.state.lock().unwrap();
        if ids.is_empty() {
            let mut result = vec![(0, filtered(root_props(), &property_names))];
            collect_properties(&s.rows, &property_names, &mut result);
            result
        } else {
            ids.into_iter()
                .filter_map(|id| {
                    if id == 0 {
                        Some((0, filtered(root_props(), &property_names)))
                    } else {
                        find(&s.rows, id).map(|r| (id, filtered(props(r), &property_names)))
                    }
                })
                .collect()
        }
    }
    fn get_property(&self, id: i32, name: &str) -> zbus::fdo::Result<OwnedValue> {
        let s = self.state.lock().unwrap();
        let value = if id == 0 {
            root_props().remove(name)
        } else {
            find(&s.rows, id).and_then(|r| props(r).remove(name))
        };
        value.ok_or_else(|| zbus::fdo::Error::InvalidArgs("Unknown menu property".into()))
    }
    fn event(&self, id: i32, event_id: &str, _data: OwnedValue, _timestamp: u32) {
        if event_id == "opened" {
            self.open();
            return;
        }
        if event_id == "closed" {
            if id == 0 {
                self.close();
            }
            return;
        }
        if event_id != "clicked" {
            return;
        }
        let action = {
            let s = self.state.lock().unwrap();
            find(&s.rows, id)
                .filter(|r| r.enabled && !r.action.is_empty())
                .map(|r| (r.action.clone(), s.menu_intent.clone()))
        };
        if let Some((action, (app, domain))) = action {
            if action == "app_toggle" {
                let _ = self
                    .sender
                    .send(UiAction::Command(Command::ToggleApp { app, domain }));
            } else {
                let _ = self.sender.send(UiAction::Native(action));
            }
            // Exporters need not send a final hide event. Quit never waits for it.
            self.close();
        }
    }
    fn event_group(&self, events: Vec<(i32, String, OwnedValue, u32)>) -> Vec<i32> {
        let mut errors = Vec::new();
        for (id, event, data, time) in events {
            let valid = id == 0 || find(&self.state.lock().unwrap().rows, id).is_some();
            if valid {
                self.event(id, &event, data, time);
            } else {
                errors.push(id);
            }
        }
        errors
    }
    fn about_to_show(&self, id: i32) -> zbus::fdo::Result<bool> {
        if id != 0 && find(&self.state.lock().unwrap().rows, id).is_none() {
            return Err(zbus::fdo::Error::InvalidArgs("Unknown menu item".into()));
        }
        self.open();
        Ok(false)
    }
    fn about_to_show_group(&self, ids: Vec<i32>) -> (Vec<i32>, Vec<i32>) {
        let errors = {
            let s = self.state.lock().unwrap();
            ids.iter()
                .copied()
                .filter(|id| *id != 0 && find(&s.rows, *id).is_none())
                .collect::<Vec<_>>()
        };
        if ids.iter().any(|id| !errors.contains(id)) {
            self.open();
        }
        (Vec::new(), errors)
    }
}
impl Menu {
    fn open(&self) {
        let mut s = self.state.lock().unwrap();
        if !s.tracking {
            s.menu_intent = (s.app.clone(), s.domain.clone());
            s.tracking = true;
        }
        drop(s);
        let _ = self.sender.send(UiAction::Native("menu_open".into()));
    }
    fn close(&self) {
        self.state.lock().unwrap().tracking = false;
        let _ = self.sender.send(UiAction::Native("menu_closed".into()));
    }
}
pub(super) struct Tray {
    state: Arc<Mutex<State>>,
    available: Arc<AtomicBool>,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}
fn row(id: i32, label: &str, action: &str, enabled: bool) -> Row {
    Row {
        id,
        label: label.into(),
        action: action.into(),
        enabled,
        checked: None,
        separator: false,
        children: Vec::new(),
    }
}
fn rows(status: &Value, config: &Config) -> Vec<Row> {
    let mut id = 0;
    let mut make = |title: &str, action: &str, enabled: bool| {
        id += 1;
        row(id, title, action, enabled)
    };
    let mut rows = vec![
        make(&format!("BroTypist — {}", text(status, "state")), "", false),
        make(&text(status, "native_status"), "", false),
        make(
            &format!(
                "Model: {}",
                if config.backend == "gguf" {
                    &config.model_path
                } else {
                    &config.model
                }
            ),
            "",
            false,
        ),
        make(&text(status, "inline_message"), "", false),
    ];
    rows.push(make(&text(status, "context_message"), "", false));
    let mut separator = make("", "", false);
    separator.separator = true;
    rows.push(separator);
    let mut predictions = make("Predictions", "toggle", true);
    predictions.checked = Some(config.global_enabled);
    rows.push(predictions);
    rows.push(make("Resume now", "resume", true));
    let mut pause = make("Pause", "", true);
    pause.children = vec![
        make("For 5 minutes", "pause5", true),
        make("For 15 minutes", "pause15", true),
        make("For 1 hour", "pause60", true),
        make("Until tomorrow", "tomorrow", true),
    ];
    rows.push(pause);
    let mut app = make(
        &format!(
            "Predictions in {} (this session)",
            text(status, "current_app")
        ),
        "app_toggle",
        !text(status, "current_app").is_empty(),
    );
    app.checked = Some(status["current_app_enabled"] == true);
    rows.push(app);
    for (index, (title, action, enabled)) in [
        ("Request completion", "force", true),
        ("Alternative words", "alternatives", true),
        (
            "Selected-text synonyms — unavailable on Linux",
            "synonyms",
            false,
        ),
        ("Settings…", "settings", true),
        ("Welcome…", "welcome", true),
        ("Models…", "models", true),
        ("Check for Updates…", "updates", true),
        ("Quit BroTypist", "quit", true),
    ]
    .into_iter()
    .enumerate()
    {
        if matches!(index, 0 | 3 | 7) {
            let mut separator = make("", "", false);
            separator.separator = true;
            rows.push(separator);
        }
        rows.push(make(title, action, enabled));
    }
    rows
}
fn icon_pixmaps() -> Vec<(i32, i32, Vec<u8>)> {
    let pixbuf = app_icon().expect("Validated embedded application artwork");
    let Some(bytes) = pixbuf.pixel_bytes() else {
        return Vec::new();
    };
    let pixels = bytes.as_ref();
    let width = pixbuf.width();
    let height = pixbuf.height();
    let stride = pixbuf.rowstride() as usize;
    let channels = pixbuf.n_channels() as usize;
    let mut argb = Vec::with_capacity((width * height * 4) as usize);
    for y in 0..height as usize {
        for x in 0..width as usize {
            let p = &pixels[y * stride + x * channels..];
            argb.extend_from_slice(&[if channels == 4 { p[3] } else { 255 }, p[0], p[1], p[2]]);
        }
    }
    vec![(width, height, argb)]
}
impl Tray {
    pub fn start(sender: Sender) -> Result<Self> {
        let state = Arc::new(Mutex::new(State {
            rows: rows(&json!({}), &Config::default()),
            revision: 1,
            title: "BroTypist".into(),
            status: "Active".into(),
            app: String::new(),
            domain: String::new(),
            menu_intent: Default::default(),
            icons: icon_pixmaps(),
            tracking: false,
            config: Config::default(),
            native_detail: String::new(),
        }));
        let available = Arc::new(AtomicBool::new(false));
        let stop = Arc::new(AtomicBool::new(false));
        let shared = state.clone();
        let alive = available.clone();
        let stopping = stop.clone();
        let (initialized, initial) = mpsc::sync_channel(1);
        let thread = thread::Builder::new()
            .name("brotypist-status-notifier".into())
            .spawn(move || {
                while !stopping.load(Ordering::Acquire) {
                    let connection = (|| -> Result<zbus::blocking::Connection> {
                        let connection = zbus::blocking::connection::Builder::session()?
                            .method_timeout(Duration::from_millis(750))
                            .serve_at(
                                "/StatusNotifierItem",
                                Item {
                                    state: shared.clone(),
                                    sender: sender.clone(),
                                },
                            )?
                            .serve_at(
                                "/MenuBar",
                                Menu {
                                    state: shared.clone(),
                                    sender: sender.clone(),
                                },
                            )?
                            .build()?;
                        Ok(connection)
                    })();
                    if let Ok(connection) = connection {
                        let mut registered_owner = String::new();
                        let mut revision = 0;
                        while !stopping.load(Ordering::Acquire) {
                            if connection.is_closed() {
                                break;
                            }
                            let owner = connection
                                .call_method(
                                    Some("org.freedesktop.DBus"),
                                    "/org/freedesktop/DBus",
                                    Some("org.freedesktop.DBus"),
                                    "GetNameOwner",
                                    &("org.kde.StatusNotifierWatcher",),
                                )
                                .ok()
                                .and_then(|reply| reply.body().deserialize::<String>().ok())
                                .unwrap_or_default();
                            let host = !owner.is_empty()
                                && connection
                                    .call_method(
                                        Some("org.kde.StatusNotifierWatcher"),
                                        "/StatusNotifierWatcher",
                                        Some("org.freedesktop.DBus.Properties"),
                                        "Get",
                                        &(
                                            "org.kde.StatusNotifierWatcher",
                                            "IsStatusNotifierHostRegistered",
                                        ),
                                    )
                                    .ok()
                                    .and_then(|reply| reply.body().deserialize::<OwnedValue>().ok())
                                    .and_then(|value| bool::try_from(value).ok())
                                    .unwrap_or(false);
                            if host && registered_owner != owner {
                                let name =
                                    connection.unique_name().map(|s| s.as_str()).unwrap_or("");
                                if connection
                                    .call_method(
                                        Some("org.kde.StatusNotifierWatcher"),
                                        "/StatusNotifierWatcher",
                                        Some("org.kde.StatusNotifierWatcher"),
                                        "RegisterStatusNotifierItem",
                                        &(name,),
                                    )
                                    .is_ok()
                                {
                                    registered_owner = owner.clone();
                                }
                            }
                            alive.store(host && registered_owner == owner, Ordering::Release);
                            let _ = initialized.try_send(());
                            if !host {
                                registered_owner.clear();
                                shared.lock().unwrap().tracking = false;
                                let _ = sender.send(UiAction::Native("menu_closed".into()));
                            }
                            let current = shared.lock().unwrap().revision;
                            if revision != current {
                                revision = current;
                                let _ = connection.emit_signal(
                                    None::<&str>,
                                    "/MenuBar",
                                    "com.canonical.dbusmenu",
                                    "LayoutUpdated",
                                    &(revision, 0i32),
                                );
                                let _ = connection.emit_signal(
                                    None::<&str>,
                                    "/StatusNotifierItem",
                                    "org.kde.StatusNotifierItem",
                                    "NewTitle",
                                    &(),
                                );
                                let _ = connection.emit_signal(
                                    None::<&str>,
                                    "/StatusNotifierItem",
                                    "org.kde.StatusNotifierItem",
                                    "NewToolTip",
                                    &(),
                                );
                            }
                            for _ in 0..10 {
                                if stopping.load(Ordering::Acquire) {
                                    break;
                                }
                                thread::sleep(Duration::from_millis(100));
                            }
                        }
                    }
                    alive.store(false, Ordering::Release);
                    let _ = initialized.try_send(());
                    if !stopping.load(Ordering::Acquire) {
                        for _ in 0..10 {
                            if stopping.load(Ordering::Acquire) {
                                break;
                            }
                            thread::sleep(Duration::from_millis(100));
                        }
                    }
                }
            })?;
        // One bounded initial registration attempt avoids flashing recovery Settings on a healthy tray launch.
        let _ = initial.recv_timeout(Duration::from_secs(2));
        Ok(Self {
            state,
            available,
            stop,
            thread: Some(thread),
        })
    }
    pub fn available(&self) -> bool {
        !self.stop.load(Ordering::Acquire) && self.available.load(Ordering::Acquire)
    }
    pub fn request_stop(&self) {
        self.stop.store(true, Ordering::Release);
    }
    pub fn is_stopped(&self) -> bool {
        self.thread.as_ref().is_none_or(JoinHandle::is_finished)
    }
    pub fn update(&self, status: &Value, config: &Config) {
        let mut s = self.state.lock().unwrap();
        let title = format!(
            "BroTypist — {} — {}",
            text(status, "state"),
            text(status, "message")
        );
        let detail = format!(
            "{}\n{}\n{}\n{}",
            text(status, "native_status"),
            text(status, "inline_message"),
            status["current_app_enabled"],
            text(status, "context_message")
        );
        if title != s.title
            || s.app != text(status, "current_app")
            || s.domain != text(status, "current_domain")
            || s.config != *config
            || s.native_detail != detail
        {
            s.title = title;
            s.app = text(status, "current_app");
            s.domain = text(status, "current_domain");
            s.config = config.clone();
            s.native_detail = detail;
            s.rows = rows(status, config);
            s.revision = s.revision.wrapping_add(1).max(1);
        }
    }
}
impl Drop for Tray {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn essential_controls_are_exported_and_quit_has_no_hide_fence() {
        let rows = rows(
            &json!({"current_app":"fixture.writer","inline_available":false}),
            &Config::default(),
        );
        for action in [
            "settings",
            "quit",
            "models",
            "resume",
            "pause60",
            "force",
            "alternatives",
        ] {
            fn contains(rows: &[Row], action: &str) -> bool {
                rows.iter()
                    .any(|r| r.action == action && r.enabled || contains(&r.children, action))
            }
            assert!(contains(&rows, action), "{action}");
        }
        assert!(
            !rows
                .iter()
                .find(|r| r.action == "synonyms")
                .unwrap()
                .enabled
        );
    }
    #[test]
    fn menu_ids_are_unique_across_submenus() {
        let rows = rows(&json!({}), &Config::default());
        fn ids(rows: &[Row], out: &mut Vec<i32>) {
            for row in rows {
                out.push(row.id);
                ids(&row.children, out)
            }
        }
        let mut all = Vec::new();
        ids(&rows, &mut all);
        let mut unique = all.clone();
        unique.sort();
        unique.dedup();
        assert_eq!(all.len(), unique.len());
    }
}

#[cfg(test)]
mod menu_lifecycle_tests {
    use super::*;
    #[test]
    fn nested_exported_menus_keep_captured_app_and_clicked_quit_needs_no_hide_notification() {
        let config = Config::default();
        let status = json!({"current_app":"fixture.original","current_domain":"","current_app_enabled":true});
        let state = Arc::new(Mutex::new(State {
            rows: rows(&status, &config),
            revision: 1,
            title: "Fixture".into(),
            status: "Active".into(),
            app: "fixture.original".into(),
            domain: String::new(),
            menu_intent: Default::default(),
            icons: Vec::new(),
            tracking: false,
            config,
            native_detail: String::new(),
        }));
        let (tx, rx) = mpsc::channel();
        let menu = Menu {
            state: state.clone(),
            sender: tx,
        };
        menu.open();
        state.lock().unwrap().app = "fixture.changed".into();
        menu.open();
        let id = state
            .lock()
            .unwrap()
            .rows
            .iter()
            .find(|r| r.action == "app_toggle")
            .unwrap()
            .id;
        menu.event(id, "clicked", OwnedValue::from(false), 0);
        assert!(!state.lock().unwrap().tracking);
        assert!(rx.try_iter().any(
            |a| matches!(a,UiAction::Command(Command::ToggleApp{app,..})if app=="fixture.original")
        ));
        let quit = state
            .lock()
            .unwrap()
            .rows
            .iter()
            .find(|r| r.action == "quit")
            .unwrap()
            .id;
        menu.open();
        menu.event(quit, "clicked", OwnedValue::from(false), 0);
        assert!(
            rx.try_iter()
                .any(|a| matches!(a,UiAction::Native(id)if id=="quit"))
        );
        assert!(!state.lock().unwrap().tracking);
    }
}
