use super::*;
use std::{cell::Cell, io::Read};
pub(super) struct ModelWindow {
    pub window: gtk::Window,
    catalog: gtk::ComboBoxText,
    current: gtk::Label,
    status: gtk::Label,
    progress: gtk::ProgressBar,
    download: gtk::Button,
    select: gtk::Button,
    cancel: gtk::Button,
    import: gtk::Button,
    inventory: Rc<RefCell<Vec<Value>>>,
    loading: Rc<Cell<bool>>,
    last_inventory: RefCell<Instant>,
}
pub(super) fn validate_import(path: &std::path::Path) -> Result<()> {
    anyhow::ensure!(path.is_file(), "Choose a regular local GGUF file.");
    let mut file = std::fs::File::open(path)?;
    let mut magic = [0; 4];
    file.read_exact(&mut magic)?;
    anyhow::ensure!(
        &magic == b"GGUF",
        "This file is not a GGUF model. Choose a valid .gguf file."
    );
    Ok(())
}
fn transfer_active(status: &Value) -> bool {
    matches!(
        status["state"].as_str(),
        Some("downloading" | "verifying" | "cancelling")
    )
}
fn size(bytes: u64) -> String {
    if bytes >= 1024 * 1024 * 1024 {
        format!("{:.2} GiB", bytes as f64 / (1024f64.powi(3)))
    } else {
        format!("{} MiB", bytes / (1024 * 1024))
    }
}
impl ModelWindow {
    pub fn new(sender: &Sender) -> Self {
        let window = gtk::Window::new(gtk::WindowType::Toplevel);
        window.set_title("BroTypist models");
        window.set_default_size(620, 460);
        let root = gtk::Box::new(gtk::Orientation::Vertical, 12);
        root.set_margin_start(20);
        root.set_margin_end(20);
        root.set_margin_top(20);
        root.set_margin_bottom(20);
        window.add(&root);
        root.pack_start(&label("Download verified local weights or reference your own GGUF file. Selecting a model does not run inference. Your writing is not uploaded."),false,false,0);
        let catalog = gtk::ComboBoxText::new();
        catalog.set_widget_name("available_local_models");
        root.pack_start(&catalog, false, false, 0);
        let description = label("");
        root.pack_start(&description, false, false, 0);
        let current = label("");
        root.pack_start(&current, false, false, 0);
        let progress = gtk::ProgressBar::new();
        progress.set_show_text(true);
        root.pack_start(&progress, false, false, 0);
        let status = label("");
        root.pack_start(&status, false, false, 0);
        let actions = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        let download = gtk::Button::with_label("Download…");
        let select = gtk::Button::with_label("Use installed model");
        let cancel = button(
            "Cancel download",
            sender,
            UiAction::Command(Command::CancelDownload),
        );
        for b in [&download, &select, &cancel] {
            actions.pack_start(b, false, false, 0);
        }
        root.pack_start(&actions, false, false, 0);
        let import = gtk::Button::with_label("Choose GGUF…");
        root.pack_start(&import, false, false, 0);
        let reveal = gtk::Button::with_label("Reveal models folder");
        reveal.connect_clicked(|_| {
            if let Err(e) = system::reveal_models() {
                error(None, e)
            }
        });
        root.pack_start(&reveal, false, false, 0);
        let close = gtk::Button::with_label("Close");
        let w = window.clone();
        close.connect_clicked(move |_| w.hide());
        root.pack_end(&close, false, false, 0);
        let inventory = Rc::new(RefCell::new(Vec::<Value>::new()));
        let loading = Rc::new(Cell::new(false));
        let items = inventory.clone();
        let details = description.clone();
        catalog.connect_changed(move |c| {
            let id = c.active_id();
            if let Some(model) = items
                .borrow()
                .iter()
                .find(|m| m["id"].as_str() == id.as_deref())
            {
                details.set_text(&format!(
                    "{}\n\nLicense: {}\n{}\n{}",
                    text(model, "description"),
                    text(model, "license_name"),
                    text(model, "license_url"),
                    text(model, "availability")
                ));
            }
        });
        let items = inventory.clone();
        let c = catalog.clone();
        let tx = sender.clone();
        let parent = window.clone();
        download.connect_clicked(move |_|{
            let selected=c.active_id();let model=items.borrow().iter().find(|m|m["id"].as_str()==selected.as_deref()).cloned();
            if let Some(model)=model {if !text(&model,"availability").is_empty(){return;}let required=model["requires_license_acceptance"]==true;
                let message=format!("Download {} ({})? Only model weights are downloaded.{}",text(&model,"label"),size(model["size_bytes"].as_u64().unwrap_or(0)),if required{format!("\n\nRead and accept {} before downloading:\n{}\n\nContinue accepts these terms for this download only.",text(&model,"license_name"),text(&model,"license_url"))}else{String::new()});
                if confirm(Some(&parent),"Download local model?",&message){let _=tx.send(UiAction::Command(Command::DownloadModel{id:text(&model,"id"),accept_license:required}));}
            }
        });
        let items = inventory.clone();
        let c = catalog.clone();
        let tx = sender.clone();
        select.connect_clicked(move |_| {
            let id = c.active_id();
            if let Some(m) = items
                .borrow()
                .iter()
                .find(|m| m["id"].as_str() == id.as_deref())
                .filter(|m| m["installed"] == true && text(m, "availability").is_empty())
            {
                let _ = tx.send(UiAction::Command(Command::SetModel {
                    path: text(m, "path"),
                    prompt_style: text(m, "prompt_style"),
                    model_id: Some(text(m, "id")),
                }));
            }
        });
        let tx = sender.clone();
        let parent = window.clone();
        import.connect_clicked(move |_| {
            let _modal = ModalGuard::enter();
            let chooser = gtk::FileChooserDialog::new(
                Some("Choose local GGUF model"),
                Some(&parent),
                gtk::FileChooserAction::Open,
            );
            chooser.add_buttons(&[
                ("Cancel", gtk::ResponseType::Cancel),
                ("Choose", gtk::ResponseType::Accept),
            ]);
            let filter = gtk::FileFilter::new();
            filter.set_name(Some("GGUF models"));
            filter.add_pattern("*.gguf");
            chooser.add_filter(filter);
            if chooser.run() == gtk::ResponseType::Accept {
                if let Some(path) = chooser.filename() {
                    match validate_import(&path).and_then(|_| {
                        path.to_str()
                            .map(str::to_owned)
                            .context("The model filename must be valid UTF-8.")
                    }) {
                        Ok(path) => {
                            let _ = tx.send(UiAction::Command(Command::SetModel {
                                path,
                                prompt_style: "continuation".into(),
                                model_id: None,
                            }));
                        }
                        Err(e) => error(Some(&parent), e),
                    }
                }
            }
            chooser.close();
        });
        window.connect_delete_event(|w, _| {
            w.hide();
            glib::Propagation::Stop
        });
        Self {
            window,
            catalog,
            current,
            status,
            progress,
            download,
            select,
            cancel,
            import,
            inventory,
            loading,
            last_inventory: RefCell::new(Instant::now() - Duration::from_secs(2)),
        }
    }
    pub fn show(&self) {
        self.window.show_all();
        self.window.present();
        *self.last_inventory.borrow_mut() = Instant::now() - Duration::from_secs(2);
    }
    pub fn refresh(&self, status: &Value, config: &Config) {
        if !self.window.is_visible() {
            return;
        }
        if self.last_inventory.borrow().elapsed() >= Duration::from_secs(1) {
            *self.last_inventory.borrow_mut() = Instant::now();
            match crate::runtime::model_inventory() {
                Ok(items) => {
                    if *self.inventory.borrow() != items {
                        let selected = self.catalog.active_id();
                        self.loading.set(true);
                        self.catalog.remove_all();
                        for m in &items {
                            self.catalog.append(
                                m["id"].as_str(),
                                &format!(
                                    "{} — {}",
                                    text(m, "label"),
                                    size(m["size_bytes"].as_u64().unwrap_or(0))
                                ),
                            );
                        }
                        *self.inventory.borrow_mut() = items;
                        self.catalog.set_active_id(selected.as_deref());
                        if self.catalog.active_id().is_none() {
                            self.catalog.set_active(Some(0));
                        }
                        self.loading.set(false);
                    }
                }
                Err(e) => {
                    self.status.set_text(&format!("Inventory unavailable: {e}. Check model-folder permissions and reopen Models."));
                    self.download.set_sensitive(false);
                    self.select.set_sensitive(false);
                    return;
                }
            }
        }
        let transfer = &status["download"];
        let active = transfer_active(transfer);
        let id = self.catalog.active_id();
        let items = self.inventory.borrow();
        let selected = items.iter().find(|m| m["id"].as_str() == id.as_deref());
        let available = selected.is_some_and(|m| text(m, "availability").is_empty());
        self.download.set_sensitive(available && !active);
        self.select.set_sensitive(
            available && !active && selected.is_some_and(|m| m["installed"] == true),
        );
        self.cancel.set_sensitive(active);
        self.import.set_sensitive(!active);
        self.catalog.set_sensitive(!active);
        self.current.set_text(&format!(
            "Selected model: {}",
            if config.backend == "gguf" {
                &config.model_path
            } else {
                &config.model
            }
        ));
        let total = transfer["total"].as_u64().unwrap_or(0);
        let received = transfer["received"].as_u64().unwrap_or(0);
        self.progress.set_fraction(if total > 0 {
            (received as f64 / total as f64).clamp(0., 1.)
        } else {
            0.
        });
        self.progress
            .set_text(Some(&format!("{} / {}", size(received), size(total))));
        if active && total == 0 {
            self.progress.pulse();
        }
        let message = text(transfer, "error");
        self.status.set_text(if !message.is_empty() {
            &message
        } else if active {
            "Downloading verified weights; cancellation preserves previous weights."
        } else if selected.is_some_and(|m| m["installed"] == true) {
            "Installed. Choose Use installed model to select it; no inference is performed here."
        } else {
            "No download is running. Download or choose a local GGUF explicitly."
        });
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn only_transfer_states_disable_catalog() {
        for state in ["downloading", "verifying", "cancelling"] {
            assert!(transfer_active(&json!({"state":state})));
        }
        for state in ["complete", "cancelled", "error", ""] {
            assert!(!transfer_active(&json!({"state":state})));
        }
    }
    #[test]
    fn import_checks_actual_magic_without_copying_model() {
        use std::io::Write;
        let mut file = tempfile::NamedTempFile::new().unwrap();
        file.write_all(b"GGUFsynthetic-header").unwrap();
        assert!(validate_import(file.path()).is_ok());
        let bad = tempfile::NamedTempFile::new().unwrap();
        assert!(validate_import(bad.path()).is_err());
    }
}
