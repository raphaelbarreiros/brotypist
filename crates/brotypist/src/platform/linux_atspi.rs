//! Uncached AT-SPI wire requests. Privacy gates execute before any Text request.
use crate::core::Rect;
use anyhow::{Context, Result, bail};
use std::{collections::BTreeMap, time::Duration};
use zbus::{
    blocking::{Connection, Proxy, connection::Builder},
    zvariant::OwnedObjectPath,
};

pub(super) const ACTIVE: u32 = 1;
pub(super) const DEFUNCT: u32 = 6;
pub(super) const EDITABLE: u32 = 7;
pub(super) const ENABLED: u32 = 8;
pub(super) const FOCUSED: u32 = 12;
pub(super) const MULTILINE: u32 = 17;
pub(super) const SENSITIVE: u32 = 24;
pub(super) const SHOWING: u32 = 25;
pub(super) const VISIBLE: u32 = 30;
pub(super) const LABEL: u32 = 29;
pub(super) const PASSWORD: u32 = 40;
pub(super) const TEXT: u32 = 61;
pub(super) const ENTRY: u32 = 79;
pub(super) type Attributes = BTreeMap<String, String>;
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Object {
    pub bus: String,
    pub path: OwnedObjectPath,
}
impl Ord for Object {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.bus
            .cmp(&other.bus)
            .then_with(|| self.path.as_str().cmp(other.path.as_str()))
    }
}
impl PartialOrd for Object {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}
impl Object {
    pub fn valid(&self) -> bool {
        !self.bus.is_empty() && self.path.as_str() != "/org/a11y/atspi/null"
    }
    pub fn identity(&self, pid: i32) -> String {
        format!("{pid}:{}:{}", self.bus, self.path)
    }
}
#[derive(Clone, Debug)]
pub(super) struct Metadata {
    pub role: u32,
    pub states: Vec<u32>,
    pub attrs: Attributes,
}
impl Metadata {
    pub fn has(&self, state: u32) -> bool {
        self.states
            .get((state / 32) as usize)
            .is_some_and(|v| v & (1 << (state % 32)) != 0)
    }
    pub fn protected(&self) -> bool {
        self.role == PASSWORD
            || self.attrs.iter().any(|(k, v)| {
                let k = crate::core::casefold(k);
                let v = crate::core::casefold(v);
                ((k.contains("password") || k.contains("protected"))
                    && !matches!(v.as_str(), "false" | "0" | "no"))
                    || v == "password"
            })
    }
    pub fn composing(&self) -> bool {
        self.attrs.iter().any(|(k, v)| {
            matches!(
                crate::core::casefold(k).as_str(),
                "preedit" | "composing" | "composition-active"
            ) && !matches!(crate::core::casefold(v).as_str(), "false" | "0" | "no" | "")
        })
    }
    pub fn inactive_composition(&self) -> bool {
        self.attrs.iter().any(|(k, v)| {
            matches!(
                crate::core::casefold(k).as_str(),
                "preedit" | "composing" | "composition-active"
            ) && matches!(crate::core::casefold(v).as_str(), "false" | "0" | "no")
        })
    }
    pub fn editable(&self) -> bool {
        matches!(self.role, TEXT | ENTRY)
            && !self.protected()
            && !self.composing()
            && [FOCUSED, EDITABLE, ENABLED, SENSITIVE]
                .iter()
                .all(|s| self.has(*s))
    }
}
#[derive(Clone)]
pub(super) struct TextRead {
    pub count: i32,
    pub caret: i32,
    pub selections: Vec<(i32, i32)>,
    pub text: String,
}
/// Injectable protocol boundary, not a second native implementation.
pub(super) trait Accessibility {
    fn root(&self) -> Object;
    fn children(&self, o: &Object, limit: usize) -> Result<Vec<Object>>;
    fn metadata(&self, o: &Object) -> Result<Metadata>;
    fn parent(&self, o: &Object) -> Result<Option<Object>>;
    fn app_identity(&self, o: &Object) -> Result<(i32, String, String)>;
    fn text(&self, o: &Object, reject_selection: bool) -> Result<Option<TextRead>>;
    fn static_text(&self, o: &Object, max: usize) -> Result<String>;
    fn extents(&self, o: &Object) -> Result<Rect>;
    fn character_rect(&self, o: &Object, index: usize) -> Result<Rect>;
    fn attribute_run(&self, o: &Object, index: usize) -> Result<(Attributes, i32, i32)>;
    fn insert(&self, o: &Object, caret: i32, text: &str, bytes: i32) -> Result<bool>;
    fn set_caret(&self, o: &Object, caret: i32) -> Result<bool>;
}
pub(super) struct Atspi {
    connection: Connection,
    root: Object,
}
impl Atspi {
    pub fn connect() -> Result<Self> {
        let session = Builder::session()?
            .method_timeout(Duration::from_millis(500))
            .build()?;
        let bus = Proxy::new(&session, "org.a11y.Bus", "/org/a11y/bus", "org.a11y.Bus")?;
        let address: String = bus
            .call("GetAddress", &())
            .context("AT-SPI accessibility bus unavailable; enable session accessibility")?;
        let connection = Builder::address(address.as_str())?
            .method_timeout(Duration::from_millis(200))
            .build()?;
        let root = Object {
            bus: "org.a11y.atspi.Registry".into(),
            path: OwnedObjectPath::try_from("/org/a11y/atspi/accessible/root")?,
        };
        let this = Self { connection, root };
        let _: i32 = this
            .proxy(&this.root, "Accessible")?
            .get_property("ChildCount")?;
        Ok(this)
    }
    fn proxy<'a>(&'a self, o: &'a Object, interface: &str) -> Result<Proxy<'a>> {
        // Disable property caching: each read proves this native revision, not a prior focus.
        Ok(zbus::blocking::proxy::Builder::new(&self.connection)
            .destination(o.bus.as_str())?
            .path(o.path.as_str())?
            .interface(format!("org.a11y.atspi.{interface}"))?
            .cache_properties(zbus::proxy::CacheProperties::No)
            .build()?)
    }
    fn reference(value: (String, OwnedObjectPath)) -> Object {
        Object {
            bus: value.0,
            path: value.1,
        }
    }
}
impl Accessibility for Atspi {
    fn root(&self) -> Object {
        self.root.clone()
    }
    fn children(&self, o: &Object, limit: usize) -> Result<Vec<Object>> {
        let p = self.proxy(o, "Accessible")?;
        let count: i32 = p.get_property("ChildCount")?;
        let mut children = Vec::new();
        for index in 0..(count.max(0) as usize).min(limit) {
            if let Ok(reference) =
                p.call::<_, _, (String, OwnedObjectPath)>("GetChildAtIndex", &(index as i32,))
            {
                let child = Self::reference(reference);
                if child.valid() {
                    children.push(child);
                }
            }
        }
        Ok(children)
    }
    fn metadata(&self, o: &Object) -> Result<Metadata> {
        let p = self.proxy(o, "Accessible")?;
        Ok(Metadata {
            role: p.call("GetRole", &())?,
            states: p.call("GetState", &())?,
            attrs: p.call("GetAttributes", &())?,
        })
    }
    fn parent(&self, o: &Object) -> Result<Option<Object>> {
        let value = Self::reference(self.proxy(o, "Accessible")?.get_property("Parent")?);
        Ok(value.valid().then_some(value))
    }
    fn app_identity(&self, o: &Object) -> Result<(i32, String, String)> {
        let dbus = Proxy::new(
            &self.connection,
            "org.freedesktop.DBus",
            "/org/freedesktop/DBus",
            "org.freedesktop.DBus",
        )?;
        let pid: u32 = dbus.call("GetConnectionUnixProcessID", &(o.bus.as_str(),))?;
        if pid == 0 || pid > i32::MAX as u32 {
            bail!("Invalid AT-SPI process identity");
        }
        let name: String = self.proxy(o, "Accessible")?.get_property("Name")?;
        let executable = std::fs::read_link(format!("/proc/{pid}/exe"))
            .map(|v| v.to_string_lossy().into_owned())
            .unwrap_or_default();
        Ok((pid as i32, name, executable))
    }
    fn text(&self, o: &Object, reject_selection: bool) -> Result<Option<TextRead>> {
        // Requiring EditableText is checked without invoking an edit or obtaining text.
        let interfaces: Vec<String> = self.proxy(o, "Accessible")?.call("GetInterfaces", &())?;
        if !interfaces
            .iter()
            .any(|i| i == "org.a11y.atspi.EditableText")
        {
            return Ok(None);
        }
        let p = self.proxy(o, "Text")?;
        let count: i32 = p.get_property("CharacterCount")?;
        if !(0..=1_000_000).contains(&count) {
            return Ok(None);
        }
        let caret: i32 = p.get_property("CaretOffset")?;
        let n: i32 = p.call("GetNSelections", &())?;
        if !(0..=1).contains(&n) || (reject_selection && n != 0) {
            return Ok(None);
        }
        let selections = if n == 1 {
            vec![p.call("GetSelection", &(0i32,))?]
        } else {
            Vec::new()
        };
        Ok(Some(TextRead {
            count,
            caret,
            selections,
            text: p.call("GetText", &(0i32, -1i32))?,
        }))
    }
    fn static_text(&self, o: &Object, max: usize) -> Result<String> {
        match self
            .proxy(o, "Text")?
            .call("GetText", &(0i32, max.min(i32::MAX as usize) as i32))
        {
            Ok(value) => Ok(value),
            Err(_) => Ok(self
                .proxy(o, "Accessible")?
                .get_property::<String>("Name")?
                .chars()
                .take(max)
                .collect()),
        }
    }
    fn extents(&self, o: &Object) -> Result<Rect> {
        let (x, y, w, h): (i32, i32, i32, i32) =
            self.proxy(o, "Component")?.call("GetExtents", &(0u32,))?;
        Ok([x as f64, y as f64, w as f64, h as f64])
    }
    fn character_rect(&self, o: &Object, index: usize) -> Result<Rect> {
        let (x, y, w, h): (i32, i32, i32, i32) = self
            .proxy(o, "Text")?
            .call("GetCharacterExtents", &(i32::try_from(index)?, 0u32))?;
        Ok([x as f64, y as f64, w as f64, h as f64])
    }
    fn attribute_run(&self, o: &Object, index: usize) -> Result<(Attributes, i32, i32)> {
        Ok(self
            .proxy(o, "Text")?
            .call("GetAttributeRun", &(i32::try_from(index)?, true))?)
    }
    fn insert(&self, o: &Object, caret: i32, text: &str, bytes: i32) -> Result<bool> {
        Ok(self
            .proxy(o, "EditableText")?
            .call("InsertText", &(caret, text, bytes))?)
    }
    fn set_caret(&self, o: &Object, caret: i32) -> Result<bool> {
        Ok(self.proxy(o, "Text")?.call("SetCaretOffset", &(caret,))?)
    }
}
