use super::*;
use std::{cell::Cell, collections::BTreeMap};
#[derive(Clone)] enum Control { Check(gtk::CheckButton), Spin(gtk::SpinButton), Entry(gtk::Entry), Combo(gtk::ComboBoxText), Text(gtk::TextBuffer), Lines(gtk::TextBuffer) }
impl Control {
    fn write(&self,value:&Value){match self {
        Self::Check(c)=>{let v=value.as_bool().unwrap_or(false);if c.is_active()!=v{c.set_active(v);}}, Self::Spin(c)=>{let v=value.as_f64().unwrap_or(0.);if c.value()!=v{c.set_value(v);}},
        Self::Entry(c)=>{let v=value.as_str().unwrap_or("");if c.text().as_str()!=v{c.set_text(v);}}, Self::Combo(c)=>{let id=if let Some(s)=value.as_str(){s.to_owned()}else{value.to_string()};if c.active_id().as_deref()!=Some(id.as_str()){c.set_active_id(Some(&id));}},
        Self::Text(c)=>{let v=value.as_str().unwrap_or("");if c.text(&c.start_iter(),&c.end_iter(),false).as_deref()!=Some(v){c.set_text(v);}},
        Self::Lines(c)=>{let v=value.as_array().map(|a|a.iter().filter_map(Value::as_str).collect::<Vec<_>>().join("\n")).unwrap_or_default();if c.text(&c.start_iter(),&c.end_iter(),false).as_deref()!=Some(v.as_str()){c.set_text(&v);}},
    }}
}
pub(super) struct SettingsWindow {
    pub window:gtk::Window, pub draft:Rc<RefCell<SettingsDraft>>, pub notice:gtk::Label,
    stack:gtk::Stack, bindings:BTreeMap<String,Control>, loading:Rc<Cell<bool>>, recording:Rc<Cell<bool>>,
    save:gtk::Button,revert:gtk::Button,dirty:gtk::Label,login:gtk::CheckButton,history:gtk::CheckButton,
    rules:gtk::ListBox, status_label:gtk::Label,model:gtk::Label,context:gtk::Label,history_info:gtk::Label,
    stats:gtk::Label,stats_rows:gtk::ListBox,battery:gtk::Label,updates:gtk::Label,progress:gtk::ProgressBar,
    update_check:gtk::Button,update_download:gtk::Button,update_cancel:gtk::Button,update_install:gtk::Button,
    shortcut_buttons:BTreeMap<String,gtk::Button>,recovery:gtk::Label,sender:Sender,
    login_status:gtk::Label,
    statistics_query:Rc<RefCell<(String,String)>>,
}
fn section(page:&gtk::Box,title:&str)->gtk::Box {
    let frame=gtk::Frame::new(Some(title)); let body=gtk::Box::new(gtk::Orientation::Vertical,8);
    body.set_margin_start(14);body.set_margin_end(14);body.set_margin_top(14);body.set_margin_bottom(14);frame.add(&body);page.pack_start(&frame,false,false,4);body
}
fn row(parent:&gtk::Box,title:&str,widget:&impl IsA<gtk::Widget>){let row=gtk::Box::new(gtk::Orientation::Horizontal,12);let l=label(title);l.set_mnemonic_widget(Some(widget));l.set_hexpand(true);if let Some(a)=widget.as_ref().accessible(){a.set_name(title);}row.pack_start(&l,true,true,0);row.pack_start(widget,false,false,0);parent.pack_start(&row,false,false,0);}
fn text_view(parent:&gtk::Box,height:i32)->gtk::TextView { let v=gtk::TextView::new();v.set_wrap_mode(gtk::WrapMode::WordChar);let area=gtk::ScrolledWindow::new(None::<&gtk::Adjustment>,None::<&gtk::Adjustment>);area.set_min_content_height(height);area.add(&v);parent.pack_start(&area,false,true,0);v }
impl SettingsWindow {
    pub fn new(config:&Config,sender:&Sender)->Result<Self>{
        let window=gtk::Window::new(gtk::WindowType::Toplevel);window.set_title("BroTypist Settings");window.set_default_size(960,700);window.set_size_request(900,620);
        let root=gtk::Box::new(gtk::Orientation::Vertical,10);window.add(&root);
        let columns=gtk::Box::new(gtk::Orientation::Horizontal,16);root.pack_start(&columns,true,true,0);
        let stack=gtk::Stack::new();stack.set_hexpand(true);stack.set_vexpand(true);
        let sidebar=gtk::StackSidebar::new();sidebar.set_stack(&stack);sidebar.set_size_request(185,-1);columns.pack_start(&sidebar,false,false,0);columns.pack_start(&stack,true,true,0);
        let mut pages=BTreeMap::new();
        for name in PANES {let area=gtk::ScrolledWindow::new(None::<&gtk::Adjustment>,None::<&gtk::Adjustment>);area.set_policy(gtk::PolicyType::Never,gtk::PolicyType::Automatic);let page=gtk::Box::new(gtk::Orientation::Vertical,12);page.set_margin_start(12);page.set_margin_end(24);page.set_margin_top(24);page.set_margin_bottom(24);page.pack_start(&label(name),false,false,0);area.add(&page);stack.add_titled(&area,name,name);pages.insert(name,page);}
        let draft=Rc::new(RefCell::new(SettingsDraft::new(config)?));let loading=Rc::new(Cell::new(false));let recording=Rc::new(Cell::new(false));
        let notice=label("");root.pack_start(&notice,false,false,0);let recovery=label("");root.pack_start(&recovery,false,false,0);
        let footer=gtk::Box::new(gtk::Orientation::Horizontal,8);footer.set_margin_start(12);footer.set_margin_end(12);footer.set_margin_bottom(12);
        let dirty=label("Changes apply only after Save.");footer.pack_start(&dirty,true,true,0);
        let save=button("Save",sender,UiAction::Save);let revert=button("Revert",sender,UiAction::Revert);
        for b in [button("Quit BroTypist",sender,UiAction::Quit),revert.clone(),save.clone(),button("Close window",sender,UiAction::CloseSettings)]{footer.pack_start(&b,false,false,0);}root.pack_end(&footer,false,false,0);
        let login=gtk::CheckButton::with_label("Launch BroTypist at login (applied immediately)");let history=gtk::CheckButton::with_label("Learn locally from recorded writing by default (applied immediately)");
        let login_status=label("");
        let status_label=label("");let model=label("");let context=label("");let history_info=label("");let stats=label("");let stats_rows=gtk::ListBox::new();let battery=label("");let updates=label("");let progress=gtk::ProgressBar::new();progress.set_show_text(true);
        let update_check=button("Check for Updates…",sender,UiAction::UpdateCheck);let update_download=button("Download Update",sender,UiAction::UpdateDownload);let update_cancel=button("Cancel",sender,UiAction::UpdateCancel);let update_install=button("Install & Relaunch",sender,UiAction::UpdateInstall);
        let rules=gtk::ListBox::new();rules.set_selection_mode(gtk::SelectionMode::Single);
        let mut s=Self{window,draft,notice,stack,bindings:BTreeMap::new(),loading,recording,save,revert,dirty,login,history,rules,status_label,model,context,history_info,stats,stats_rows,battery,updates,progress,update_check,update_download,update_cancel,update_install,shortcut_buttons:BTreeMap::new(),recovery,sender:sender.clone(),statistics_query:Rc::new(RefCell::new(("week".into(),"day".into()))),login_status};
        let p=&pages["General"];let app=section(p,"Application");app.pack_start(&s.login,false,false,0);app.pack_start(&label("Close window keeps BroTypist resident only while a tray host is available. Quit always stops assistance and leaves KDE's selected input method unchanged."),false,false,0);
        app.pack_start(&s.login_status,false,false,0);
        s.check(&app,"global_enabled","Enable predictions","");s.check(&app,"enabled","Enable in eligible apps by default","Application rules may override this default.");s.check(&app,"show_accessory","Show accessory beside the text field","");
        let stats_section=section(p,"Local aggregate statistics");s.check(&stats_section,"statistics_enabled","Count successfully accepted completions","Aggregate counts only, never draft text.");
        let period=gtk::ComboBoxText::new();for(id,title)in[("day","Today"),("week","Past week"),("month","Past month"),("all","All time")]{period.append(Some(id),title);}period.set_active_id(Some("week"));row(&stats_section,"Period",&period);
        let group=gtk::ComboBoxText::new();group.append(Some("day"),"By day");group.append(Some("app"),"By application");group.set_active_id(Some("day"));row(&stats_section,"Group",&group);
        for source in [period.clone(),group.clone()]{let tx=sender.clone();let a=period.clone();let b=group.clone();let query=s.statistics_query.clone();source.connect_changed(move |_|{let period=a.active_id().unwrap_or_default().to_string();let group=b.active_id().unwrap_or_default().to_string();*query.borrow_mut()=(period.clone(),group.clone());let _=tx.send(UiAction::Statistics(period,group));});}
        stats_section.pack_start(&s.stats,false,false,0);stats_section.pack_start(&s.stats_rows,false,false,0);stats_section.pack_start(&button("Clear statistics…",sender,UiAction::ClearStatistics),false,false,0);
        let p=&pages["AI Models"];let selected=section(p,"Current local model");selected.pack_start(&s.model,false,false,0);selected.pack_start(&button("Manage local models…",sender,UiAction::Models),false,false,0);let reveal=gtk::Button::with_label("Reveal model files");reveal.connect_clicked(|_|if let Err(e)=system::reveal_models(){error(None,e)});selected.pack_start(&reveal,false,false,0);
        s.combo(&selected,"prompt_style","Model prompt style",&[("continuation","Text continuation"),("writing","Instruction-tuned writing")]);
        let inference=section(p,"Local inference configuration");s.combo(&inference,"backend","Backend",&[("gguf","Local GGUF process"),("ollama","Local Ollama server"),("openai","Local OpenAI-compatible server")]);s.entry(&inference,"endpoint","Loopback server URL");s.entry(&inference,"model","Local server model name");s.combo(&inference,"acceleration","Acceleration",&[("auto","Automatic"),("cpu","CPU only")]);s.spin(&inference,"max_tokens","Maximum generated tokens",1.,128.);inference.pack_start(&label("Server endpoints must be literal loopback URLs. Externally managed servers own their logging and model residency. No remote API substitute."),false,false,0);
        let power=section(p,"While on battery");s.check(&power,"battery_on_demand","Generate only when requested","");let mut choices=vec![("same".to_owned(),"Same as usual".to_owned())];choices.extend(crate::models::presets().iter().map(|p|(p.id.clone(),p.label.clone())));s.combo_owned(&power,"battery_model","Battery model",&choices);s.check(&power,"battery_shorter","Prefer shorter completions","Alternate models must already be installed. Battery mode never downloads one.");power.pack_start(&s.battery,false,false,0);s.spin(&power,"idle_release_seconds","Release model after inactivity (seconds; 0 keeps ready)",0.,3600.);
        let p=&pages["Autocomplete"];let suggest=section(p,"Suggestions");s.check(&suggest,"complete_inside_text","Complete inside existing text","");s.combo(&suggest,"completion_length","Maximum completion length",&[("word","Current word only"),("short","Short"),("medium","Medium"),("long","Long")]);s.spin(&suggest,"debounce_ms","Typing pause (ms)",100.,5000.);s.spin(&suggest,"min_chars","Minimum typed characters",1.,1000.);
        let shortcuts=section(p,"Keyboard shortcuts");
        for(action,title)in[("word","Accept next word"),("all","Accept full completion"),("force","Request completion now"),("app_toggle","Toggle current app this session"),("toggle","Toggle globally"),("alternatives","Alternative words"),("synonyms","Selected-text synonyms")]{s.shortcut(&shortcuts,action,title);}
        shortcuts.pack_start(&label("Click then press the actual chord. Escape cancels; Backspace clears. Physical Above-Tab works across layouts. Desktop-reserved chords remain system-owned. Linux selected-text replacement is unavailable."),false,false,0);
        s.combo(&shortcuts,"escape_behavior","Escape action",&[("pause","Pause briefly"),("app","Disable current application"),("passthrough","Pass through")]);s.spin(&shortcuts,"escape_pause_seconds","Brief pause (seconds)",1.,3600.);s.check(&shortcuts,"trailing_space","Include trailing space when accepting","");s.combo(&shortcuts,"punctuation_mode","Trailing punctuation",&[("full","Only with full completion"),("always","With either shortcut"),("never","Never")]);
        let emoji=section(p,"Emoji preferences (not available on Linux)");s.check(&emoji,"emoji_enabled","Find emoji by :name","");s.check(&emoji,"emoticons_enabled","Replace emoticons","");s.combo(&emoji,"emoji_skin_tone","Preferred skin tone",&[("0","Default"),("1","Light"),("2","Medium-light"),("3","Medium"),("4","Medium-dark"),("5","Dark")]);s.check(&emoji,"emoji_include_neutral","Include neutral-tone alternatives","");s.combo(&emoji,"emoji_gender","Preferred gender",&[("neutral","Neutral"),("female","Female"),("male","Male")]);emoji.set_sensitive(false);p.pack_start(&label("AT-SPI cannot atomically replace a typed suffix. Emoji and emoticon preferences are preserved but native rewrites remain disabled."),false,false,0);
        let alternatives=section(p,"Alternative words");s.check(&alternatives,"alternatives_enabled","Offer alternative continuations","");s.combo(&alternatives,"alternatives_mode","Show alternatives",&[("shortcut","With shortcut"),("pause","After a pause")]);s.spin(&alternatives,"alternatives_delay_ms","Pause delay (ms)",100.,10000.);
        let synonyms=section(p,"Contextual synonyms (not available on Linux)");s.check(&synonyms,"synonyms_enabled","Offer synonyms for selected text","");s.combo(&synonyms,"synonyms_mode","Show synonyms",&[("selection","When selected"),("shortcut","With shortcut")]);s.spin(&synonyms,"synonyms_delay_ms","Selection delay (ms)",100.,10000.);synonyms.set_sensitive(false);
        let p=&pages["Languages"];p.pack_start(&label("Language support comes from your selected local model, not a language-quality selector. Instructions can request your preferred language; output quality depends on the model."),false,false,0);let instructions=section(p,"Custom instructions");s.multiline(&instructions,"custom_instructions",false);let spelling=section(p,"Spelling");s.check(&spelling,"typo_suppression","Suppress completion for likely typos","Requires an available native dictionary. Unavailable languages never silently become English.");s.check(&spelling,"typo_fixes","Offer typo fixes","Native Linux replacement is unavailable; no delete-then-insert rewrite is attempted.");spelling.set_sensitive(false);
        let p=&pages["Privacy"];p.pack_start(&label("Predictions run on this computer. Optional local servers must use loopback URLs. Downloads fetch weights, not writing. Every additional data source requires independent consent."),false,false,0);
        let access=section(p,"System access");access.pack_start(&s.status_label,false,false,0);
        for(title,kind)in[("Open accessibility / input permissions","accessibility")]{let b=gtk::Button::with_label(title);b.connect_clicked(move |_|if let Err(e)=system::open_permissions(kind){error(None,e)});access.pack_start(&b,false,false,0);}
        access.pack_start(&label("X11 uses AT-SPI and replayable grabs. KWin Wayland requires explicitly choosing BroTypist as the private input method. Manual controls only: ordinary inline/preedit is unavailable, no popup fallback. Other compositors are not claimed supported."),false,false,0);
        let install=gtk::Button::with_label("Install KDE input-method entry…");let parent=s.window.clone();install.connect_clicked(move |_|{if confirm(Some(&parent),"Install KDE entry?","Create the owned KDE input-method desktop entry? This does not change the selected input method. Quit BroTypist and select it in Virtual Keyboard settings afterward."){match system::install_wayland_input_method(){Ok(path)=>error(Some(&parent),format!("Installed {}. Choose BroTypist explicitly in KDE Virtual Keyboard settings. Ordinary inline remains unavailable.",path.display())),Err(e)=>error(Some(&parent),e)}}});access.pack_start(&install,false,false,0);
        let open=gtk::Button::with_label("Open KDE Virtual Keyboard settings");open.connect_clicked(|_|if let Err(e)=system::open_wayland_settings(){error(None,e)});access.pack_start(&open,false,false,0);access.pack_start(&button("Choose or download a local model…",sender,UiAction::Models),false,false,0);s.check(&access,"setup_completed","Setup completed","Review access and select a model first. No permission is automatically granted.");
        let context=section(p,"Optional local context");s.check(&context,"clipboard_context","Include clipboard text","Read only for an eligible completion. Clipboard content is untrusted reference data.");s.check(&context,"screen_context","Include focused-window Accessibility text","Only visible, unprotected controls in the active native window. Unsupported providers return no context.");s.spin(&context,"context_chars","Maximum typed context (characters)",64.,8000.);context.pack_start(&s.context,false,false,0);
        let unsupported=section(p,"macOS-specific sources (unavailable on Linux)");for(field,title)in[("screenshot_context","Screenshot / OCR context"),("screenshot_appearance","Screenshot appearance sampling"),("terminal_completion","Special terminal completion")]{s.check(&unsupported,field,title,"Retained independently; never activated by a Linux UI fallback.");}unsupported.set_sensitive(false);
        let history=section(p,"Encrypted local writing learning");history.pack_start(&s.history,false,false,0);history.pack_start(&label("History is off by default and requires OS-backed encryption; no plaintext fallback. App recording overrides take precedence even when this default is off. Learning uses bounded authored style/vocabulary, not raw sentence replay or weight training."),false,false,0);s.check(&history,"record_unaccepted","Also learn when no suggestion was accepted","Only when that app/site has history enabled.");s.spin(&history,"personalization_strength","Style and vocabulary strength (%)",0.,100.);history.pack_start(&s.history_info,false,false,0);history.pack_start(&button("Refresh encrypted history information",sender,UiAction::Command(Command::HistoryInfo)),false,false,0);history.pack_start(&button("Unlock and count recorded writing…",sender,UiAction::Command(Command::HistoryCounts{unlock:true})),false,false,0);history.pack_start(&button("Forget all learned writing…",sender,UiAction::DeleteHistory(None)),false,false,0);
        let rules=section(p,"Application and website rules");rules.pack_start(&label("Exact app identifiers only. Browser URLs are unavailable here. Existing website exclusions fail closed for recognized browsers with unknown URLs; editing them does not imply per-site support. Rule changes remain staged until Save."),false,false,0);rules.pack_start(&s.rules,false,false,0);let actions=gtk::Box::new(gtk::Orientation::Horizontal,8);
        for(title,edit)in[("Add…",false),("Edit…",true)]{let b=gtk::Button::with_label(title);let list=s.rules.clone();let draft=s.draft.clone();let window=s.window.clone();b.connect_clicked(move |_|{
            let key=if edit{selected_rule(&list)}else{None};if edit&&key.is_none(){return;}
            let initial={let current=draft.borrow();key.as_deref().and_then(|key|current.value("app_rules").and_then(|rules|rules.get(key))).cloned().unwrap_or(json!({}))};
            if let Some((new_key,rule))=rule_dialog(&window,key.as_deref().unwrap_or(""),&initial){
                let all=draft.borrow().value("app_rules").and_then(Value::as_object).cloned().unwrap_or_default();
                match stage_rule(all,key.as_deref(),new_key,&initial,rule){
                    Ok(all)=>{let _=draft.borrow_mut().set("app_rules",Value::Object(all));refresh_rules(&list,&draft.borrow());},
                    Err(e)=>error(Some(&window),e),
                }
            }
        });actions.pack_start(&b,false,false,0);}
        let remove=gtk::Button::with_label("Remove");let list=s.rules.clone();let draft=s.draft.clone();remove.connect_clicked(move |_|{if let Some(k)=selected_rule(&list){let mut rules=draft.borrow().value("app_rules").and_then(Value::as_object).cloned().unwrap_or_default();rules.remove(&k);let _=draft.borrow_mut().set("app_rules",Value::Object(rules));refresh_rules(&list,&draft.borrow());}});actions.pack_start(&remove,false,false,0);
        let delete=gtk::Button::with_label("Delete app history…");let list=s.rules.clone();let tx=sender.clone();delete.connect_clicked(move |_|if let Some(k)=selected_rule(&list){let _=tx.send(UiAction::RuleHistory(k));});actions.pack_start(&delete,false,false,0);rules.pack_start(&actions,false,false,0);rules.pack_start(&label("Excluded applications: one exact identifier per line"),false,false,0);s.multiline(&rules,"excluded_apps",true);
        let p=&pages["About"];p.pack_start(&app_image(88)?,false,false,0);p.pack_start(&label(&format!("BroTypist {} · Linux (experimental)",env!("CARGO_PKG_VERSION"))),false,false,0);p.pack_start(&label("An independent local typing assistant. No account, cloud sync, subscription or main writing editor. Models retain their respective licenses. Unsupported or protected fields stay inactive."),false,false,0);
        p.pack_start(&gtk::LinkButton::with_label("https://github.com/renan-brasilio/brotypist","Source repository"),false,false,0);p.pack_start(&button("Show Welcome…",sender,UiAction::Welcome),false,false,0);
        let folder=gtk::Button::with_label("Open settings folder");folder.connect_clicked(|_|{let result=crate::paths::config_dir().and_then(|p|{gio::AppInfo::launch_default_for_uri(gio::File::for_path(p).uri().as_str(),None::<&gio::AppLaunchContext>).map_err(Into::into)});if let Err(e)=result{error(None,e)}});p.pack_start(&folder,false,false,0);
        let updates=section(p,"Updates");s.check(&updates,"check_updates_automatically","Check automatically once a day","Downloads and installation always need explicit approval.");updates.pack_start(&s.updates,false,false,0);updates.pack_start(&s.progress,false,false,0);let buttons=gtk::Box::new(gtk::Orientation::Horizontal,8);for b in [&s.update_check,&s.update_download,&s.update_cancel,&s.update_install]{buttons.pack_start(b,false,false,0);}updates.pack_start(&buttons,false,false,0);
        let tx=sender.clone();s.window.connect_delete_event(move |_,_|{let _=tx.send(UiAction::CloseSettings);glib::Propagation::Stop});
        let tx=sender.clone();let loading=s.loading.clone();s.login.connect_toggled(move |c|if !loading.get(){let _=tx.send(UiAction::Login(c.is_active()));});let tx=sender.clone();let loading=s.loading.clone();s.history.connect_toggled(move |c|if !loading.get(){let _=tx.send(UiAction::History(c.is_active()));});
        let tx=sender.clone();let stack=s.stack.clone();s.window.connect_key_press_event(move |_,e|{if e.state().contains(gdk::ModifierType::CONTROL_MASK){let key=e.keyval();if key==gdk::keys::constants::q{let _=tx.send(UiAction::Quit);return glib::Propagation::Stop;}if let Some(c)=key.to_unicode(){if let Some(n)=c.to_digit(10).filter(|n|(1..=6).contains(n)){stack.set_visible_child_name(PANES[n as usize-1]);return glib::Propagation::Stop;}}}glib::Propagation::Proceed});
        s.reset(config)?;Ok(s)
    }
    fn changed(draft:&Rc<RefCell<SettingsDraft>>,loading:&Rc<Cell<bool>>,field:&str,value:Value){if !loading.get(){let _=draft.borrow_mut().set(field,value);}}
    fn check(&mut self,parent:&gtk::Box,field:&str,title:&str,help:&str){let c=gtk::CheckButton::with_label(title);c.set_widget_name(field);c.set_tooltip_text(Some(help));parent.pack_start(&c,false,false,0);if !help.is_empty(){parent.pack_start(&label(help),false,false,0);}let d=self.draft.clone();let l=self.loading.clone();let f=field.to_owned();let parent=self.window.clone();c.connect_toggled(move |c|{if !l.get()&&c.is_active()&&matches!(f.as_str(),"clipboard_context"|"screen_context"|"screenshot_context"|"screenshot_appearance"){if !confirm(Some(&parent),"Allow optional local context?",&format!("Enable {}? Read only for eligible requests, never saved as an image or uploaded. You can revoke this independent consent at any time.",f.replace('_'," "))){l.set(true);c.set_active(false);l.set(false);return;}}Self::changed(&d,&l,&f,json!(c.is_active()));});self.bindings.insert(field.into(),Control::Check(c));}
    fn spin(&mut self,parent:&gtk::Box,field:&str,title:&str,low:f64,high:f64){let c=gtk::SpinButton::with_range(low,high,1.);c.set_widget_name(field);row(parent,title,&c);let d=self.draft.clone();let l=self.loading.clone();let f=field.to_owned();c.connect_value_changed(move |c|Self::changed(&d,&l,&f,json!(c.value_as_int())));self.bindings.insert(field.into(),Control::Spin(c));}
    fn entry(&mut self,parent:&gtk::Box,field:&str,title:&str){let c=gtk::Entry::new();c.set_width_chars(32);c.set_widget_name(field);row(parent,title,&c);let d=self.draft.clone();let l=self.loading.clone();let f=field.to_owned();c.connect_changed(move |c|Self::changed(&d,&l,&f,json!(c.text().as_str())));self.bindings.insert(field.into(),Control::Entry(c));}
    fn combo(&mut self,parent:&gtk::Box,field:&str,title:&str,choices:&[(&str,&str)]){self.combo_owned(parent,field,title,&choices.iter().map(|(a,b)|(a.to_string(),b.to_string())).collect::<Vec<_>>());}
    fn combo_owned(&mut self,parent:&gtk::Box,field:&str,title:&str,choices:&[(String,String)]){let c=gtk::ComboBoxText::new();c.set_widget_name(field);for(id,title)in choices{c.append(Some(id),title);}row(parent,title,&c);let d=self.draft.clone();let l=self.loading.clone();let f=field.to_owned();c.connect_changed(move |c|{if let Some(id)=c.active_id(){let v=if f=="emoji_skin_tone"{json!(id.parse::<u32>().unwrap_or(0))}else{json!(id.as_str())};Self::changed(&d,&l,&f,v);}});self.bindings.insert(field.into(),Control::Combo(c));}
    fn multiline(&mut self,parent:&gtk::Box,field:&str,lines:bool){let v=text_view(parent,120);v.set_widget_name(field);if let Some(a)=v.accessible(){a.set_name(&field.replace('_'," "));}let c=v.buffer().expect("TextView buffer");let d=self.draft.clone();let l=self.loading.clone();let f=field.to_owned();c.connect_changed(move |c|{let t=c.text(&c.start_iter(),&c.end_iter(),false).unwrap_or_default();let v=if lines{json!(t.lines().map(str::trim).filter(|s|!s.is_empty()).collect::<Vec<_>>())}else{json!(t.as_str())};Self::changed(&d,&l,&f,v);});self.bindings.insert(field.into(),if lines{Control::Lines(c)}else{Control::Text(c)});}
    fn shortcut(&mut self,parent:&gtk::Box,action:&str,title:&str){let b=gtk::Button::with_label("Unassigned");b.set_widget_name(&format!("shortcut_{action}"));row(parent,title,&b);if action=="synonyms"{b.set_sensitive(false);}let recording=self.recording.clone();b.connect_clicked(move |b|{recording.set(true);b.set_label("Press shortcut…");b.grab_focus();});let d=self.draft.clone();let recording=self.recording.clone();let a=action.to_owned();b.connect_key_press_event(move |b,e|{if !recording.get(){return glib::Propagation::Proceed;}let key=e.keyval();if key==gdk::keys::constants::Escape{recording.set(false);let v=d.borrow().value("shortcuts").and_then(|v|v.get(&a)).and_then(Value::as_str).unwrap_or("").to_owned();b.set_label(if v.is_empty(){"Unassigned"}else{&v});return glib::Propagation::Stop;}let value=if key==gdk::keys::constants::BackSpace{Ok(String::new())}else{event_chord(e)};if let Ok(value)=value{let mut map=d.borrow().value("shortcuts").and_then(Value::as_object).cloned().unwrap_or_default();map.insert(a.clone(),json!(value));let _=d.borrow_mut().set("shortcuts",Value::Object(map));recording.set(false);b.set_label(if value.is_empty(){"Unassigned"}else{&value});}glib::Propagation::Stop});let recording=self.recording.clone();let d=self.draft.clone();let a=action.to_owned();b.connect_focus_out_event(move |b,_|{if recording.replace(false){let v=d.borrow().value("shortcuts").and_then(|v|v.get(&a)).and_then(Value::as_str).unwrap_or("").to_owned();b.set_label(if v.is_empty(){"Unassigned"}else{&v});}glib::Propagation::Proceed});self.shortcut_buttons.insert(action.into(),b);}
    pub fn recording(&self)->bool{self.recording.get()}
    pub fn show_pane(&self,pane:&str){let pane=PANES.iter().find(|p|p.eq_ignore_ascii_case(pane)).unwrap_or(&PANES[0]);self.stack.set_visible_child_name(pane);self.window.show_all();self.window.present();let(period,group)=self.statistics_query.borrow().clone();let _=self.sender.send(UiAction::Statistics(period,group));let _=self.sender.send(UiAction::Command(Command::HistoryInfo));}
    pub fn reset(&self,c:&Config)->Result<()>{self.draft.borrow_mut().reset(c)?;self.write_controls();self.refresh_immediate(c);Ok(())}
    pub fn refresh(&self,c:&Config)->Result<()>{self.draft.borrow_mut().refresh(c)?;self.write_controls();self.refresh_immediate(c);Ok(())}
    fn write_controls(&self){self.loading.set(true);let d=self.draft.borrow();for(field,c)in &self.bindings{if let Some(v)=d.value(field){c.write(v);}}if !self.recording.get(){for(a,b)in &self.shortcut_buttons{let v=d.value("shortcuts").and_then(|v|v.get(a)).and_then(Value::as_str).unwrap_or("");b.set_label(if v.is_empty(){"Unassigned"}else{v});}}refresh_rules(&self.rules,&d);self.loading.set(false);}
    pub fn refresh_immediate(&self,c:&Config){
        self.loading.set(true);self.login.set_active(c.launch_at_login);self.history.set_active(c.record_history);self.loading.set(false);
        let status=if std::env::var_os("INKLOW_HOME").is_some(){"Login registration is disabled for isolated profiles.".into()}else{match system::launch_at_login_state(){Ok(true)=>"Launch at login is on.".into(),Ok(false)=>"Launch at login is off.".into(),Err(e)=>format!("Login registration unavailable: {e}")}};
        self.login_status.set_text(&status);
    }
    pub fn set_recovery(&self,recovery:bool){self.recovery.set_text(if recovery{"No system tray host is available. Keep Settings open to control BroTypist. Closing offers Quit; the tray reconnects automatically."}else{""});}
    pub fn status(&self,status:&Value,c:&Config,update:&Value){
        let dirty=self.draft.borrow().is_dirty();self.save.set_sensitive(dirty);self.revert.set_sensitive(dirty);self.dirty.set_text(if dirty{"Unsaved changes"}else{"All changes saved"});
        self.status_label.set_text(&format!("{}\n{}\nCurrent application: {}\nObservable website: {}\nSession completions: {} · Tab: {} · Recording: {}",text(status,"native_status"),text(status,"inline_message"),text(status,"current_app"),if text(status,"current_domain").is_empty(){"Unknown / unavailable".into()}else{text(status,"current_domain")},status["current_app_enabled"],status["current_tab_enabled"],status["current_recording"]));
        let field_state=if !c.global_enabled{"Disabled"}else if status["state"]=="paused"{"Paused"}else if status["inline_available"]==true{"Eligible field"}else{"Unsupported / inactive field"};
        let model=if c.backend=="gguf"{std::path::Path::new(&c.model_path).file_name().map(|s|s.to_string_lossy().into_owned()).unwrap_or_else(||"No model selected".into())}else{c.model.clone()};
        let message=if status["inline_available"]!=true&&matches!(status["state"].as_str(),Some("idle"|"ready")){text(status,"inline_message")}else{text(status,"message")};
        self.notice.set_text(&format!("{field_state} · {} · {model}\n{message}",text(status,"state")));
        self.model.set_text(&format!("Selected: {}\n{}",if c.backend=="gguf"{&c.model_path}else{&c.model},text(status,"message")));self.context.set_text(&text(status,"context_message"));
        let power=system::power_status();
        self.battery.set_text(if !power.available{"Battery state unavailable; unknown is not low battery."}else if !power.has_battery{"No battery detected."}else if power.on_battery{"On battery; configured limits apply."}else{"External power; normal preferences apply."});
        let state=text(update,"state");let busy=matches!(state.as_str(),"checking"|"downloading"|"installing");let installable=update["installable"]==true||update["install_kind"]=="linux_debian";
        self.updates.set_text(&format!("Installed {} · Update {}\n{}\n{}",text(update,"current_version"),text(update,"version"),text(update,"message"),text(update,"install_reason")));
        self.update_check.set_sensitive(!busy);self.update_download.set_sensitive(state=="available"&&installable);self.update_cancel.set_sensitive(matches!(state.as_str(),"checking"|"downloading"));self.update_install.set_sensitive(state=="ready"&&installable);self.update_install.set_label(if update["install_kind"]=="linux_debian"{"Open System Installer"}else{"Install & Relaunch"});
        let total=update["total"].as_f64().unwrap_or(0.);let received=update["received"].as_f64().unwrap_or(0.);
        self.progress.set_fraction(if total>0.{(received/total).clamp(0.,1.)}else{0.});self.progress.set_text(Some(&format!("{} / {} bytes",received as u64,total as u64)));
    }
    pub fn service_event(&self,event:&Value){match event["type"].as_str().unwrap_or(""){
        "statistics"=>{let result=&event["result"];let totals=&result["totals"];let today=&result["today"];let average=&result["daily_average"];self.stats.set_text(&format!("{} completions · {} words · {} characters\nEstimated time saved: {} seconds · {} active days\nToday: {} completions · {} words · {} characters\nDaily average: {} completions · {} words · {} characters",totals["completions"],totals["words"],totals["characters"],result["estimated_seconds"],result["active_days"],today["completions"],today["words"],today["characters"],average["completions"],average["words"],average["characters"]));for child in self.stats_rows.children(){self.stats_rows.remove(&child);}if let Some(rows)=result["rows"].as_array(){for r in rows{self.stats_rows.add(&label(&format!("{} · {} completions · {} words · {} characters",text(r,"label"),r["completions"],r["words"],r["characters"])));}}self.stats_rows.show_all();},
        "statistics_cleared"=>{self.stats.set_text("Statistics cleared. Encrypted history was kept.");for child in self.stats_rows.children(){self.stats_rows.remove(&child);}},
        "history_info"|"history_counts"=>{let r=&event["result"];self.history_info.set_text(&format_service_information(r));},
        "history_deleted"=>self.history_info.set_text(&format!("Forgot {} learned records. Statistics and encryption key were kept.",event["count"])),_=>{},}}
}
fn format_service_information(value:&Value)->String{let Some(object)=value.as_object()else{return "Encrypted history information unavailable.".into()};object.iter().filter(|(_,v)|!v.is_object()&&!v.is_array()).map(|(k,v)|format!("{}: {}",k.replace('_'," "),v.as_str().map(str::to_owned).unwrap_or_else(||v.to_string()))).collect::<Vec<_>>().join("\n")}
pub(super) fn event_chord(e:&gdk::EventKey)->Result<String>{
    let mut mods=Vec::new();let s=e.state();
    anyhow::ensure!(!s.contains(gdk::ModifierType::MOD5_MASK),"AltGr is text input, not a shortcut.");
    if s.contains(gdk::ModifierType::CONTROL_MASK){mods.push("Ctrl");}if s.contains(gdk::ModifierType::MOD1_MASK){mods.push("Alt");}
    if s.contains(gdk::ModifierType::SHIFT_MASK)||e.keyval()==gdk::keys::constants::ISO_Left_Tab{mods.push("Shift");}
    if s.intersects(gdk::ModifierType::SUPER_MASK|gdk::ModifierType::META_MASK){mods.push("Super");}
    let key=if e.keyval()==gdk::keys::constants::ISO_Left_Tab{"Tab".into()}else{e.keyval().name().unwrap_or_default().to_string()};
    crate::shortcuts::shortcut_from_event(&key,&mods,"linux",Some(u32::from(e.hardware_keycode())))
}
fn selected_rule(list:&gtk::ListBox)->Option<String>{let row=list.selected_row()?;Some(row.widget_name().into())}
fn refresh_rules(list:&gtk::ListBox,draft:&SettingsDraft){for child in list.children(){list.remove(&child);}if let Some(rules)=draft.value("app_rules").and_then(Value::as_object){for(key,rule)in rules{let row=gtk::ListBoxRow::new();row.set_widget_name(key);let state=|field:&str|match rule.get(field).and_then(Value::as_bool){Some(true)=>"Enabled",Some(false)=>"Disabled",None=>"Use default"};row.add(&label(&format!("{}\nCompletions: {} · Tab: {} · History: {}",key,state("enabled"),state("tab_enabled"),state("record_history"))));list.add(&row);}}list.show_all();}
fn stage_rule(mut all:serde_json::Map<String,Value>,old:Option<&str>,key:String,before:&Value,after:Value)->Result<serde_json::Map<String,Value>>{
    anyhow::ensure!(old==Some(key.as_str())||!all.contains_key(&key),"A rule already exists for that identifier. Edit the existing rule instead.");
    let empty=serde_json::Map::new();let current=old.and_then(|old|all.get(old)).and_then(Value::as_object).unwrap_or(&empty);
    let merged=crate::ui::settings::merge_mapping_edits(current,before.as_object().unwrap_or(&empty),after.as_object().context("Rule must be a mapping.")?);
    if let Some(old)=old{all.remove(old);}all.insert(key,Value::Object(merged));Ok(all)
}
fn rule_dialog(parent:&gtk::Window,key:&str,initial:&Value)->Option<(String,Value)>{
    let _modal=ModalGuard::enter();
    let dialog=gtk::Dialog::with_buttons(Some("Application or website settings"),Some(parent),gtk::DialogFlags::MODAL,&[("Cancel",gtk::ResponseType::Cancel),("Save rule draft",gtk::ResponseType::Accept)]);dialog.set_default_size(560,480);let body=dialog.content_area();let kind=gtk::ComboBoxText::new();kind.append(Some("app"),"Application identifier");if key.starts_with("domain:"){kind.append(Some("domain"),"Existing website hostname");}kind.set_active_id(Some(if key.starts_with("domain:"){"domain"}else{"app"}));row(&body,"Rule type",&kind);let identifier=gtk::Entry::new();identifier.set_text(key.split_once(':').map(|(_,id)|id).unwrap_or(""));row(&body,"Identifier",&identifier);
    let mut controls=Vec::new();for(field,title)in[("enabled","Completions"),("tab_enabled","Accept with Tab"),("record_history","Personalization history"),("improve_compatibility","Compatibility mode"),("complete_in_code_editors","Code editor")]{let c=gtk::ComboBoxText::new();for(id,title)in[("inherit","Use default"),("true","Enabled"),("false","Disabled")]{c.append(Some(id),title);}c.set_active_id(Some(match initial.get(field).and_then(Value::as_bool){Some(true)=>"true",Some(false)=>"false",None=>"inherit"}));row(&body,title,&c);controls.push((field,c));}
    body.pack_start(&label("Existing domain exclusions fail closed without a trusted URL. Enabled recording overrides require OS-backed encryption. Unknown rule fields are preserved."),false,false,0);body.pack_start(&label("Application-specific instructions"),false,false,0);let instructions=text_view(&body,120);instructions.buffer().unwrap().set_text(initial["custom_instructions"].as_str().unwrap_or(""));dialog.show_all();
    let result=loop{
        if dialog.run()!=gtk::ResponseType::Accept{break None;}
        let id=identifier.text().trim().to_owned();let kind=kind.active_id().unwrap_or_default();
        let id=if kind=="domain"{match crate::config::normalize_domain(&id){Ok(id)=>id,Err(e)=>{error(Some(parent),e);continue;}}}else{id};
        if id.is_empty(){error(Some(parent),"Enter an exact application identifier.");continue;}
        let b=instructions.buffer().unwrap();let text=b.text(&b.start_iter(),&b.end_iter(),false).unwrap_or_default();
        if text.chars().count()>8000{error(Some(parent),"Custom instructions must not exceed 8,000 characters.");continue;}
        let mut rule=initial.as_object().cloned().unwrap_or_default();
        for(field,c)in &controls{match c.active_id().as_deref(){Some("true")=>{rule.insert((*field).into(),json!(true));},Some("false")=>{rule.insert((*field).into(),json!(false));},_=>{rule.remove(*field);}}}
        if initial["record_history"]!=true&&rule.get("record_history")==Some(&json!(true))&&!confirm(Some(parent),"Enable recording for this scope?","Record eligible authored writing in OS-backed encrypted storage for this scope, even when global history is off? No plaintext fallback, upload or model-weight training."){continue;}
        rule.remove("custom_instructions");if !text.is_empty(){rule.insert("custom_instructions".into(),json!(text.as_str()));}
        break Some((format!("{kind}:{id}"),Value::Object(rule)));
    };dialog.close();result
}
#[cfg(test)] mod tests{use super::*;#[test]fn rule_inheritance_survives_three_way_refresh(){let mut c=Config::default();c.app_rules.insert("app:writer".into(),BTreeMap::from([("enabled".into(),json!(true)),("record_history".into(),json!(false))]));let mut d=SettingsDraft::new(&c).unwrap();d.set("app_rules",json!({"app:writer":{"enabled":false,"record_history":false}})).unwrap();let mut concurrent=c.clone();concurrent.app_rules.get_mut("app:writer").unwrap().insert("tab_enabled".into(),json!(false));d.refresh(&concurrent).unwrap();assert_eq!(d.apply_to(&concurrent).unwrap().app_rules["app:writer"]["tab_enabled"],false);assert!(d.is_dirty());}#[test]fn service_info_is_named_not_raw_json(){let value=json!({"encrypted":true,"records":42});let text=format_service_information(&value);assert!(text.contains("encrypted: true"));assert!(text.contains("records: 42"));assert!(!text.contains('{'));}}

#[cfg(test)]
mod native_widget_tests{
    use super::*;
    #[test]
    #[ignore="Requires a disposable native GTK display; run explicitly on the Linux UI lane, single-threaded."]
    fn owned_settings_controls_preserve_all_fields_and_draft_across_panes(){
        gtk::init().expect("Native Linux GTK display required");
        let (tx,rx)=mpsc::channel();let config=Config::default();
        let settings=SettingsWindow::new(&config,&tx).unwrap();
        let service_fields=["model_path","model_id","launch_at_login","record_history","shortcuts","app_rules"];
        for field in serde_json::to_value(&config).unwrap().as_object().unwrap().keys(){
            assert!(settings.bindings.contains_key(field)||service_fields.contains(&field.as_str()),"Missing functional field control: {field}");
        }
        for pane in PANES{settings.show_pane(pane);assert_eq!(settings.stack.visible_child_name().as_deref(),Some(pane));}
        settings.draft.borrow_mut().set("custom_instructions",json!("Use concise Portuguese.")).unwrap();
        settings.show_pane("Autocomplete");settings.show_pane("Languages");
        assert!(settings.draft.borrow().is_dirty());
        let mut concurrent=config.clone();concurrent.clipboard_context=true;concurrent.model_path="/owned/synthetic-selected.gguf".into();
        settings.refresh(&concurrent).unwrap();
        let candidate=settings.draft.borrow().apply_to(&concurrent).unwrap();
        assert_eq!(candidate.custom_instructions,"Use concise Portuguese.");assert!(candidate.clipboard_context);assert_eq!(candidate.model_path,concurrent.model_path);
        settings.reset(&concurrent).unwrap();assert!(!settings.draft.borrow().is_dirty());
        settings.window.close();
        while gtk::events_pending(){gtk::main_iteration_do(false);}
        assert!(rx.try_iter().any(|action|matches!(action,UiAction::CloseSettings)));
        // This test owns only native widgets. No controller, hook, profile or credentials were created.
        unsafe{settings.window.destroy();}
    }
}

#[cfg(feature="native-smoke")]
impl SettingsWindow{
    pub(super) fn smoke_control(&self,field:&str,value:Value)->Result<()>{
        self.bindings.get(field).context("Smoke requested an unknown native control.")?.write(&value);Ok(())
    }
    pub(super) fn smoke_save(&self){self.save.clicked();}
    pub(super) fn smoke_revert(&self){self.revert.clicked();}
    pub(super) fn smoke_pane(&self)->String{self.stack.visible_child_name().unwrap_or_default().into()}
    pub(super) fn smoke_inventory(&self)->bool{
        serde_json::to_value(Config::default()).ok().and_then(|v|v.as_object().cloned()).is_some_and(|fields|fields.keys().all(|f|self.bindings.contains_key(f)||["model_path","model_id","launch_at_login","record_history","shortcuts","app_rules"].contains(&f.as_str())))
    }
}

#[cfg(feature="native-smoke")]
impl SettingsWindow{
    pub(super) fn smoke_history(&self,enabled:bool){self.history.set_active(enabled);}
    pub(super) fn smoke_history_checked(&self)->bool{self.history.is_active()}
}

#[cfg(test)]
mod rule_editor_tests{
    use super::*;
    #[test] fn duplicate_rule_identifier_is_not_silently_overwritten(){
        let all=json!({"app:existing":{"enabled":false}});
        assert!(stage_rule(all.as_object().unwrap().clone(),None,"app:existing".into(),&json!({}),json!({"enabled":true})).is_err());
    }
    #[test] fn a_long_lived_rule_dialog_rebases_only_its_changed_fields(){
        let current=json!({"app:writer":{"enabled":true,"record_history":true},"app:other":{"enabled":false}});
        let staged=stage_rule(current.as_object().unwrap().clone(),Some("app:writer"),"app:writer".into(),&json!({"enabled":true,"record_history":false}),json!({"enabled":false,"record_history":false})).unwrap();
        assert_eq!(staged["app:writer"],json!({"enabled":false,"record_history":true}));
        assert_eq!(staged["app:other"],json!({"enabled":false}));
    }
}
